//! Safe binding to libghostty-vt plus cell-diff extraction.
//!
//! A [`Terminal`] owns one libghostty-vt instance and a [`Grid`] mirror of its viewport.
//! [`Terminal::sync`] copies only the rows libghostty-vt reports dirty into the mirror and
//! stamps each copied row with a fresh generation. Consumers diff by generation, so any number
//! of readers can each catch up to the newest state without a per-reader backlog.

#[allow(unsafe_op_in_unsafe_fn)]
pub mod ffi;

mod cell;
mod history;
mod input;
mod query;

use std::ffi::c_void;
use std::ptr;

use smol_str::SmolStr;

pub use input::{Key, KeyAction, Mods, MouseAction, MouseButton, MouseInput};
pub use query::HostCaps;

/// Error returned by a libghostty-vt call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "libghostty-vt error {}", self.0)
    }
}

impl std::error::Error for Error {}

fn check(r: ffi::GhosttyResult) -> Result<(), Error> {
    if r == ffi::GhosttyResult::GHOSTTY_SUCCESS {
        Ok(())
    } else {
        Err(Error(r.0))
    }
}

/// A cell color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Bit flags in [`Cell::attrs`]. Bits 8–10 hold the underline style (0 = none).
pub mod attrs {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    pub const FAINT: u16 = 1 << 2;
    pub const BLINK: u16 = 1 << 3;
    pub const INVERSE: u16 = 1 << 4;
    pub const INVISIBLE: u16 = 1 << 5;
    pub const STRIKETHROUGH: u16 = 1 << 6;
    pub const OVERLINE: u16 = 1 << 7;
    pub const UNDERLINE_SHIFT: u16 = 8;
    pub const UNDERLINE_MASK: u16 = 0b111 << UNDERLINE_SHIFT;
}

/// One grid cell. `text` is empty for a blank cell. `width` is 1, 2 for a wide character,
/// or 0 for the spacer column that follows a wide character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: SmolStr,
    pub fg: Color,
    pub bg: Color,
    pub ul: Option<Color>,
    pub attrs: u16,
    pub width: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            text: SmolStr::default(),
            fg: Color::Default,
            bg: Color::Default,
            ul: None,
            attrs: 0,
            width: 1,
        }
    }
}

/// Plain text of a row of cells, trailing blanks trimmed.
pub fn line_text(cells: &[Cell]) -> String {
    let mut line = String::new();
    for c in cells {
        match c.width {
            0 => {}
            _ if c.text.is_empty() => line.push(' '),
            _ => line.push_str(&c.text),
        }
    }
    line.truncate(line.trim_end().len());
    line
}

/// One viewport row and the generation at which it last changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Bar,
    Underline,
    BlockHollow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub shape: CursorShape,
}

/// Mirror of the visible viewport.
#[derive(Debug, Clone, Default)]
pub struct Grid {
    pub cols: u16,
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    /// Generation of the last cursor change.
    pub cursor_generation: u64,
    /// Bumped whenever the grid is reshaped; a reader holding an older value needs a full frame.
    pub shape_generation: u64,
}

impl Grid {
    /// Plain text of the grid, rows joined by `\n`, trailing blanks trimmed. For debugging.
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|r| line_text(&r.cells))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What a [`Terminal::sync`] call changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncStats {
    pub rows_copied: usize,
    pub full: bool,
}

/// Things the terminal reported since they were last taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub bells: u32,
    pub title: Option<String>,
    pub pwd: Option<String>,
    /// Texts written to the system clipboard by the pane program (OSC 52 and friends).
    pub clipboard: Vec<String>,
}

/// State shared with libghostty-vt callbacks through the terminal's userdata pointer.
#[derive(Default)]
struct Effects {
    pty_out: Vec<u8>,
    bells: u32,
    title_changed: bool,
    pwd_changed: bool,
    clipboard: Vec<String>,
    host: HostCaps,
    xtversion: String,
    cell_px: (u32, u32),
    size: (u16, u16),
}

/// One libghostty-vt terminal with its render state and viewport mirror.
pub struct Terminal {
    raw: ffi::GhosttyTerminal,
    render: ffi::GhosttyRenderState,
    rows_iter: ffi::GhosttyRenderStateRowIterator,
    cells: ffi::GhosttyRenderStateRowCells,
    keys: ffi::GhosttyKeyEncoder,
    key_event: ffi::GhosttyKeyEvent,
    mouse: ffi::GhosttyMouseEncoder,
    mouse_event: ffi::GhosttyMouseEvent,
    effects: Box<Effects>,
    grid: Grid,
    generation: u64,
    utf8: Vec<u8>,
    host_set: bool,
}

