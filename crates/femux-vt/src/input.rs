//! Encoding structured input for a pane according to the modes it requested (REQ-PROTO-030):
//! cursor/keypad modes and Kitty keyboard flags for keys, mouse tracking modes and formats for
//! the mouse, bracketed paste for pastes. libghostty-vt reads the modes from the terminal.

use std::ffi::{c_char, c_void};

use crate::{Error, Terminal, check, ffi};

/// Modifier keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_: bool,
}

impl Mods {
    fn bits(self) -> ffi::GhosttyMods {
        (self.shift as u16)
            | (self.ctrl as u16) << 1
            | (self.alt as u16) << 2
            | (self.super_ as u16) << 3
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyAction {
    #[default]
    Press,
    Repeat,
    Release,
}

/// A key event. `name` is a single unshifted character for text keys, else a named key
/// (enter, tab, backspace, escape, space, up, down, left, right, home, end, pageup, pagedown,
/// insert, delete, f1–f24). `text` is what the key types, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Key<'a> {
    pub name: &'a str,
    pub mods: Mods,
    pub text: Option<&'a str>,
    pub action: KeyAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Move,
    WheelUp,
    WheelDown,
}

/// A mouse event at pane-relative cell `(x, y)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub x: u16,
    pub y: u16,
    pub button: Option<MouseButton>,
    pub mods: Mods,
    pub action: MouseAction,
}

fn ghostty_key(name: &str) -> ffi::GhosttyKey {
    use ffi::GhosttyKey as K;
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let c = c.to_ascii_lowercase();
        return match c {
            'a'..='z' => K(K::GHOSTTY_KEY_A.0 + (c as i32 - 'a' as i32)),
            '0'..='9' => K(K::GHOSTTY_KEY_DIGIT_0.0 + (c as i32 - '0' as i32)),
            '`' => K::GHOSTTY_KEY_BACKQUOTE,
            '\\' => K::GHOSTTY_KEY_BACKSLASH,
            '[' => K::GHOSTTY_KEY_BRACKET_LEFT,
            ']' => K::GHOSTTY_KEY_BRACKET_RIGHT,
            ',' => K::GHOSTTY_KEY_COMMA,
            '=' => K::GHOSTTY_KEY_EQUAL,
            '-' => K::GHOSTTY_KEY_MINUS,
            '.' => K::GHOSTTY_KEY_PERIOD,
            '\'' => K::GHOSTTY_KEY_QUOTE,
            ';' => K::GHOSTTY_KEY_SEMICOLON,
            '/' => K::GHOSTTY_KEY_SLASH,
            ' ' => K::GHOSTTY_KEY_SPACE,
            _ => K::GHOSTTY_KEY_UNIDENTIFIED,
        };
    }
    match name {
        "enter" => K::GHOSTTY_KEY_ENTER,
        "tab" => K::GHOSTTY_KEY_TAB,
        "backspace" => K::GHOSTTY_KEY_BACKSPACE,
        "escape" => K::GHOSTTY_KEY_ESCAPE,
        "space" => K::GHOSTTY_KEY_SPACE,
        "up" => K::GHOSTTY_KEY_ARROW_UP,
        "down" => K::GHOSTTY_KEY_ARROW_DOWN,
        "left" => K::GHOSTTY_KEY_ARROW_LEFT,
        "right" => K::GHOSTTY_KEY_ARROW_RIGHT,
        "home" => K::GHOSTTY_KEY_HOME,
        "end" => K::GHOSTTY_KEY_END,
        "pageup" => K::GHOSTTY_KEY_PAGE_UP,
        "pagedown" => K::GHOSTTY_KEY_PAGE_DOWN,
        "insert" => K::GHOSTTY_KEY_INSERT,
        "delete" => K::GHOSTTY_KEY_DELETE,
        f if f.starts_with('f') => match f[1..].parse::<i32>() {
            Ok(n @ 1..=24) => K(K::GHOSTTY_KEY_F1.0 + n - 1),
            _ => K::GHOSTTY_KEY_UNIDENTIFIED,
        },
        _ => K::GHOSTTY_KEY_UNIDENTIFIED,
    }
}

