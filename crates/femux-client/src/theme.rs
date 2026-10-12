//! Colors for the client's chrome (REQ-UI-051): a bundled dark and light theme chosen by
//! `[theme] name`, or, for `terminal`, colors derived from the host terminal itself: its
//! foreground and background (OSC 10/11) and its own palette's blue, yellow, green and red.

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

/// What the host terminal reported at startup (the probe).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostColors {
    /// Mode 2031's answer. Terminals report the desktop's preference here, which can differ
    /// from their own colors, so it only decides when the background is unknown.
    pub dark_mode: Option<bool>,
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
}

/// `a` moved `t` of the way to `b`.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// The host's own palette entry (`n` of the 16 ANSI colors).
fn ansi(n: u8) -> ColorAttribute {
    ColorAttribute::PaletteIndex(n)
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

    /// The `terminal` theme when the host reported its background: chrome in the host's
    /// own colors. Bars and overlays are the background tinted toward the foreground; accent,
    /// warn, ok and error are the host palette's blue, yellow, green and red, so any terminal
    /// color scheme carries over.
    pub fn from_terminal(dark: bool, fg: Option<[u8; 3]>, bg: [u8; 3]) -> Theme {
        let fg = fg.unwrap_or(if dark {
            [0xd7, 0xdb, 0xe2]
        } else {
            [0x20, 0x24, 0x2b]
        });
        let c = |[r, g, b]: [u8; 3]| rgb(r, g, b);
        Theme {
            dark,
            accent: ansi(4),
            accent_fg: c(bg),
            muted: c(mix(fg, bg, 0.45)),
            warn: ansi(3),
            ok: ansi(2),
            error: ansi(1),
            text: c(fg),
            bar_bg: c(mix(bg, fg, 0.08)),
            bar_fg: c(mix(fg, bg, 0.2)),
            selection: c(mix(bg, fg, 0.2)),
            overlay_bg: c(mix(bg, fg, 0.05)),
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

    /// Picks a theme (REQ-UI-051): a plugin's palette for a name it knows, else a built-in
    /// one ([`Theme::pick`]). The error is a theme a plugin knows but could not load; the
    /// `terminal` theme applies then.
    pub fn resolve(
        name: &str,
        plugins: &[Box<dyn femux_plugin::ClientPlugin>],
        host: HostColors,
    ) -> (Theme, Option<String>) {
        if name != "terminal" {
            for p in plugins {
                match p.theme(name) {
                    Some(Ok(palette)) => return (Theme::from_palette(&palette), None),
                    Some(Err(e)) => return (Theme::pick("terminal", host), Some(e)),
                    None => {}
                }
            }
        }
        (Theme::pick(name, host), None)
    }

    /// `dark`/`light` by name. Otherwise (`terminal`, or an unknown name) light or dark
    /// follows the reported background's luminance, else mode 2031, else dark; with a known
    /// background the colors come from the terminal ([`Theme::from_terminal`]).
    pub fn pick(name: &str, host: HostColors) -> Theme {
        match name {
            "dark" => return Theme::dark(),
            "light" => return Theme::light(),
            _ => {}
        }
        let dark = host
            .bg
            .map(|[r, g, b]| luminance(r, g, b) < 0.5)
            .or(host.dark_mode)
            .unwrap_or(true);
        match host.bg {
            Some(bg) if name == "terminal" => Theme::from_terminal(dark, host.fg, bg),
            _ if dark => Theme::dark(),
            _ => Theme::light(),
        }
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
    fn plugin_palettes_win_and_their_errors_fall_back() {
        let plugins = vec![femux_plugin_theme_client()];
        let (t, err) = Theme::resolve("high-contrast", &plugins, HostColors::default());
        assert_eq!(err, None);
        assert_eq!(t, Theme::from_palette(&femux_plugin_theme::high_contrast()));
        let light = HostColors {
            dark_mode: Some(false),
            ..Default::default()
        };
        let (t, err) = Theme::resolve("nord", &plugins, light);
        assert!(err.unwrap().contains("nord"));
        assert!(!t.dark, "falls back to the terminal's light/dark");
        // Without the plugin, dark and light still work and unknown names are dark.
        assert!(!Theme::resolve("light", &[], HostColors::default()).0.dark);
        assert_eq!(
            Theme::resolve("nord", &[], HostColors::default()),
            (Theme::dark(), None)
        );
    }

    fn femux_plugin_theme_client() -> Box<dyn femux_plugin::ClientPlugin> {
        femux_plugin_theme::plugin().client.unwrap()
    }

    fn host(dark_mode: Option<bool>, bg: Option<[u8; 3]>) -> HostColors {
        HostColors {
            dark_mode,
            fg: None,
            bg,
        }
    }

    #[test]
    fn picks_by_name_then_background_then_mode() {
        assert!(Theme::pick("dark", host(Some(false), None)).dark);
        assert!(!Theme::pick("light", host(Some(true), None)).dark);
        // The background wins over mode 2031: Ghostty with a dark theme on a light desktop
        // reports light mode but a dark background.
        assert!(Theme::pick("terminal", host(Some(false), Some([0x30, 0x34, 0x46]))).dark);
        assert!(!Theme::pick("terminal", host(Some(true), Some([250, 250, 250]))).dark);
        // Without a background, the mode decides, else dark.
        assert!(!Theme::pick("terminal", host(Some(false), None)).dark);
        assert!(Theme::pick("terminal", host(None, None)).dark);
    }

    #[test]
    fn terminal_theme_takes_the_hosts_colors() {
        let frappe = HostColors {
            dark_mode: Some(false),
            fg: Some([0xc6, 0xd0, 0xf5]),
            bg: Some([0x30, 0x34, 0x46]),
        };
        let t = Theme::pick("terminal", frappe);
        assert_eq!(t.text, rgb(0xc6, 0xd0, 0xf5));
        assert_eq!(t.accent_fg, rgb(0x30, 0x34, 0x46));
        // The bar is the background tinted a little toward the foreground.
        assert_eq!(t.bar_bg, rgb(0x3c, 0x40, 0x54));
        // Semantic colors are the host palette's own.
        assert_eq!(
            (t.accent, t.warn, t.ok, t.error),
            (ansi(4), ansi(3), ansi(2), ansi(1))
        );
        // A name for a fixed theme ignores the host.
        assert_eq!(Theme::pick("dark", frappe), Theme::dark());
    }
}