// SAFETY: libghostty-vt handles have no thread affinity; all access goes through `&mut self`.
unsafe impl Send for Terminal {}

fn effects<'a>(ud: *mut c_void) -> &'a mut Effects {
    // SAFETY: userdata is always the terminal's boxed Effects, which outlives the terminal.
    unsafe { &mut *(ud as *mut Effects) }
}

unsafe extern "C" fn on_write_pty(
    _t: ffi::GhosttyTerminal,
    ud: *mut c_void,
    data: *const u8,
    len: usize,
) {
    if len > 0 {
        effects(ud)
            .pty_out
            .extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    }
}

unsafe extern "C" fn on_bell(_t: ffi::GhosttyTerminal, ud: *mut c_void) {
    effects(ud).bells += 1;
}

unsafe extern "C" fn on_title(_t: ffi::GhosttyTerminal, ud: *mut c_void) {
    effects(ud).title_changed = true;
}

unsafe extern "C" fn on_pwd(_t: ffi::GhosttyTerminal, ud: *mut c_void) {
    effects(ud).pwd_changed = true;
}

/// Clipboard writes from the pane program (OSC 52, OSC 5522, OSC 1337 Copy) are accepted and
/// queued for the daemon (REQ-SEC-002: writes are allowed from any pane). The text/plain
/// representation is used, else the first one.
unsafe extern "C" fn on_clipboard_write(
    _t: ffi::GhosttyTerminal,
    ud: *mut c_void,
    write: *const ffi::GhosttyClipboardWrite,
) {
    // SAFETY: `write` and everything it borrows are valid for the duration of this callback.
    unsafe {
        let w = &*write;
        let contents = if w.contents.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(w.contents, w.contents_len)
        };
        let text_of = |s: &ffi::GhosttyString| {
            if s.ptr.is_null() {
                String::new()
            } else {
                String::from_utf8_lossy(std::slice::from_raw_parts(s.ptr, s.len)).into_owned()
            }
        };
        let plain = contents
            .iter()
            .find(|c| text_of(&c.mime).starts_with("text/plain"))
            .or(contents.first());
        if let Some(c) = plain {
            effects(ud).clipboard.push(text_of(&c.data));
        }
        if let Some(reply) = w.reply {
            let r = ffi::GhosttyClipboardWriteReply {
                size: std::mem::size_of::<ffi::GhosttyClipboardWriteReply>(),
                result: ffi::GhosttyClipboardWriteResult::GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS,
                remember: false,
            };
            reply(write, &r);
        }
    }
}

/// Virtual cell size used until a client reports a real one; keeps pixel reports sane.
const DEFAULT_CELL_PX: (u32, u32) = (10, 20);

impl Terminal {
    /// Creates a terminal of `cols`×`rows` keeping at most `scrollback_lines` of history.
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Result<Self, Error> {
        let mut t = Terminal {
            raw: ptr::null_mut(),
            render: ptr::null_mut(),
            rows_iter: ptr::null_mut(),
            cells: ptr::null_mut(),
            keys: ptr::null_mut(),
            key_event: ptr::null_mut(),
            mouse: ptr::null_mut(),
            mouse_event: ptr::null_mut(),
            effects: Box::default(),
            grid: Grid::default(),
            generation: 0,
            utf8: Vec::with_capacity(32),
            host_set: false,
        };
        t.effects.cell_px = DEFAULT_CELL_PX;
        t.effects.size = (cols, rows);
        // SAFETY: out-pointers are valid; on error `Drop` frees whatever was created.
        unsafe {
            check(ffi::ghostty_terminal_new(
                ptr::null(),
                &mut t.raw,
                cols,
                rows,
            ))?;
            check(ffi::ghostty_render_state_new(ptr::null(), &mut t.render))?;
            check(ffi::ghostty_render_state_row_iterator_new(
                ptr::null(),
                &mut t.rows_iter,
            ))?;
            check(ffi::ghostty_render_state_row_cells_new(
                ptr::null(),
                &mut t.cells,
            ))?;
            check(ffi::ghostty_key_encoder_new(ptr::null(), &mut t.keys))?;
            check(ffi::ghostty_key_event_new(ptr::null(), &mut t.key_event))?;
            check(ffi::ghostty_mouse_encoder_new(ptr::null(), &mut t.mouse))?;
            check(ffi::ghostty_mouse_event_new(
                ptr::null(),
                &mut t.mouse_event,
            ))?;
            check(ffi::ghostty_terminal_resize(
                t.raw,
                cols,
                rows,
                DEFAULT_CELL_PX.0,
                DEFAULT_CELL_PX.1,
            ))?;

            use ffi::GhosttyTerminalOption as O;
            let ud = &mut *t.effects as *mut Effects as *const c_void;
            t.set_opt(O::GHOSTTY_TERMINAL_OPT_USERDATA, ud)?;
            // Callback options take the function pointer itself as the value.
            let callbacks: [(O, *const c_void); 5] = [
                (
                    O::GHOSTTY_TERMINAL_OPT_WRITE_PTY,
                    on_write_pty as *const c_void,
                ),
                (O::GHOSTTY_TERMINAL_OPT_BELL, on_bell as *const c_void),
                (
                    O::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED,
                    on_title as *const c_void,
                ),
                (O::GHOSTTY_TERMINAL_OPT_PWD_CHANGED, on_pwd as *const c_void),
                (
                    O::GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE,
                    on_clipboard_write as *const c_void,
                ),
            ];
            for (opt, f) in callbacks {
                t.set_opt(opt, f)?;
            }
            let lines: usize = scrollback_lines;
            t.set_opt(
                O::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES,
                &lines as *const usize as *const c_void,
            )?;
            // Scrollback is bounded by lines only (REQ-VT-003); NULL removes the byte limit.
            t.set_opt(O::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, ptr::null())?;
            t.install_query_callbacks()?;
        }
        t.set_host(&HostCaps::default())?;
        Ok(t)
    }

