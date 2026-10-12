//! Colors for the client's chrome (REQ-UI-051): a bundled dark and light theme, chosen from
//! `[theme] name`, or from the host terminal's background (OSC 11) and mode 2031 when the
//! name is `terminal`.

use termwiz::color::{ColorAttribute, SrgbaTuple};

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub accent: ColorAttribute,
    pub accent_fg: ColorAttribute,
    pub muted: ColorAttribute,
    pub warn: ColorAttribute,
    pub ok: ColorAttribute,
    pub error: ColorAttribute,
    pub text: ColorAttribute,
    pub bar_bg: ColorAttribute,
    pub bar_fg: ColorAttribute,
    pub selection: ColorAttribute,
    pub overlay_bg: ColorAttribute,
}

fn rgb(r: u8, g: u8, b: u8) -> ColorAttribute {
    ColorAttribute::TrueColorWithPaletteFallback(
        SrgbaTuple::from((r, g, b)),
        termwiz::color::AnsiColor::White.into(),
    )
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            dark: true,
            accent: rgb(0x4f, 0xa3, 0xe3),
            accent_fg: rgb(0x0b, 0x0e, 0x14),
            muted: rgb(0x6b, 0x72, 0x80),
            warn: rgb(0xe3, 0xb3, 0x41),
            ok: rgb(0x8c, 0xc2, 0x65),
            error: rgb(0xe0, 0x6c, 0x75),
            text: rgb(0xd7, 0xdb, 0xe2),
            bar_bg: rgb(0x14, 0x18, 0x20),
            bar_fg: rgb(0xb8, 0xbe, 0xc8),
            selection: rgb(0x3a, 0x4a, 0x6a),
            overlay_bg: rgb(0x10, 0x14, 0x1b),
        }
    }

    pub fn light() -> Theme {
        Theme {
            dark: false,
            accent: rgb(0x1f, 0x6f, 0xc4),
            accent_fg: rgb(0xff, 0xff, 0xff),
            muted: rgb(0x80, 0x86, 0x90),
            warn: rgb(0xb0, 0x7a, 0x00),
            ok: rgb(0x3b, 0x8a, 0x2e),
            error: rgb(0xc0, 0x30, 0x30),
            text: rgb(0x20, 0x24, 0x2b),
            bar_bg: rgb(0xe8, 0xea, 0xee),
            bar_fg: rgb(0x30, 0x34, 0x3c),
            selection: rgb(0xc4, 0xd8, 0xf2),
            overlay_bg: rgb(0xf2, 0xf4, 0xf7),
        }
    }

    /// A theme from a plugin's palette (the `theme` plugin, 0.4.0).
    pub fn from_palette(p: &femux_plugin::Palette) -> Theme {
        let c = |[r, g, b]: [u8; 3]| rgb(r, g, b);
        Theme {
            dark: p.dark,
            accent: c(p.accent),
            accent_fg: c(p.accent_fg),
            muted: c(p.muted),
            warn: c(p.warn),
            ok: c(p.ok),
            error: c(p.error),
            text: c(p.text),
            bar_bg: c(p.bar_bg),
            bar_fg: c(p.bar_fg),
            selection: c(p.selection),
            overlay_bg: c(p.overlay_bg),
        }
    }

    /// Picks a theme (REQ-UI-051): a plugin's palette for a name it knows, else `dark`/`light`
    /// by name, else from mode 2031's answer, else from the background's luminance, else dark.
    /// The error is a theme a plugin knows but could not load; the fallback applies then.
    pub fn resolve(
        name: &str,
        plugins: &[Box<dyn femux_plugin::ClientPlugin>],
        dark_mode: Option<bool>,
        bg: Option<[u8; 3]>,
    ) -> (Theme, Option<String>) {
        if name != "terminal" {
            for p in plugins {
                match p.theme(name) {
                    Some(Ok(palette)) => return (Theme::from_palette(&palette), None),
                    Some(Err(e)) => {
                        return (Theme::pick("terminal", dark_mode, bg), Some(e));
                    }
                    None => {}
                }
            }
        }
        (Theme::pick(name, dark_mode, bg), None)
    }

    /// Picks a built-in theme: `dark`/`light` by name, else from mode 2031's answer, else from
    /// the background's luminance, else dark.
    pub fn pick(name: &str, dark_mode: Option<bool>, bg: Option<[u8; 3]>) -> Theme {
        match name {
            "dark" => return Theme::dark(),
            "light" => return Theme::light(),
            _ => {}
        }
        let dark = dark_mode.unwrap_or_else(|| bg.is_none_or(|[r, g, b]| luminance(r, g, b) < 0.5));
        if dark { Theme::dark() } else { Theme::light() }
    }
}

/// Relative luminance in [0, 1].
pub fn luminance(r: u8, g: u8, b: u8) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_by_name_mode_or_background() {
        assert!(Theme::pick("dark", Some(false), None).dark);
        assert!(!Theme::pick("light", Some(true), None).dark);
        assert!(!Theme::pick("terminal", Some(false), Some([0, 0, 0])).dark);
        assert!(!Theme::pick("terminal", None, Some([250, 250, 250])).dark);
        assert!(Theme::pick("terminal", None, Some([20, 20, 30])).dark);
        assert!(Theme::pick("terminal", None, None).dark);
    }
}
