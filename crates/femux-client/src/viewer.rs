//! The bare viewer's logic (ADR-014): one session, its focused pane, typing, `prefix d`.
//! Terminal I/O lives in [`crate::run`]; this part is pure so it can be tested headlessly.

use std::collections::HashMap;

use femux_proto::msg as m;
use femux_proto::{Event, Request};
use termwiz::cell::{Blink, CellAttributes, Intensity, Underline};
use termwiz::color::{ColorAttribute, SrgbaTuple};
use termwiz::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use termwiz::surface::{Change, CursorVisibility, Position, Surface};

/// What the viewer wants done after an input.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(Request),
    Detach,
}

/// One pane's cells as received in frames.
#[derive(Debug, Clone, Default)]
pub struct PaneScreen {
    pub rect: m::Rect,
    pub rows: Vec<Vec<Option<m::Cell>>>,
    pub cursor: m::Cursor,
}

impl PaneScreen {
    pub fn apply(&mut self, pf: &m::PaneFrame) {
        self.rect = pf.rect.clone();
        self.cursor = pf.cursor.clone();
        let (w, h) = (pf.rect.w as usize, pf.rect.h as usize);
        self.rows.resize_with(h, Vec::new);
        for r in &mut self.rows {
            r.resize(w, None);
        }
        for run in &pf.cells {
            let Some(row) = self.rows.get_mut(run.y as usize) else {
                continue;
            };
            let mut x = run.x as usize;
            for c in &run.run {
                if x < row.len() {
                    row[x] = Some(c.clone());
                }
                if c.width == 2 && x + 1 < row.len() {
                    row[x + 1] = None;
                }
                x += c.width.max(1) as usize;
            }
        }
    }
}

pub struct Viewer {
    pub view_id: String,
    pub tab: String,
    /// Focused pane per tab.
    pub focus: HashMap<String, String>,
    pub panes: HashMap<String, PaneScreen>,
    prefix_armed: bool,
    pub dirty: bool,
}

/// The default prefix (ADR-008).
fn is_prefix(k: &m::KeyEvent) -> bool {
    k.key == "space" && k.mods == [m::Mod::Ctrl]
}

fn mods(m: Modifiers) -> Vec<m::Mod> {
    let mut out = Vec::new();
    if m.contains(Modifiers::CTRL) {
        out.push(m::Mod::Ctrl);
    }
    if m.contains(Modifiers::ALT) {
        out.push(m::Mod::Alt);
    }
    if m.contains(Modifiers::SHIFT) {
        out.push(m::Mod::Shift);
    }
    if m.contains(Modifiers::SUPER) {
        out.push(m::Mod::Super);
    }
    out
}

/// Maps a termwiz key to a protocol key event. Raw control bytes become ctrl+key.
pub fn map_key(k: &KeyEvent) -> Option<m::KeyEvent> {
    let mut md = mods(k.modifiers);
    let named = |n: &str| Some((n.to_owned(), None));
    let (key, text) = match k.key {
        KeyCode::Char('\r') | KeyCode::Char('\n') | KeyCode::Enter => named("enter")?,
        KeyCode::Char('\t') | KeyCode::Tab => named("tab")?,
        KeyCode::Char('\x7f') | KeyCode::Char('\x08') | KeyCode::Backspace => named("backspace")?,
        KeyCode::Char('\x1b') | KeyCode::Escape => named("escape")?,
        KeyCode::Char('\0') | KeyCode::Char(' ')
            if k.modifiers.contains(Modifiers::CTRL) || k.key == KeyCode::Char('\0') =>
        {
            if !md.contains(&m::Mod::Ctrl) {
                md.push(m::Mod::Ctrl);
            }
            ("space".to_owned(), None)
        }
        KeyCode::Char(' ') => ("space".to_owned(), Some(" ".to_owned())),
        KeyCode::Char(c) if (c as u32) < 0x20 => {
            // 0x01..=0x1a are ctrl+a..ctrl+z.
            if !md.contains(&m::Mod::Ctrl) {
                md.push(m::Mod::Ctrl);
            }
            (char::from(b'a' + c as u8 - 1).to_string(), None)
        }
        KeyCode::Char(c) => {
            let ctrl_or_alt = md.contains(&m::Mod::Ctrl) || md.contains(&m::Mod::Alt);
            let lower = c.to_lowercase().next().unwrap_or(c);
            (lower.to_string(), (!ctrl_or_alt).then(|| c.to_string()))
        }
        KeyCode::UpArrow => named("up")?,
        KeyCode::DownArrow => named("down")?,
        KeyCode::LeftArrow => named("left")?,
        KeyCode::RightArrow => named("right")?,
        KeyCode::Home => named("home")?,
        KeyCode::End => named("end")?,
        KeyCode::PageUp => named("pageup")?,
        KeyCode::PageDown => named("pagedown")?,
        KeyCode::Insert => named("insert")?,
        KeyCode::Delete => named("delete")?,
        KeyCode::Function(n @ 1..=24) => (format!("f{n}"), None),
        _ => return None,
    };
    Some(m::KeyEvent {
        key,
        mods: md,
        text,
        event: m::KeyEventKind::Press,
    })
}

