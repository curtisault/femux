//! Answers to terminal queries from pane programs (REQ-QRY-001/002).
//!
//! libghostty-vt parses every query and answers mode reports (DECRQM), the Kitty keyboard
//! query and color queries (OSC 10/11/12) itself; we supply the facts: the host terminal's
//! colors, femux's identity for DA and XTVERSION, the color scheme and the cell size.

use std::ffi::c_void;

use crate::{Effects, Error, Terminal, effects, ffi};

/// What femux reports about the host terminal. Defaults are conservative: a dark scheme with
/// a light-gray-on-black palette, used when no client has reported its terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCaps {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub cursor: Option<(u8, u8, u8)>,
}

impl Default for HostCaps {
    fn default() -> Self {
        HostCaps {
            fg: (0xd0, 0xd0, 0xd0),
            bg: (0, 0, 0),
            cursor: None,
        }
    }
}

impl HostCaps {
    fn dark(&self) -> bool {
        let (r, g, b) = self.bg;
        // Rec. 709 luma.
        (2126 * r as u32 + 7152 * g as u32 + 722 * b as u32) < 128 * 10_000
    }
}

unsafe extern "C" fn on_xtversion(_t: ffi::GhosttyTerminal, ud: *mut c_void) -> ffi::GhosttyString {
    let fx = effects(ud);
    ffi::GhosttyString {
        ptr: fx.xtversion.as_ptr(),
        len: fx.xtversion.len(),
    }
}

unsafe extern "C" fn on_device_attributes(
    _t: ffi::GhosttyTerminal,
    _ud: *mut c_void,
    out: *mut ffi::GhosttyDeviceAttributes,
) -> bool {
    // VT220 with ANSI color: what femux implements regardless of the host terminal.
    // Sixel (4) is not advertised until femux passes sixel through (0.7.0).
    let out = unsafe { &mut *out };
    out.primary.conformance_level = 62;
    out.primary.features = [0; 64];
    out.primary.features[0] = 22;
    out.primary.num_features = 1;
    out.secondary.device_type = 1;
    out.secondary.firmware_version = version_number();
    out.secondary.rom_cartridge = 0;
    out.tertiary.unit_id = 0x6665_6d78; // "femx"
    true
}

unsafe extern "C" fn on_color_scheme(
    _t: ffi::GhosttyTerminal,
    ud: *mut c_void,
    out: *mut ffi::GhosttyColorScheme,
) -> bool {
    let dark = effects(ud).host.dark();
    unsafe {
        *out = if dark {
            ffi::GhosttyColorScheme::GHOSTTY_COLOR_SCHEME_DARK
        } else {
            ffi::GhosttyColorScheme::GHOSTTY_COLOR_SCHEME_LIGHT
        };
    }
    true
}

unsafe extern "C" fn on_size(
    _t: ffi::GhosttyTerminal,
    ud: *mut c_void,
    out: *mut ffi::GhosttySizeReportSize,
) -> bool {
    let fx: &Effects = effects(ud);
    unsafe {
        *out = ffi::GhosttySizeReportSize {
            rows: fx.size.1,
            columns: fx.size.0,
            cell_width: fx.cell_px.0,
            cell_height: fx.cell_px.1,
        };
    }
    true
}

/// `0.1.0` -> 100, for DA2's firmware version.
fn version_number() -> u16 {
    let v: Vec<u16> = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    v[0] * 10_000 + v[1] * 100 + v[2]
}

fn rgb((r, g, b): (u8, u8, u8)) -> ffi::GhosttyColorRgb {
    ffi::GhosttyColorRgb { r, g, b }
}

impl Terminal {
    pub(crate) unsafe fn install_query_callbacks(&mut self) -> Result<(), Error> {
        self.effects.xtversion = format!("femux {}", env!("CARGO_PKG_VERSION"));
        use ffi::GhosttyTerminalOption as O;
        let callbacks: [(O, *const c_void); 4] = [
            (
                O::GHOSTTY_TERMINAL_OPT_XTVERSION,
                on_xtversion as *const c_void,
            ),
            (
                O::GHOSTTY_TERMINAL_OPT_DEVICE_ATTRIBUTES,
                on_device_attributes as *const c_void,
            ),
            (
                O::GHOSTTY_TERMINAL_OPT_COLOR_SCHEME,
                on_color_scheme as *const c_void,
            ),
            (O::GHOSTTY_TERMINAL_OPT_SIZE, on_size as *const c_void),
        ];
        for (opt, f) in callbacks {
            unsafe { self.set_opt(opt, f)? };
        }
        Ok(())
    }

