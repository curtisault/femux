//! Safe binding to libghostty-vt plus cell-diff extraction.
//!
//! A [`Terminal`] owns one libghostty-vt instance and a [`Grid`] mirror of its viewport.
//! [`Terminal::sync`] copies only the rows libghostty-vt reports dirty into the mirror and
//! stamps each copied row with a fresh generation. Consumers diff by generation, so any number
//! of readers can each catch up to the newest state without a per-reader backlog.

#[allow(unsafe_op_in_unsafe_fn)]
pub mod ffi;

use std::ffi::c_void;
use std::ptr;

use smol_str::SmolStr;

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
    /// Bumped whenever the grid is reshaped; a reader holding an older value needs a full frame.
    pub shape_generation: u64,
}

impl Grid {
    /// Plain text of the grid, rows joined by `\n`, trailing blanks trimmed. For debugging.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, row) in self.rows.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let mut line = String::new();
            for c in &row.cells {
                match c.width {
                    0 => {}
                    _ if c.text.is_empty() => line.push(' '),
                    _ => line.push_str(&c.text),
                }
            }
            out.push_str(line.trim_end());
        }
        out
    }
}

/// What a [`Terminal::sync`] call changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncStats {
    pub rows_copied: usize,
    pub full: bool,
}

/// Side effects reported by libghostty-vt callbacks, drained by the owner.
#[derive(Default)]
struct Effects {
    pty_out: Vec<u8>,
    bells: u32,
    title_changed: bool,
    pwd_changed: bool,
}

/// One libghostty-vt terminal with its render state and viewport mirror.
pub struct Terminal {
    raw: ffi::GhosttyTerminal,
    render: ffi::GhosttyRenderState,
    rows_iter: ffi::GhosttyRenderStateRowIterator,
    cells: ffi::GhosttyRenderStateRowCells,
    effects: Box<Effects>,
    grid: Grid,
    generation: u64,
    utf8: Vec<u8>,
}

// SAFETY: libghostty-vt handles have no thread affinity; all access goes through `&mut self`.
unsafe impl Send for Terminal {}

unsafe extern "C" fn on_write_pty(
    _t: ffi::GhosttyTerminal,
    ud: *mut c_void,
    data: *const u8,
    len: usize,
) {
    let fx = unsafe { &mut *(ud as *mut Effects) };
    if len > 0 {
        fx.pty_out
            .extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    }
}

unsafe extern "C" fn on_bell(_t: ffi::GhosttyTerminal, ud: *mut c_void) {
    unsafe { (*(ud as *mut Effects)).bells += 1 };
}

unsafe extern "C" fn on_title(_t: ffi::GhosttyTerminal, ud: *mut c_void) {
    unsafe { (*(ud as *mut Effects)).title_changed = true };
}

unsafe extern "C" fn on_pwd(_t: ffi::GhosttyTerminal, ud: *mut c_void) {
    unsafe { (*(ud as *mut Effects)).pwd_changed = true };
}

