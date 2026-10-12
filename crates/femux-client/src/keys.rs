//! Key chords: parsing binding strings (`ctrl+space`, `H`, `'`), matching protocol key
//! events against them, and mapping termwiz input to protocol key events.

use std::fmt;

use femux_proto::msg as m;
use termwiz::input::{KeyCode, KeyEvent, Modifiers};

/// A key with modifiers. `shift+<letter>` is normalized to the uppercase letter, so `H` and
/// `shift+h` are the same chord; `shift` stays a flag only on named keys (`shift+tab`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub sup: bool,
}

impl Chord {
    pub fn parse(s: &str) -> Option<Chord> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut c = Chord {
            key: String::new(),
            ctrl: false,
            alt: false,
            shift: false,
            sup: false,
        };
        // The key itself may be `+`.
        let (mods, key) = match s.rsplit_once('+') {
            Some((m, k)) if !k.is_empty() => (m, k),
            Some((m, "")) if m.ends_with('+') => (&m[..m.len() - 1], "+"),
            _ => ("", s),
        };
        for md in mods.split('+').filter(|m| !m.is_empty()) {
            match md.to_ascii_lowercase().as_str() {
                "ctrl" | "c" | "control" => c.ctrl = true,
                "alt" | "a" | "meta" | "opt" => c.alt = true,
                "shift" | "s" => c.shift = true,
                "super" | "cmd" | "win" => c.sup = true,
                _ => return None,
            }
        }
        let mut key = key.to_owned();
        if key.chars().count() == 1 {
            let ch = key.chars().next().unwrap();
            if c.shift && ch.is_alphabetic() {
                key = ch.to_uppercase().to_string();
                c.shift = false;
            }
        } else {
            key = key.to_ascii_lowercase();
            key = match key.as_str() {
                "esc" => "escape".into(),
                "return" | "cr" => "enter".into(),
                "bs" => "backspace".into(),
                "del" => "delete".into(),
                "spc" => "space".into(),
                _ => key,
            };
        }
        c.key = key;
        Some(c)
    }

    /// The chord a protocol key event types. A text key with at most shift held is the text
    /// itself (`H`, `:`); anything else keeps its key name and modifiers.
    pub fn of(k: &m::KeyEvent) -> Chord {
        let ctrl = k.mods.contains(&m::Mod::Ctrl);
        let alt = k.mods.contains(&m::Mod::Alt);
        let shift = k.mods.contains(&m::Mod::Shift);
        let sup = k.mods.contains(&m::Mod::Super);
        if let Some(t) = &k.text
            && t.chars().count() == 1
            && !ctrl
            && !alt
            && !sup
            && t != " "
        {
            return Chord {
                key: t.clone(),
                ctrl,
                alt,
                shift: false,
                sup,
            };
        }
        let mut key = k.key.clone();
        let mut shift = shift;
        if shift && key.chars().count() == 1 && key.chars().all(|c| c.is_alphabetic()) {
            key = key.to_uppercase();
            shift = false;
        }
        Chord {
            key,
            ctrl,
            alt,
            shift,
            sup,
        }
    }

    /// Whether this chord is one of the configured prefixes.
    pub fn is_prefix(&self, prefixes: &[Chord]) -> bool {
        prefixes.contains(self)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.alt {
            f.write_str("alt+")?;
        }
        if self.shift {
            f.write_str("shift+")?;
        }
        if self.sup {
            f.write_str("super+")?;
        }
        f.write_str(&self.key)
    }
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
            if c.is_uppercase() && !md.contains(&m::Mod::Shift) {
                md.push(m::Mod::Shift);
            }
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

/// A protocol key event for tests and synthetic input: `key("H")`, `key("ctrl+p")`.
pub fn key(chord: &str) -> m::KeyEvent {
    Chord::parse(chord).expect("valid chord").event()
}

impl Chord {
    /// The key event this chord types (the inverse of [`Chord::of`]).
    pub fn event(&self) -> m::KeyEvent {
        let c = self;
        let mut mods = Vec::new();
        if c.ctrl {
            mods.push(m::Mod::Ctrl);
        }
        if c.alt {
            mods.push(m::Mod::Alt);
        }
        if c.sup {
            mods.push(m::Mod::Super);
        }
        let single = c.key.chars().count() == 1;
        let text = (single && !c.ctrl && !c.alt && !c.sup).then(|| c.key.clone());
        let key = if single {
            let ch = c.key.chars().next().unwrap();
            if ch.is_uppercase() {
                mods.push(m::Mod::Shift);
            }
            ch.to_lowercase().to_string()
        } else {
            if c.shift {
                mods.push(m::Mod::Shift);
            }
            c.key.clone()
        };
        m::KeyEvent {
            key,
            mods,
            text,
            event: m::KeyEventKind::Press,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse_and_normalize() {
        assert_eq!(
            Chord::parse("ctrl+space").unwrap().to_string(),
            "ctrl+space"
        );
        assert_eq!(Chord::parse("shift+h").unwrap().to_string(), "H");
        assert_eq!(Chord::parse("H").unwrap(), Chord::parse("shift+h").unwrap());
        assert_eq!(Chord::parse("Esc").unwrap().key, "escape");
        assert_eq!(Chord::parse("ctrl++").unwrap().key, "+");
        assert_eq!(Chord::parse("'").unwrap().key, "'");
        assert!(Chord::parse("hyper+x").is_none());
    }

    #[test]
    fn events_map_to_chords() {
        assert_eq!(Chord::of(&key("H")), Chord::parse("H").unwrap());
        assert_eq!(Chord::of(&key(":")), Chord::parse(":").unwrap());
        assert_eq!(Chord::of(&key("ctrl+p")), Chord::parse("ctrl+p").unwrap());
        assert_eq!(
            Chord::of(&key("ctrl+space")),
            Chord::parse("ctrl+space").unwrap()
        );
        assert_eq!(Chord::of(&key("escape")), Chord::parse("esc").unwrap());
        assert_eq!(Chord::of(&key("1")).key, "1");
        // A termwiz uppercase char carries shift and the text.
        let ev = map_key(&KeyEvent {
            key: KeyCode::Char('J'),
            modifiers: Modifiers::NONE,
        })
        .unwrap();
        assert_eq!((ev.key.as_str(), ev.text.as_deref()), ("j", Some("J")));
        assert_eq!(Chord::of(&ev).key, "J");
    }
}