    unsafe fn set_opt(
        &mut self,
        opt: ffi::GhosttyTerminalOption,
        value: *const c_void,
    ) -> Result<(), Error> {
        check(unsafe { ffi::ghostty_terminal_set(self.raw, opt, value) })
    }

    unsafe fn get<T>(&self, key: ffi::GhosttyTerminalData, out: *mut T) -> Result<(), Error> {
        check(unsafe { ffi::ghostty_terminal_get(self.raw, key, out as *mut c_void) })
    }

    /// Feeds bytes read from the pane's PTY.
    pub fn write(&mut self, bytes: &[u8]) {
        // SAFETY: `raw` is a live terminal and `bytes` is a valid slice.
        unsafe { ffi::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) }
    }

    /// Resizes the terminal, keeping the current cell pixel size.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        let (w, h) = self.effects.cell_px;
        self.effects.size = (cols, rows);
        // SAFETY: `raw` is a live terminal.
        check(unsafe { ffi::ghostty_terminal_resize(self.raw, cols, rows, w, h) })
    }

    /// Current size in cells.
    pub fn size(&self) -> (u16, u16) {
        self.effects.size
    }

    /// Bytes the terminal wants written back to the PTY (query responses, mode reports).
    pub fn take_pty_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.effects.pty_out)
    }

    /// Bells, title, cwd and clipboard changes since the last call.
    pub fn take_changes(&mut self) -> Changes {
        let fx = &mut self.effects;
        let (title, pwd) = (
            std::mem::take(&mut fx.title_changed),
            std::mem::take(&mut fx.pwd_changed),
        );
        let bells = std::mem::take(&mut fx.bells);
        let clipboard = std::mem::take(&mut fx.clipboard);
        Changes {
            bells,
            title: title.then(|| self.title()),
            pwd: pwd.then(|| self.pwd()),
            clipboard,
        }
    }

    fn string(&self, key: ffi::GhosttyTerminalData) -> String {
        let mut s = ffi::GhosttyString {
            ptr: ptr::null(),
            len: 0,
        };
        // SAFETY: the string is borrowed from the terminal and copied before any mutation.
        unsafe {
            if self.get(key, &mut s).is_err() || s.ptr.is_null() {
                return String::new();
            }
            String::from_utf8_lossy(std::slice::from_raw_parts(s.ptr, s.len)).into_owned()
        }
    }

    /// Window title set by OSC 0/2 (REQ-VT-006).
    pub fn title(&self) -> String {
        self.string(ffi::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_TITLE)
    }

    /// Working directory reported by OSC 7 (REQ-VT-006), as sent (usually a file:// URL).
    pub fn pwd(&self) -> String {
        self.string(ffi::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_PWD)
    }

    /// Whether DEC private mode `mode` is set.
    pub fn dec_mode(&self, mode: u16) -> bool {
        let mut m = ffi::GhosttyTerminalModeConfig {
            mode: mode & 0x7fff,
            value: false,
        };
        // SAFETY: m is a valid in/out struct.
        unsafe {
            self.get(ffi::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_MODE, &mut m)
                .is_ok()
                && m.value
        }
    }

    /// The viewport mirror as of the last [`Terminal::sync`].
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Copies dirty rows from libghostty-vt into the grid mirror.
    pub fn sync(&mut self) -> Result<SyncStats, Error> {
        use ffi::GhosttyRenderStateData as D;
        let mut stats = SyncStats::default();
        // SAFETY: all handles are live; out-pointers point at correctly typed locals.
        unsafe {
            check(ffi::ghostty_render_state_update(self.render, self.raw))?;
            let mut dirty = ffi::GhosttyRenderStateDirty::GHOSTTY_RENDER_STATE_DIRTY_FALSE;
            self.rs_get(D::GHOSTTY_RENDER_STATE_DATA_DIRTY, &mut dirty)?;
            if dirty == ffi::GhosttyRenderStateDirty::GHOSTTY_RENDER_STATE_DIRTY_FALSE {
                return Ok(stats);
            }
            let (mut cols, mut rows) = (0u16, 0u16);
            self.rs_get(D::GHOSTTY_RENDER_STATE_DATA_COLS, &mut cols)?;
            self.rs_get(D::GHOSTTY_RENDER_STATE_DATA_ROWS, &mut rows)?;
            let reshaped = cols != self.grid.cols || rows as usize != self.grid.rows.len();
            let full =
                dirty == ffi::GhosttyRenderStateDirty::GHOSTTY_RENDER_STATE_DIRTY_FULL || reshaped;
            if reshaped {
                self.grid.cols = cols;
                self.grid.rows = vec![
                    Row {
                        cells: vec![Cell::default(); cols as usize],
                        generation: 0
                    };
                    rows as usize
                ];
                self.grid.shape_generation += 1;
            }
            stats.full = full;
            self.generation += 1;
            let generation = self.generation;

            let iter: *mut ffi::GhosttyRenderStateRowIterator = &mut self.rows_iter;
            self.rs_get(D::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, iter)?;
            let (mut y, mut next_y) = (0u16, 0u16);
            loop {
                let more = if full {
                    y = next_y;
                    next_y += 1;
                    ffi::ghostty_render_state_row_iterator_next(self.rows_iter)
                } else {
                    ffi::ghostty_render_state_row_iterator_next_dirty(self.rows_iter, &mut y)
                };
                if !more {
                    break;
                }
                if y as usize >= self.grid.rows.len() {
                    continue;
                }
                self.copy_row(y as usize, generation)?;
                stats.rows_copied += 1;
            }

            let mut cur = ffi::GhosttyRenderStateCursor {
                size: std::mem::size_of::<ffi::GhosttyRenderStateCursor>(),
                ..std::mem::zeroed()
            };
            self.rs_get(D::GHOSTTY_RENDER_STATE_DATA_CURSOR, &mut cur)?;
            use ffi::GhosttyRenderStateCursorVisualStyle as S;
            let cursor = Cursor {
                x: cur.viewport_x,
                y: cur.viewport_y,
                visible: cur.visible && cur.viewport_has_value,
                shape: match cur.visual_style {
                    S::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BAR => CursorShape::Bar,
                    S::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_UNDERLINE => CursorShape::Underline,
                    S::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK_HOLLOW => {
                        CursorShape::BlockHollow
                    }
                    _ => CursorShape::Block,
                },
            };
            if cursor != self.grid.cursor || full {
                self.grid.cursor = cursor;
                self.grid.cursor_generation = generation;
            }
            check(ffi::ghostty_render_state_clean(self.render))?;
        }
        Ok(stats)
    }

    unsafe fn rs_get<T>(&self, key: ffi::GhosttyRenderStateData, out: *mut T) -> Result<(), Error> {
        check(unsafe { ffi::ghostty_render_state_get(self.render, key, out as *mut c_void) })
    }

    /// Copies the iterator's current row into `self.grid.rows[y]`.
    unsafe fn copy_row(&mut self, y: usize, generation: u64) -> Result<(), Error> {
        use ffi::GhosttyRenderStateRowCellsData as C;
        unsafe {
            check(ffi::ghostty_render_state_row_get(
                self.rows_iter,
                ffi::GhosttyRenderStateRowData::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                &mut self.cells as *mut _ as *mut c_void,
            ))?;
            let row = &mut self.grid.rows[y];
            row.generation = generation;
            let mut x = 0usize;
            while x < row.cells.len() && ffi::ghostty_render_state_row_cells_next(self.cells) {
                let mut raw: ffi::GhosttyCell = 0;
                let mut glen: u32 = 0;
                let mut keys = [
                    C::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                    C::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN,
                ];
                let mut vals = [
                    &mut raw as *mut _ as *mut c_void,
                    &mut glen as *mut _ as *mut c_void,
                ];
                check(ffi::ghostty_render_state_row_cells_get_multi(
                    self.cells,
                    keys.len(),
                    keys.as_mut_ptr(),
                    vals.as_mut_ptr(),
                    ptr::null_mut(),
                ))?;
                let out = &mut row.cells[x];
                let styled = cell::decode_raw(raw, out)?;
                let cells = self.cells;
                out.text = cell::text_from(raw, glen, &mut self.utf8, |buf| {
                    ffi::ghostty_render_state_row_cells_get(
                        cells,
                        C::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
                        buf as *mut _ as *mut c_void,
                    )
                })?;
                if styled {
                    let mut st = cell::new_style();
                    check(ffi::ghostty_render_state_row_cells_get(
                        cells,
                        C::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                        &mut st as *mut _ as *mut c_void,
                    ))?;
                    cell::apply_style(&st, out);
                }
                cell::apply_bg_content(raw, out)?;
                x += 1;
            }
            for cell in &mut row.cells[x..] {
                *cell = Cell::default();
            }
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: each handle is either null (accepted by the free functions) or live.
        unsafe {
            ffi::ghostty_mouse_event_free(self.mouse_event);
            ffi::ghostty_mouse_encoder_free(self.mouse);
            ffi::ghostty_key_event_free(self.key_event);
            ffi::ghostty_key_encoder_free(self.keys);
            ffi::ghostty_render_state_row_cells_free(self.cells);
            ffi::ghostty_render_state_row_iterator_free(self.rows_iter);
            ffi::ghostty_render_state_free(self.render);
            ffi::ghostty_terminal_free(self.raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_writes_are_reported() {
        let mut t = Terminal::new(20, 3, 100).unwrap();
        // OSC 52 with base64 "hello"; a read request ("?") produces nothing.
        t.write(b"\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07");
        let c = t.take_changes();
        assert_eq!(c.clipboard, ["hello"]);
        assert!(t.take_changes().clipboard.is_empty());
        assert!(t.take_pty_output().is_empty(), "reads are not answered");
    }

    #[test]
    fn writes_text_and_styles() {
        let mut t = Terminal::new(20, 3, 100).unwrap();
        t.write(b"hi \x1b[1;31mred\x1b[0m\r\n\xe4\xb8\xad!");
        let s = t.sync().unwrap();
        assert!(s.full);
        let g = t.grid();
        assert_eq!(g.text(), "hi red\n中!\n");
        let r = &g.rows[0].cells[3];
        assert_eq!(r.text, "r");
        assert_eq!(r.fg, Color::Indexed(1));
        assert_eq!(r.attrs & attrs::BOLD, attrs::BOLD);
        assert_eq!(g.rows[1].cells[0].width, 2);
        assert_eq!(g.rows[1].cells[1].width, 0);
        assert_eq!((g.cursor.x, g.cursor.y), (3, 1));
    }

    #[test]
    fn only_dirty_rows_are_copied() {
        let mut t = Terminal::new(10, 5, 100).unwrap();
        t.write(b"a\r\nb\r\nc");
        t.sync().unwrap();
        let before: Vec<u64> = t.grid().rows.iter().map(|r| r.generation).collect();
        t.write(b"\x1b[1;1Hz");
        let s = t.sync().unwrap();
        assert!(!s.full);
        let after: Vec<u64> = t.grid().rows.iter().map(|r| r.generation).collect();
        assert!(after[0] > before[0]);
        // Row 2 held the cursor before the move, so libghostty-vt reports it dirty too.
        assert_eq!(after[1], before[1]);
        assert_eq!(after[3..], before[3..]);
        assert_eq!(t.grid().rows[0].cells[0].text, "z");
    }

    #[test]
    fn captures_title_and_cwd() {
        let mut t = Terminal::new(10, 5, 100).unwrap();
        t.write(b"\x1b]2;hello\x07\x1b]7;file://host/tmp/x\x07\x07");
        let c = t.take_changes();
        assert_eq!(c.title.as_deref(), Some("hello"));
        assert_eq!(c.pwd.as_deref(), Some("file://host/tmp/x"));
        assert_eq!(c.bells, 1);
        assert_eq!(t.take_changes(), Changes::default());
    }

    #[test]
    fn resize_reshapes_grid() {
        let mut t = Terminal::new(10, 5, 100).unwrap();
        t.sync().unwrap();
        let shape = t.grid().shape_generation;
        t.resize(30, 7).unwrap();
        assert!(t.sync().unwrap().full);
        assert_eq!((t.grid().cols, t.grid().rows.len()), (30, 7));
        assert!(t.grid().shape_generation > shape);
    }
}