/// Runs an encoder that writes into a caller buffer, growing it on OUT_OF_SPACE.
fn encode_into(
    mut f: impl FnMut(*mut c_char, usize, &mut usize) -> ffi::GhosttyResult,
) -> Result<Vec<u8>, Error> {
    let mut buf = vec![0u8; 128];
    loop {
        let mut n = 0usize;
        let r = f(buf.as_mut_ptr() as *mut c_char, buf.len(), &mut n);
        if r == ffi::GhosttyResult::GHOSTTY_OUT_OF_SPACE && n > buf.len() {
            buf.resize(n, 0);
            continue;
        }
        if r == ffi::GhosttyResult::GHOSTTY_OUT_OF_SPACE {
            let len = buf.len() * 2;
            buf.resize(len, 0);
            continue;
        }
        check(r)?;
        buf.truncate(n);
        return Ok(buf);
    }
}

impl Terminal {
    /// Bytes to write to the pane for `key` under the pane's current keyboard modes.
    pub fn encode_key(&mut self, key: &Key) -> Result<Vec<u8>, Error> {
        let ev = self.key_event;
        let mut chars = key.name.chars();
        let single = match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        };
        let text = key.text.or(match key.name {
            "enter" => Some("\r"),
            "tab" => Some("\t"),
            "space" => Some(" "),
            _ => None,
        });
        // SAFETY: encoder and event are live; strings outlive the encode call.
        unsafe {
            ffi::ghostty_key_encoder_setopt_from_terminal(self.keys, self.raw);
            ffi::ghostty_key_event_set_action(
                ev,
                match key.action {
                    KeyAction::Press => ffi::GhosttyKeyAction::GHOSTTY_KEY_ACTION_PRESS,
                    KeyAction::Repeat => ffi::GhosttyKeyAction::GHOSTTY_KEY_ACTION_REPEAT,
                    KeyAction::Release => ffi::GhosttyKeyAction::GHOSTTY_KEY_ACTION_RELEASE,
                },
            );
            ffi::ghostty_key_event_set_key(ev, ghostty_key(key.name));
            ffi::ghostty_key_event_set_mods(ev, key.mods.bits());
            // Shift is consumed when it changed the text (e.g. "a" -> "A").
            let consumed = match (text, single) {
                (Some(t), Some(c)) if key.mods.shift && t != c.to_string() => Mods {
                    shift: true,
                    ..Mods::default()
                },
                _ => Mods::default(),
            };
            ffi::ghostty_key_event_set_consumed_mods(ev, consumed.bits());
            ffi::ghostty_key_event_set_unshifted_codepoint(ev, single.map_or(0, |c| c as u32));
            match text {
                Some(t) => {
                    ffi::ghostty_key_event_set_utf8(ev, t.as_ptr() as *const c_char, t.len())
                }
                None => ffi::ghostty_key_event_set_utf8(ev, std::ptr::null(), 0),
            }
            let keys = self.keys;
            encode_into(|buf, len, n| ffi::ghostty_key_encoder_encode(keys, ev, buf, len, n))
        }
    }

    /// Bytes to write to the pane for a mouse event; empty when the pane does not track the mouse.
    pub fn encode_mouse(&mut self, m: &MouseInput) -> Result<Vec<u8>, Error> {
        let (cw, ch) = self.effects.cell_px;
        let (cols, rows) = self.size();
        let ev = self.mouse_event;
        use ffi::GhosttyMouseAction as A;
        use ffi::GhosttyMouseButton as B;
        let (action, button) = match m.action {
            MouseAction::WheelUp => (
                A::GHOSTTY_MOUSE_ACTION_PRESS,
                Some(B::GHOSTTY_MOUSE_BUTTON_FOUR),
            ),
            MouseAction::WheelDown => (
                A::GHOSTTY_MOUSE_ACTION_PRESS,
                Some(B::GHOSTTY_MOUSE_BUTTON_FIVE),
            ),
            a => (
                match a {
                    MouseAction::Press => A::GHOSTTY_MOUSE_ACTION_PRESS,
                    MouseAction::Release => A::GHOSTTY_MOUSE_ACTION_RELEASE,
                    _ => A::GHOSTTY_MOUSE_ACTION_MOTION,
                },
                m.button.map(|b| match b {
                    MouseButton::Left => B::GHOSTTY_MOUSE_BUTTON_LEFT,
                    MouseButton::Middle => B::GHOSTTY_MOUSE_BUTTON_MIDDLE,
                    MouseButton::Right => B::GHOSTTY_MOUSE_BUTTON_RIGHT,
                }),
            ),
        };
        let size = ffi::GhosttyMouseEncoderSize {
            size: std::mem::size_of::<ffi::GhosttyMouseEncoderSize>(),
            screen_width: cols as u32 * cw,
            screen_height: rows as u32 * ch,
            cell_width: cw,
            cell_height: ch,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };
        // SAFETY: encoder and event are live; option values outlive the calls.
        unsafe {
            ffi::ghostty_mouse_encoder_setopt_from_terminal(self.mouse, self.raw);
            ffi::ghostty_mouse_encoder_setopt(
                self.mouse,
                ffi::GhosttyMouseEncoderOption::GHOSTTY_MOUSE_ENCODER_OPT_SIZE,
                &size as *const _ as *const c_void,
            );
            ffi::ghostty_mouse_event_set_action(ev, action);
            match button {
                Some(b) => ffi::ghostty_mouse_event_set_button(ev, b),
                None => ffi::ghostty_mouse_event_clear_button(ev),
            }
            ffi::ghostty_mouse_event_set_mods(ev, m.mods.bits());
            ffi::ghostty_mouse_event_set_position(
                ev,
                ffi::GhosttyMousePosition {
                    x: (m.x as u32 * cw + cw / 2) as f32,
                    y: (m.y as u32 * ch + ch / 2) as f32,
                },
            );
            let mouse = self.mouse;
            encode_into(|buf, len, n| ffi::ghostty_mouse_encoder_encode(mouse, ev, buf, len, n))
        }
    }

    /// Bytes to write for pasted text: control bytes stripped, bracketed if the pane set mode 2004.
    pub fn encode_paste(&self, text: &str) -> Result<Vec<u8>, Error> {
        let bracketed = self.dec_mode(2004);
        let mut data = text.as_bytes().to_vec();
        let len = data.len();
        // SAFETY: `data` is a private mutable copy (the encoder edits it in place).
        encode_into(|buf, cap, n| unsafe {
            ffi::ghostty_paste_encode(
                data.as_mut_ptr() as *mut c_char,
                len,
                bracketed,
                buf,
                cap,
                n,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key<'_> {
        Key {
            name,
            ..Key::default()
        }
    }

    #[test]
    fn cursor_keys_follow_decckm() {
        let mut t = Terminal::new(10, 5, 100).unwrap();
        assert_eq!(t.encode_key(&key("up")).unwrap(), b"\x1b[A");
        t.write(b"\x1b[?1h");
        assert_eq!(t.encode_key(&key("up")).unwrap(), b"\x1bOA");
    }

    #[test]
    fn text_ctrl_and_kitty_keys() {
        let mut t = Terminal::new(10, 5, 100).unwrap();
        let a = Key {
            name: "a",
            text: Some("a"),
            ..Key::default()
        };
        assert_eq!(t.encode_key(&a).unwrap(), b"a");
        let ctrl_c = Key {
            name: "c",
            mods: Mods {
                ctrl: true,
                ..Mods::default()
            },
            ..Key::default()
        };
        assert_eq!(t.encode_key(&ctrl_c).unwrap(), b"\x03");
        assert_eq!(t.encode_key(&key("enter")).unwrap(), b"\r");
        // Kitty keyboard: disambiguate escape codes (flag 1).
        t.write(b"\x1b[>1u");
        assert_eq!(t.encode_key(&key("escape")).unwrap(), b"\x1b[27u");
        assert_eq!(t.encode_key(&ctrl_c).unwrap(), b"\x1b[99;5u");
    }

    #[test]
    fn mouse_only_when_tracked() {
        let mut t = Terminal::new(10, 5, 100).unwrap();
        let click = MouseInput {
            x: 2,
            y: 3,
            button: Some(MouseButton::Left),
            mods: Mods::default(),
            action: MouseAction::Press,
        };
        assert!(t.encode_mouse(&click).unwrap().is_empty());
        t.write(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(t.encode_mouse(&click).unwrap(), b"\x1b[<0;3;4M");
    }

    #[test]
    fn paste_is_bracketed_on_request() {
        let mut t = Terminal::new(10, 5, 100).unwrap();
        assert_eq!(t.encode_paste("hi").unwrap(), b"hi");
        t.write(b"\x1b[?2004h");
        assert_eq!(t.encode_paste("hi").unwrap(), b"\x1b[200~hi\x1b[201~");
    }
}