impl Viewer {
    pub fn new(attach: &m::SessionAttachOk) -> Viewer {
        let focus = attach
            .snapshot
            .tabs
            .iter()
            .map(|t| (t.id.clone(), t.focus.clone()))
            .collect();
        Viewer {
            view_id: attach.view_id.clone(),
            tab: attach.snapshot.current_tab.clone(),
            focus,
            panes: HashMap::new(),
            prefix_armed: false,
            dirty: true,
        }
    }

    pub fn focused(&self) -> Option<&str> {
        self.focus.get(&self.tab).map(String::as_str)
    }

    fn input(&self, events: Vec<m::InputEvent>) -> Vec<Action> {
        match self.focused() {
            Some(p) => vec![Action::Send(Request::PaneInput(m::PaneInput {
                pane_id: p.to_owned(),
                events,
            }))],
            None => vec![],
        }
    }

    pub fn on_input(&mut self, ev: &InputEvent) -> Vec<Action> {
        match ev {
            InputEvent::Key(k) => {
                let Some(key) = map_key(k) else { return vec![] };
                if self.prefix_armed {
                    self.prefix_armed = false;
                    if key.key == "d" && key.mods.is_empty() {
                        return vec![Action::Detach];
                    }
                    if is_prefix(&key) {
                        // prefix prefix sends the prefix itself (REQ-UI-002).
                        return self.input(vec![m::InputEvent::KeyEvent(key)]);
                    }
                    return vec![];
                }
                if is_prefix(&key) {
                    self.prefix_armed = true;
                    return vec![];
                }
                self.input(vec![m::InputEvent::KeyEvent(key)])
            }
            InputEvent::Paste(text) => {
                self.input(vec![m::InputEvent::Paste(m::Paste { text: text.clone() })])
            }
            InputEvent::Resized { cols, rows } => {
                vec![Action::Send(Request::ViewResize(m::ViewResize {
                    view_id: self.view_id.clone(),
                    cols: *cols as u16,
                    rows: *rows as u16,
                }))]
            }
            _ => vec![],
        }
    }

    /// Applies a frame; returns the ack to send.
    pub fn on_frame(&mut self, f: &m::Frame) -> Request {
        if f.full {
            self.panes
                .retain(|p, _| f.panes.iter().any(|pf| pf.pane_id == *p));
        }
        for pf in &f.panes {
            self.panes.entry(pf.pane_id.clone()).or_default().apply(pf);
        }
        self.tab = f.tab.id.clone();
        self.dirty = true;
        Request::FrameAck(m::FrameAck {
            view_id: f.view_id.clone(),
            frame_seq: f.frame_seq,
        })
    }

