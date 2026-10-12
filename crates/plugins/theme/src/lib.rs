//! `theme` plugin (0.4): the `[theme] name` values other than `terminal` (REQ-UI-051): the
//! bundled themes `dark`, `light` and `high-contrast`, or a path to a JSON file:
//!
//! ```json
//! { "base": "dark", "accent": "#4fa3e3", "warn": "#e3b341" }
//! ```
//!
//! `base` (`dark` or `light`, default `dark`) supplies every color the file leaves out; keys are
//! `dark` and the color names of [`femux_plugin::Palette`]. Unknown keys are errors, as in the
//! config (REQ-CFG-003).

use femux_plugin::{ClientPlugin, Palette, Plugin};
use serde_json::Value;

pub fn plugin() -> Plugin {
    Plugin {
        name: "theme",
        client: Some(Box::new(Themes)),
        daemon: None,
        cli: None,
    }
}

pub const BUNDLED: &[&str] = &["dark", "light", "high-contrast"];

pub fn dark() -> Palette {
    Palette {
        dark: true,
        accent: [0x4f, 0xa3, 0xe3],
        accent_fg: [0x0b, 0x0e, 0x14],
        muted: [0x6b, 0x72, 0x80],
        warn: [0xe3, 0xb3, 0x41],
        ok: [0x8c, 0xc2, 0x65],
        error: [0xe0, 0x6c, 0x75],
        text: [0xd7, 0xdb, 0xe2],
        bar_bg: [0x14, 0x18, 0x20],
        bar_fg: [0xb8, 0xbe, 0xc8],
        selection: [0x3a, 0x4a, 0x6a],
        overlay_bg: [0x10, 0x14, 0x1b],
    }
}

pub fn light() -> Palette {
    Palette {
        dark: false,
        accent: [0x1f, 0x6f, 0xc4],
        accent_fg: [0xff, 0xff, 0xff],
        muted: [0x80, 0x86, 0x90],
        warn: [0xb0, 0x7a, 0x00],
        ok: [0x3b, 0x8a, 0x2e],
        error: [0xc0, 0x30, 0x30],
        text: [0x20, 0x24, 0x2b],
        bar_bg: [0xe8, 0xea, 0xee],
        bar_fg: [0x30, 0x34, 0x3c],
        selection: [0xc4, 0xd8, 0xf2],
        overlay_bg: [0xf2, 0xf4, 0xf7],
    }
}

pub fn high_contrast() -> Palette {
    Palette {
        dark: true,
        accent: [0x00, 0xd7, 0xff],
        accent_fg: [0x00, 0x00, 0x00],
        muted: [0xa8, 0xa8, 0xa8],
        warn: [0xff, 0xd7, 0x00],
        ok: [0x5f, 0xff, 0x5f],
        error: [0xff, 0x5f, 0x5f],
        text: [0xff, 0xff, 0xff],
        bar_bg: [0x00, 0x00, 0x00],
        bar_fg: [0xff, 0xff, 0xff],
        selection: [0x00, 0x5f, 0xaf],
        overlay_bg: [0x12, 0x12, 0x12],
    }
}

/// `#rrggbb` to `[r, g, b]`.
pub fn color(s: &str) -> Option<[u8; 3]> {
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn slot<'a>(p: &'a mut Palette, key: &str) -> Option<&'a mut [u8; 3]> {
    Some(match key {
        "accent" => &mut p.accent,
        "accent_fg" => &mut p.accent_fg,
        "muted" => &mut p.muted,
        "warn" => &mut p.warn,
        "ok" => &mut p.ok,
        "error" => &mut p.error,
        "text" => &mut p.text,
        "bar_bg" => &mut p.bar_bg,
        "bar_fg" => &mut p.bar_fg,
        "selection" => &mut p.selection,
        "overlay_bg" => &mut p.overlay_bg,
        _ => return None,
    })
}

/// A theme file's text to a palette; errors name the key.
pub fn parse(text: &str) -> Result<Palette, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or("a theme is a JSON object")?;
    let mut p = match obj.get("base").map(|b| b.as_str()) {
        None | Some(Some("dark")) => dark(),
        Some(Some("light")) => light(),
        Some(_) => return Err("base is \"dark\" or \"light\"".into()),
    };
    for (key, value) in obj {
        match key.as_str() {
            "base" => {}
            "dark" => p.dark = value.as_bool().ok_or("dark is true or false")?,
            k => {
                let s = slot(&mut p, k).ok_or_else(|| format!("unknown key {k:?}"))?;
                *s = value
                    .as_str()
                    .and_then(color)
                    .ok_or_else(|| format!("{k} is not a #rrggbb color"))?;
            }
        }
    }
    Ok(p)
}

/// A bundled name or a file path (contains `/` or ends in `.json`).
pub fn load(name: &str) -> Option<Result<Palette, String>> {
    if name.contains('/') || name.ends_with(".json") {
        let path = femux_config::expand(name);
        return Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("{}: {e}", path.display()))
                .and_then(|t| parse(&t).map_err(|e| format!("{}: {e}", path.display()))),
        );
    }
    Some(match name {
        "terminal" => return None,
        "dark" => Ok(dark()),
        "light" => Ok(light()),
        "high-contrast" => Ok(high_contrast()),
        other => Err(format!(
            "unknown theme {other:?}; bundled: {}",
            BUNDLED.join(", ")
        )),
    })
}

struct Themes;

impl ClientPlugin for Themes {
    fn name(&self) -> &'static str {
        "theme"
    }
    fn theme(&self, name: &str) -> Option<Result<Palette, String>> {
        load(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_themes_by_name() {
        assert_eq!(load("dark"), Some(Ok(dark())));
        assert_eq!(load("light"), Some(Ok(light())));
        assert!(load("high-contrast").unwrap().unwrap().dark);
        assert_eq!(
            load("terminal"),
            None,
            "terminal is the client's own choice"
        );
        let e = load("nord").unwrap().unwrap_err();
        assert!(e.contains("nord") && e.contains("high-contrast"), "{e}");
    }

    #[test]
    fn files_override_a_base_and_reject_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("mine.json");
        std::fs::write(&f, r##"{"base": "light", "accent": "#112233"}"##).unwrap();
        let p = load(f.to_str().unwrap()).unwrap().unwrap();
        assert_eq!(p.accent, [0x11, 0x22, 0x33]);
        assert_eq!((p.dark, p.text), (false, light().text));
        std::fs::write(&f, r##"{"accnet": "#112233"}"##).unwrap();
        let e = load(f.to_str().unwrap()).unwrap().unwrap_err();
        assert!(e.contains("mine.json") && e.contains("accnet"), "{e}");
        std::fs::write(&f, r##"{"warn": "yellow"}"##).unwrap();
        assert!(
            load(f.to_str().unwrap())
                .unwrap()
                .unwrap_err()
                .contains("warn")
        );
        assert!(load("/no/such/theme.json").unwrap().is_err());
        assert_eq!(color("#ff0080"), Some([255, 0, 128]));
        assert_eq!(color("ff0080"), None);
    }
}