impl Terminal {
    /// Creates a terminal of `cols`×`rows` keeping at most `scrollback_lines` of history.
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Result<Self, Error> {
        let mut t = Terminal {
            raw: ptr::null_mut(),
            render: ptr::null_mut(),
            rows_iter: ptr::null_mut(),
            cells: ptr::null_mut(),
            effects: Box::default(),
            grid: Grid::default(),
            generation: 0,
            utf8: Vec::with_capacity(32),
        };
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

            let ud = &mut *t.effects as *mut Effects as *const c_void;
            t.set_opt(
                ffi::GhosttyTerminalOption::GHOSTTY_TERMINAL_OPT_USERDATA,
                ud,
            )?;
            use ffi::GhosttyTerminalOption as O;
            // Callback options take the function pointer itself as the value.
            let callbacks: [(O, *const c_void); 4] = [
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
            ];
            for (opt, f) in callbacks {
                t.set_opt(opt, f)?;
            }
            let lines: usize = scrollback_lines;
            t.set_opt(
                O::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES,
                &lines as *const usize as *const c_void,
            )?;
        }
        Ok(t)
    }

    unsafe fn set_opt(
        &mut self,
        opt: ffi::GhosttyTerminalOption,
        value: *const c_void,
    ) -> Result<(), Error> {
        check(unsafe { ffi::ghostty_terminal_set(self.raw, opt, value) })
    }

    /// Feeds bytes read from the pane's PTY.
    pub fn write(&mut self, bytes: &[u8]) {
        // SAFETY: `raw` is a live terminal and `bytes` is a valid slice.
        unsafe { ffi::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) }
    }

    /// Resizes the terminal. Cell pixel sizes are zero until a client reports them.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        // SAFETY: `raw` is a live terminal.
        check(unsafe { ffi::ghostty_terminal_resize(self.raw, cols, rows, 0, 0) })
    }

    /// Bytes the terminal wants written back to the PTY (query responses, mode reports).
    pub fn take_pty_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.effects.pty_out)
    }

    /// Number of BEL characters seen since the last call.
    pub fn take_bells(&mut self) -> u32 {
        std::mem::take(&mut self.effects.bells)
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
            let full = dirty == ffi::GhosttyRenderStateDirty::GHOSTTY_RENDER_STATE_DIRTY_FULL
                || cols != self.grid.cols
                || rows as usize != self.grid.rows.len();
            if cols != self.grid.cols || rows as usize != self.grid.rows.len() {
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
            self.grid.cursor = Cursor {
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
            while ffi::ghostty_render_state_row_cells_next(self.cells) && x < row.cells.len() {
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
                let cell = &mut row.cells[x];
                decode_cell(raw, glen, self.cells, &mut self.utf8, cell)?;
                x += 1;
            }
            for cell in &mut row.cells[x..] {
                *cell = Cell::default();
            }
        }
        Ok(())
    }
}