    pub fn on_event(&mut self, e: &Event) {
        match e {
            Event::LayoutFocused(f) => {
                self.focus.insert(f.tab_id.clone(), f.pane_id.clone());
                self.dirty = true;
            }
            Event::TabSelected(t) if t.view_id == self.view_id => self.tab = t.tab_id.clone(),
            Event::TabCreated(t) => {
                self.focus.entry(t.tab_id.clone()).or_default();
            }
            Event::PaneSpawned(p) => {
                let f = self.focus.entry(p.tab_id.clone()).or_default();
                if f.is_empty() {
                    *f = p.pane_id.clone();
                }
            }
            _ => {}
        }
    }

    /// Draws the focused pane at the top left of `s`.
    pub fn render(&mut self, s: &mut Surface) {
        self.dirty = false;
        let (w, h) = s.dimensions();
        s.add_change(Change::ClearScreen(ColorAttribute::Default));
        let Some(pane) = self.focused().and_then(|p| self.panes.get(p)) else {
            return;
        };
        for (y, row) in pane.rows.iter().enumerate().take(h) {
            s.add_change(Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(y),
            });
            let mut x = 0;
            while x < row.len().min(w) {
                match &row[x] {
                    Some(c) if !c.ch.is_empty() => {
                        s.add_change(Change::AllAttributes(attributes(c)));
                        s.add_change(Change::Text(c.ch.clone()));
                        x += c.width.max(1) as usize;
                    }
                    Some(c) => {
                        s.add_change(Change::AllAttributes(attributes(c)));
                        s.add_change(Change::Text(" ".into()));
                        x += 1;
                    }
                    None => {
                        s.add_change(Change::AllAttributes(CellAttributes::default()));
                        s.add_change(Change::Text(" ".into()));
                        x += 1;
                    }
                }
            }
        }
        s.add_change(Change::AllAttributes(CellAttributes::default()));
        s.add_change(Change::CursorPosition {
            x: Position::Absolute(pane.cursor.x as usize),
            y: Position::Absolute(pane.cursor.y as usize),
        });
        s.add_change(Change::CursorVisibility(if pane.cursor.visible {
            CursorVisibility::Visible
        } else {
            CursorVisibility::Hidden
        }));
    }
}

fn color(c: &m::Color) -> ColorAttribute {
    match c {
        m::Color::Default(_) => ColorAttribute::Default,
        m::Color::Indexed(i) => ColorAttribute::PaletteIndex(*i),
        m::Color::Rgb([r, g, b]) => {
            ColorAttribute::TrueColorWithDefaultFallback(SrgbaTuple::from((*r, *g, *b)))
        }
    }
}

