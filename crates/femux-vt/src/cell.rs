//! Decoding libghostty-vt cells into [`Cell`].

use std::ffi::c_void;

use smol_str::SmolStr;

use crate::{Cell, Color, Error, attrs, check, ffi};

pub(crate) fn new_style() -> ffi::GhosttyStyle {
    // SAFETY: GhosttyStyle is plain data; all-zero is a valid (default) value.
    let mut st: ffi::GhosttyStyle = unsafe { std::mem::zeroed() };
    st.size = std::mem::size_of::<ffi::GhosttyStyle>();
    st
}

unsafe fn cell_get<T>(
    raw: ffi::GhosttyCell,
    key: ffi::GhosttyCellData,
    out: &mut T,
) -> Result<(), Error> {
    check(unsafe { ffi::ghostty_cell_get(raw, key, out as *mut T as *mut c_void) })
}

/// Sets width and resets colors/attrs from a raw cell. Returns whether the cell has a style.
pub(crate) fn decode_raw(raw: ffi::GhosttyCell, out: &mut Cell) -> Result<bool, Error> {
    use ffi::GhosttyCellData as CD;
    let mut wide = ffi::GhosttyCellWide::GHOSTTY_CELL_WIDE_NARROW;
    let mut styled = false;
    // SAFETY: outputs match the documented types for these keys.
    unsafe {
        cell_get(raw, CD::GHOSTTY_CELL_DATA_WIDE, &mut wide)?;
        cell_get(raw, CD::GHOSTTY_CELL_DATA_HAS_STYLING, &mut styled)?;
    }
    out.width = match wide {
        ffi::GhosttyCellWide::GHOSTTY_CELL_WIDE_WIDE => 2,
        ffi::GhosttyCellWide::GHOSTTY_CELL_WIDE_SPACER_TAIL => 0,
        _ => 1,
    };
    out.fg = Color::Default;
    out.bg = Color::Default;
    out.ul = None;
    out.attrs = 0;
    Ok(styled)
}

/// The cell's text. `utf8_into` fills a GhosttyBuffer with the full grapheme as UTF-8.
pub(crate) fn text_from(
    raw: ffi::GhosttyCell,
    glen: u32,
    scratch: &mut Vec<u8>,
    utf8_into: impl Fn(&mut ffi::GhosttyBuffer) -> ffi::GhosttyResult,
) -> Result<SmolStr, Error> {
    if glen == 0 {
        return Ok(SmolStr::default());
    }
    if glen == 1 {
        let mut cp: u32 = 0;
        // SAFETY: codepoint output is a u32.
        unsafe {
            cell_get(
                raw,
                ffi::GhosttyCellData::GHOSTTY_CELL_DATA_CODEPOINT,
                &mut cp,
            )?
        };
        let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
        return Ok(SmolStr::new(ch.encode_utf8(&mut [0u8; 4])));
    }
    scratch.resize(scratch.capacity().max(32), 0);
    loop {
        let mut buf = ffi::GhosttyBuffer {
            ptr: scratch.as_mut_ptr(),
            cap: scratch.len(),
            len: 0,
        };
        let r = utf8_into(&mut buf);
        if r == ffi::GhosttyResult::GHOSTTY_OUT_OF_SPACE {
            scratch.resize(buf.len, 0);
            continue;
        }
        check(r)?;
        return Ok(SmolStr::new(String::from_utf8_lossy(&scratch[..buf.len])));
    }
}

pub(crate) fn apply_style(st: &ffi::GhosttyStyle, out: &mut Cell) {
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
    a |= ((st.underline.clamp(0, 7) as u16) << attrs::UNDERLINE_SHIFT) & attrs::UNDERLINE_MASK;
    out.attrs = a;
}

/// Background-only cells (erased with a colored background) carry the color in the cell.
pub(crate) fn apply_bg_content(raw: ffi::GhosttyCell, out: &mut Cell) -> Result<(), Error> {
    use ffi::GhosttyCellData as CD;
    let mut tag = ffi::GhosttyCellContentTag::GHOSTTY_CELL_CONTENT_CODEPOINT;
    // SAFETY: outputs match the documented types for these keys.
    unsafe {
        cell_get(raw, CD::GHOSTTY_CELL_DATA_CONTENT_TAG, &mut tag)?;
        if tag == ffi::GhosttyCellContentTag::GHOSTTY_CELL_CONTENT_BG_COLOR_PALETTE {
            let mut i: u8 = 0;
            cell_get(raw, CD::GHOSTTY_CELL_DATA_COLOR_PALETTE, &mut i)?;
            out.bg = Color::Indexed(i);
        } else if tag == ffi::GhosttyCellContentTag::GHOSTTY_CELL_CONTENT_BG_COLOR_RGB {
            let mut c = ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 };
            cell_get(raw, CD::GHOSTTY_CELL_DATA_COLOR_RGB, &mut c)?;
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