/// Fills `out` from a raw libghostty-vt cell at the current cells-iterator position.
unsafe fn decode_cell(
    raw: ffi::GhosttyCell,
    glen: u32,
    cells: ffi::GhosttyRenderStateRowCells,
    utf8: &mut Vec<u8>,
    out: &mut Cell,
) -> Result<(), Error> {
    use ffi::GhosttyCellData as CD;
    unsafe {
        let mut wide = ffi::GhosttyCellWide::GHOSTTY_CELL_WIDE_NARROW;
        let mut tag = ffi::GhosttyCellContentTag::GHOSTTY_CELL_CONTENT_CODEPOINT;
        let mut styled = false;
        let mut keys = [
            CD::GHOSTTY_CELL_DATA_WIDE,
            CD::GHOSTTY_CELL_DATA_CONTENT_TAG,
            CD::GHOSTTY_CELL_DATA_HAS_STYLING,
        ];
        let mut vals = [
            &mut wide as *mut _ as *mut c_void,
            &mut tag as *mut _ as *mut c_void,
            &mut styled as *mut _ as *mut c_void,
        ];
        check(ffi::ghostty_cell_get_multi(
            raw,
            keys.len(),
            keys.as_mut_ptr(),
            vals.as_mut_ptr(),
            ptr::null_mut(),
        ))?;

        out.width = match wide {
            ffi::GhosttyCellWide::GHOSTTY_CELL_WIDE_WIDE => 2,
            ffi::GhosttyCellWide::GHOSTTY_CELL_WIDE_SPACER_TAIL => 0,
            _ => 1,
        };

        out.text = if glen == 0 {
            SmolStr::default()
        } else if glen == 1 {
            let mut cp: u32 = 0;
            check(ffi::ghostty_cell_get(
                raw,
                CD::GHOSTTY_CELL_DATA_CODEPOINT,
                &mut cp as *mut _ as *mut c_void,
            ))?;
            let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
            let mut b = [0u8; 4];
            SmolStr::new(ch.encode_utf8(&mut b))
        } else {
            utf8.clear();
            utf8.resize(utf8.capacity().max(32), 0);
            loop {
                let mut buf = ffi::GhosttyBuffer {
                    ptr: utf8.as_mut_ptr(),
                    cap: utf8.len(),
                    len: 0,
                };
                let r = ffi::ghostty_render_state_row_cells_get(
                    cells,
                    ffi::GhosttyRenderStateRowCellsData::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
                    &mut buf as *mut _ as *mut c_void,
                );
                if r == ffi::GhosttyResult::GHOSTTY_OUT_OF_SPACE {
                    utf8.resize(buf.len, 0);
                    continue;
                }
                check(r)?;
                break SmolStr::new(String::from_utf8_lossy(&utf8[..buf.len]));
            }
        };

        out.fg = Color::Default;
        out.bg = Color::Default;
        out.ul = None;
        out.attrs = 0;
        if styled {
            let mut st: ffi::GhosttyStyle = std::mem::zeroed();
            st.size = std::mem::size_of::<ffi::GhosttyStyle>();
            check(ffi::ghostty_render_state_row_cells_get(
                cells,
                ffi::GhosttyRenderStateRowCellsData::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                &mut st as *mut _ as *mut c_void,
            ))?;
            out.fg = style_color(&st.fg_color);
            out.bg = style_color(&st.bg_color);
            out.ul = match style_color(&st.underline_color) {
                Color::Default => None,
                c => Some(c),
            };
            let mut a = 0u16;
            for (on, bit) in [
                (st.bold, attrs::BOLD),
                (st.italic, attrs::ITALIC),
                (st.faint, attrs::FAINT),
                (st.blink, attrs::BLINK),
                (st.inverse, attrs::INVERSE),
                (st.invisible, attrs::INVISIBLE),
                (st.strikethrough, attrs::STRIKETHROUGH),
                (st.overline, attrs::OVERLINE),
            ] {
                if on {
                    a |= bit;
                }
            }
            a |= ((st.underline.clamp(0, 7) as u16) << attrs::UNDERLINE_SHIFT)
                & attrs::UNDERLINE_MASK;
            out.attrs = a;
        }
        // Background-only cells (erased with a colored background) carry the color in the cell.
        if tag == ffi::GhosttyCellContentTag::GHOSTTY_CELL_CONTENT_BG_COLOR_PALETTE {
            let mut i: u8 = 0;
            check(ffi::ghostty_cell_get(
                raw,
                CD::GHOSTTY_CELL_DATA_COLOR_PALETTE,
                &mut i as *mut _ as *mut c_void,
            ))?;
            out.bg = Color::Indexed(i);
        } else if tag == ffi::GhosttyCellContentTag::GHOSTTY_CELL_CONTENT_BG_COLOR_RGB {
            let mut c = ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 };
            check(ffi::ghostty_cell_get(
                raw,
                CD::GHOSTTY_CELL_DATA_COLOR_RGB,
                &mut c as *mut _ as *mut c_void,
            ))?;
            out.bg = Color::Rgb(c.r, c.g, c.b);
        }
    }
    Ok(())
}

fn style_color(c: &ffi::GhosttyStyleColor) -> Color {
    // SAFETY: the tag says which union field is active.
    unsafe {
        match c.tag {
            ffi::GhosttyStyleColorTag::GHOSTTY_STYLE_COLOR_PALETTE => {
                Color::Indexed(c.value.palette)
            }
            ffi::GhosttyStyleColorTag::GHOSTTY_STYLE_COLOR_RGB => {
                Color::Rgb(c.value.rgb.r, c.value.rgb.g, c.value.rgb.b)
            }
            _ => Color::Default,
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: each handle is either null (accepted by the free functions) or live.
        unsafe {
            if !self.cells.is_null() {
                ffi::ghostty_render_state_row_cells_free(self.cells);
            }
            if !self.rows_iter.is_null() {
                ffi::ghostty_render_state_row_iterator_free(self.rows_iter);
            }
            if !self.render.is_null() {
                ffi::ghostty_render_state_free(self.render);
            }
            if !self.raw.is_null() {
                ffi::ghostty_terminal_free(self.raw);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn answers_device_attributes() {
        let mut t = Terminal::new(10, 5, 100).unwrap();
        t.write(b"\x1b[c");
        assert!(!t.take_pty_output().is_empty());
    }
}