fn attributes(c: &m::Cell) -> CellAttributes {
    let mut a = CellAttributes::default();
    a.set_foreground(color(&c.fg));
    a.set_background(color(&c.bg));
    let bit = |n: u16| c.attrs & (1 << n) != 0;
    if bit(0) {
        a.set_intensity(Intensity::Bold);
    } else if bit(2) {
        a.set_intensity(Intensity::Half);
    }
    a.set_italic(bit(1));
    if bit(3) {
        a.set_blink(Blink::Slow);
    }
    a.set_reverse(bit(4));
    a.set_invisible(bit(5));
    a.set_strikethrough(bit(6));
    a.set_underline(match (c.attrs >> 8) & 0b111 {
        0 => Underline::None,
        2 => Underline::Double,
        3 => Underline::Curly,
        4 => Underline::Dotted,
        5 => Underline::Dashed,
        _ => Underline::Single,
    });
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode, m: Modifiers) -> InputEvent {
        InputEvent::Key(KeyEvent {
            key: c,
            modifiers: m,
        })
    }

    fn attach() -> m::SessionAttachOk {
        m::SessionAttachOk {
            view_id: "V".into(),
            snapshot: m::SessionSnapshot {
                session: "s".into(),
                tabs: vec![m::TabSnapshot {
                    id: "T".into(),
                    focus: "P".into(),
                    ..Default::default()
                }],
                current_tab: "T".into(),
                marks: Default::default(),
            },
        }
    }

    fn sent_keys(actions: &[Action]) -> Vec<(String, Vec<m::Mod>, Option<String>)> {
        actions
            .iter()
            .flat_map(|a| match a {
                Action::Send(Request::PaneInput(p)) => p.events.clone(),
                _ => vec![],
            })
            .filter_map(|e| match e {
                m::InputEvent::KeyEvent(k) => Some((k.key, k.mods, k.text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn keys_map_to_structured_events() {
        let mut v = Viewer::new(&attach());
        assert_eq!(
            sent_keys(&v.on_input(&key(KeyCode::Char('A'), Modifiers::SHIFT))),
            [("a".into(), vec![m::Mod::Shift], Some("A".into()))]
        );
        assert_eq!(
            sent_keys(&v.on_input(&key(KeyCode::Char('\x03'), Modifiers::NONE))),
            [("c".into(), vec![m::Mod::Ctrl], None)]
        );
        assert_eq!(
            sent_keys(&v.on_input(&key(KeyCode::Char('\r'), Modifiers::NONE))),
            [("enter".into(), vec![], None)]
        );
        assert_eq!(
            sent_keys(&v.on_input(&key(KeyCode::UpArrow, Modifiers::NONE))),
            [("up".into(), vec![], None)]
        );
    }

    #[test]
    fn prefix_d_detaches_and_prefix_prefix_passes_through() {
        let mut v = Viewer::new(&attach());
        assert!(
            v.on_input(&key(KeyCode::Char('\0'), Modifiers::NONE))
                .is_empty()
        );
        assert_eq!(
            v.on_input(&key(KeyCode::Char('d'), Modifiers::NONE)),
            [Action::Detach]
        );
        v.on_input(&key(KeyCode::Char(' '), Modifiers::CTRL));
        assert_eq!(
            sent_keys(&v.on_input(&key(KeyCode::Char('\0'), Modifiers::NONE))),
            [("space".into(), vec![m::Mod::Ctrl], None)]
        );
        // An unbound key after the prefix is swallowed.
        v.on_input(&key(KeyCode::Char('\0'), Modifiers::NONE));
        assert!(
            v.on_input(&key(KeyCode::Char('q'), Modifiers::NONE))
                .is_empty()
        );
        assert_eq!(
            sent_keys(&v.on_input(&key(KeyCode::Char('d'), Modifiers::NONE))),
            [("d".into(), vec![], Some("d".into()))]
        );
    }

    #[test]
    fn renders_the_focused_pane() {
        let mut v = Viewer::new(&attach());
        let cell = |ch: &str| m::Cell {
            ch: ch.into(),
            width: 1,
            ..Default::default()
        };
        let ack = v.on_frame(&m::Frame {
            view_id: "V".into(),
            frame_seq: 3,
            full: true,
            panes: vec![m::PaneFrame {
                pane_id: "P".into(),
                rect: m::Rect {
                    x: 0,
                    y: 0,
                    w: 6,
                    h: 2,
                },
                cells: vec![m::CellRun {
                    x: 0,
                    y: 1,
                    run: vec![cell("$"), cell(" "), cell("h"), cell("i")],
                }],
                cursor: m::Cursor {
                    x: 4,
                    y: 1,
                    visible: true,
                    shape: m::CursorShape::Block,
                },
                scroll_offset: 0,
            }],
            tab: m::TabInfo {
                id: "T".into(),
                title: "sh".into(),
                index: 0,
                count: 1,
            },
            images: vec![],
        });
        assert_eq!(
            ack,
            Request::FrameAck(m::FrameAck {
                view_id: "V".into(),
                frame_seq: 3
            })
        );
        let mut s = Surface::new(10, 3);
        v.render(&mut s);
        assert_eq!(
            s.screen_chars_to_string(),
            "          \n$ hi      \n          \n"
        );
        assert_eq!(s.cursor_position(), (4, 1));
    }
}