    /// Updates what queries report about the host terminal (REQ-QRY-002).
    pub fn set_host(&mut self, caps: &HostCaps) -> Result<(), Error> {
        // Setting colors marks the whole screen dirty; skip it when nothing changed.
        if self.host_set && self.effects.host == *caps {
            return Ok(());
        }
        self.host_set = true;
        use ffi::GhosttyTerminalOption as O;
        let (fg, bg) = (rgb(caps.fg), rgb(caps.bg));
        let cursor = caps.cursor.map(rgb);
        // SAFETY: values are valid for the duration of each call; NULL resets the cursor color.
        unsafe {
            self.set_opt(
                O::GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND,
                &fg as *const _ as *const c_void,
            )?;
            self.set_opt(
                O::GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND,
                &bg as *const _ as *const c_void,
            )?;
            let c = cursor
                .as_ref()
                .map_or(std::ptr::null(), |c| c as *const _ as *const c_void);
            self.set_opt(O::GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, c)?;
        }
        self.effects.host = caps.clone();
        Ok(())
    }

    /// Sets the pixel size of one cell, for pixel-based reports and mouse encoding.
    pub fn set_cell_px(&mut self, w: u32, h: u32) -> Result<(), Error> {
        self.effects.cell_px = (w.max(1), h.max(1));
        let (cols, rows) = self.size();
        self.resize(cols, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(t: &mut Terminal, q: &[u8]) -> String {
        t.write(q);
        String::from_utf8(t.take_pty_output()).unwrap()
    }

    #[test]
    fn answers_identity_queries() {
        let mut t = Terminal::new(80, 24, 100).unwrap();
        assert_eq!(ask(&mut t, b"\x1b[c"), "\x1b[?62;22c");
        assert_eq!(
            ask(&mut t, b"\x1b[>c"),
            format!("\x1b[>1;{};0c", version_number())
        );
        assert_eq!(
            ask(&mut t, b"\x1b[>q"),
            format!("\x1bP>|femux {}\x1b\\", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn answers_from_host_caps() {
        let mut t = Terminal::new(80, 24, 100).unwrap();
        // Conservative defaults with no client attached.
        assert_eq!(
            ask(&mut t, b"\x1b]11;?\x1b\\"),
            "\x1b]11;rgb:0000/0000/0000\x1b\\"
        );
        assert_eq!(ask(&mut t, b"\x1b[?996n"), "\x1b[?997;1n");
        t.set_host(&HostCaps {
            fg: (0x10, 0x20, 0x30),
            bg: (0xff, 0xff, 0xf0),
            cursor: Some((1, 2, 3)),
        })
        .unwrap();
        assert_eq!(
            ask(&mut t, b"\x1b]10;?\x1b\\"),
            "\x1b]10;rgb:1010/2020/3030\x1b\\"
        );
        assert_eq!(
            ask(&mut t, b"\x1b]11;?\x1b\\"),
            "\x1b]11;rgb:ffff/ffff/f0f0\x1b\\"
        );
        assert_eq!(
            ask(&mut t, b"\x1b]12;?\x1b\\"),
            "\x1b]12;rgb:0101/0202/0303\x1b\\"
        );
        assert_eq!(ask(&mut t, b"\x1b[?996n"), "\x1b[?997;2n");
    }

    #[test]
    fn answers_mode_and_keyboard_queries() {
        let mut t = Terminal::new(80, 24, 100).unwrap();
        for mode in [2026, 2031, 2004, 1006] {
            let a = ask(&mut t, format!("\x1b[?{mode}$p").as_bytes());
            assert!(
                a.starts_with(&format!("\x1b[?{mode};")) && a.ends_with("$y"),
                "{mode}: {a:?}"
            );
        }
        assert_eq!(ask(&mut t, b"\x1b[?2004h\x1b[?2004$p"), "\x1b[?2004;1$y");
        assert_eq!(ask(&mut t, b"\x1b[?u"), "\x1b[?0u");
        assert_eq!(ask(&mut t, b"\x1b[>5u\x1b[?u"), "\x1b[?5u");
        assert_eq!(ask(&mut t, b"\x1b[14t"), "\x1b[4;480;800t");
    }
}
