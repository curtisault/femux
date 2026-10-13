//! Kitty Unicode placeholders (REQ-GFX-001): a program transmits an image with a virtual
//! placement (`U=1`) and prints U+10EEEE cells where it should appear; the foreground color
//! carries the image id, the underline color the placement id, and combining diacritics the
//! row and column of the image each cell shows. yazi and programs inside tmux draw images
//! this way. libghostty-vt stores the image but leaves rendering to the embedder, so this
//! finds the runs on screen; [`crate::Terminal::placements`] turns them into ordinary
//! placements. The logic follows Ghostty's `terminal/kitty/graphics_unicode.zig` (MIT).

use crate::{Color, Grid};

/// The placeholder character.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Kitty's row/column diacritics: the n-th entry encodes n
/// (https://sw.kovidgoyal.net/kitty/_downloads/f0a0de9ec8d9ff4456206db8e0814937/rowcolumn-diacritics.txt).
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030d, 0x030e, 0x0310, 0x0312, 0x033d, 0x033e, 0x033f, 0x0346, 0x034a, 0x034b, 0x034c,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035b, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036a, 0x036b, 0x036c, 0x036d, 0x036e, 0x036f, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059c, 0x059d, 0x059e, 0x059f, 0x05a0, 0x05a1,
    0x05a8, 0x05a9, 0x05ab, 0x05ac, 0x05af, 0x05c4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065a, 0x065b, 0x065d, 0x065e, 0x06d6, 0x06d7, 0x06d8,
    0x06d9, 0x06da, 0x06db, 0x06dc, 0x06df, 0x06e0, 0x06e1, 0x06e2, 0x06e4, 0x06e7, 0x06e8, 0x06eb,
    0x06ec, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073a, 0x073d, 0x073f, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074a, 0x07eb, 0x07ec, 0x07ed, 0x07ee, 0x07ef, 0x07f0, 0x07f1, 0x07f3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081b, 0x081c, 0x081d, 0x081e, 0x081f, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082a, 0x082b, 0x082c, 0x082d, 0x0951, 0x0953, 0x0954,
    0x0f82, 0x0f83, 0x0f86, 0x0f87, 0x135d, 0x135e, 0x135f, 0x17dd, 0x193a, 0x1a17, 0x1a75, 0x1a76,
    0x1a77, 0x1a78, 0x1a79, 0x1a7a, 0x1a7b, 0x1a7c, 0x1b6b, 0x1b6d, 0x1b6e, 0x1b6f, 0x1b70, 0x1b71,
    0x1b72, 0x1b73, 0x1cd0, 0x1cd1, 0x1cd2, 0x1cda, 0x1cdb, 0x1ce0, 0x1dc0, 0x1dc1, 0x1dc3, 0x1dc4,
    0x1dc5, 0x1dc6, 0x1dc7, 0x1dc8, 0x1dc9, 0x1dcb, 0x1dcc, 0x1dd1, 0x1dd2, 0x1dd3, 0x1dd4, 0x1dd5,
    0x1dd6, 0x1dd7, 0x1dd8, 0x1dd9, 0x1dda, 0x1ddb, 0x1ddc, 0x1ddd, 0x1dde, 0x1ddf, 0x1de0, 0x1de1,
    0x1de2, 0x1de3, 0x1de4, 0x1de5, 0x1de6, 0x1dfe, 0x20d0, 0x20d1, 0x20d4, 0x20d5, 0x20d6, 0x20d7,
    0x20db, 0x20dc, 0x20e1, 0x20e7, 0x20e9, 0x20f0, 0x2cef, 0x2cf0, 0x2cf1, 0x2de0, 0x2de1, 0x2de2,
    0x2de3, 0x2de4, 0x2de5, 0x2de6, 0x2de7, 0x2de8, 0x2de9, 0x2dea, 0x2deb, 0x2dec, 0x2ded, 0x2dee,
    0x2def, 0x2df0, 0x2df1, 0x2df2, 0x2df3, 0x2df4, 0x2df5, 0x2df6, 0x2df7, 0x2df8, 0x2df9, 0x2dfa,
    0x2dfb, 0x2dfc, 0x2dfd, 0x2dfe, 0x2dff, 0xa66f, 0xa67c, 0xa67d, 0xa6f0, 0xa6f1, 0xa8e0, 0xa8e1,
    0xa8e2, 0xa8e3, 0xa8e4, 0xa8e5, 0xa8e6, 0xa8e7, 0xa8e8, 0xa8e9, 0xa8ea, 0xa8eb, 0xa8ec, 0xa8ed,
    0xa8ee, 0xa8ef, 0xa8f0, 0xa8f1, 0xaab0, 0xaab2, 0xaab3, 0xaab7, 0xaab8, 0xaabe, 0xaabf, 0xaac1,
    0xfe20, 0xfe21, 0xfe22, 0xfe23, 0xfe24, 0xfe25, 0xfe26, 0x10a0f, 0x10a38, 0x1d185, 0x1d186,
    0x1d187, 0x1d188, 0x1d189, 0x1d1aa, 0x1d1ab, 0x1d1ac, 0x1d1ad, 0x1d242, 0x1d243, 0x1d244,
];

fn index(c: char) -> Option<u32> {
    DIACRITICS.binary_search(&(c as u32)).ok().map(|i| i as u32)
}

/// An id from a color: palette index or 24-bit RGB.
fn color_id(c: Color) -> u32 {
    match c {
        Color::Default => 0,
        Color::Indexed(v) => v as u32,
        Color::Rgb(r, g, b) => (r as u32) << 16 | (g as u32) << 8 | b as u32,
    }
}

/// A run of placeholder cells on one row showing consecutive columns of one image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Screen cell of the first placeholder.
    pub x: u16,
    pub y: u16,
    pub image_id: u32,
    pub placement_id: u32,
    /// The image row and first image column it shows, in placement cells.
    pub row: u32,
    pub col: u32,
    pub width: u32,
}

#[derive(Debug, Clone)]
struct Cell {
    x: u16,
    low: u32,
    high: Option<u32>,
    pid: Option<u32>,
    row: Option<u32>,
    col: Option<u32>,
    width: u32,
}

impl Cell {
    fn can_append(&self, o: &Cell) -> bool {
        self.low == o.low
            && self.pid == o.pid
            && (o.row.is_none() || o.row == self.row)
            && (o.col.is_none() || o.col == self.col.map(|c| c + self.width))
            && (o.high.is_none() || o.high == self.high)
    }

    fn complete(&self, y: u16) -> Run {
        Run {
            x: self.x,
            y,
            image_id: self.low | self.high.unwrap_or(0) << 24,
            placement_id: self.pid.unwrap_or(0),
            row: self.row.unwrap_or(0),
            col: self.col.unwrap_or(0),
            width: self.width,
        }
    }
}

/// Every placeholder run on the screen.
pub fn runs(grid: &Grid) -> Vec<Run> {
    let mut out = Vec::new();
    for (y, row) in grid.rows.iter().enumerate() {
        let y = y as u16;
        let mut run: Option<Cell> = None;
        for (x, cell) in row.cells.iter().enumerate() {
            let mut chars = cell.text.chars();
            if chars.next() != Some(PLACEHOLDER) {
                if let Some(r) = run.take() {
                    out.push(r.complete(y));
                }
                continue;
            }
            let marks: Vec<char> = chars.collect();
            let pid = cell.ul.map(color_id).filter(|id| *id != 0);
            let cur = Cell {
                x: x as u16,
                low: color_id(cell.fg),
                high: marks.get(2).and_then(|c| index(*c)).filter(|h| *h < 256),
                pid,
                row: marks.first().and_then(|c| index(*c)),
                col: marks.get(1).and_then(|c| index(*c)),
                width: 1,
            };
            match &mut run {
                Some(prev) if prev.can_append(&cur) => prev.width += 1,
                _ => {
                    if let Some(r) = run.take() {
                        out.push(r.complete(y));
                    }
                    let mut first = cur;
                    first.row.get_or_insert(0);
                    first.col.get_or_insert(0);
                    run = Some(first);
                }
            }
        }
        if let Some(r) = run {
            out.push(r.complete(y));
        }
    }
    out
}

/// x, y, width, height in image pixels.
pub type SourceRect = (u32, u32, u32, u32);

/// Where a run shows its part of an image whose placement spans `grid` cells: the source
/// rectangle (px), the pixel offset in the first cell, or `None` when it shows no pixels.
/// The image is fitted into the placement keeping its aspect ratio, centered.
pub fn render(
    run: &Run,
    img: (u32, u32),
    grid: (u32, u32),
    cell: (u32, u32),
) -> Option<(SourceRect, (u32, u32))> {
    let (iw, ih) = (img.0 as f64, img.1 as f64);
    let (gc, gr) = (grid.0.max(1) as f64, grid.1.max(1) as f64);
    let (cw, ch) = (cell.0 as f64, cell.1 as f64);
    let (rows_px, cols_px) = (gr * ch, gc * cw);
    // Fit, center.
    let (xs, ys, xo, yo) = if iw * rows_px > ih * cols_px {
        let s = cols_px / iw.max(1.0);
        (s, s, 0.0, (rows_px - ih * s) / 2.0)
    } else {
        let s = rows_px / ih.max(1.0);
        (s, s, (cols_px - iw * s) / 2.0, 0.0)
    };
    let (sxo, syo) = (xo / xs, yo / ys);
    let (sw, sh) = (iw + sxo * 2.0, ih + syo * 2.0);
    let (vw, vh) = (run.width as f64, 1.0);
    let mut src = (
        sw * (run.col as f64 / gc),
        sh * (run.row as f64 / gr),
        sw * (vw / gc),
        sh * (vh / gr),
    );
    let (mut dx, mut dy) = (0.0, 0.0);
    if src.1 < syo {
        let off = syo - src.1;
        src.3 -= off;
        dy = off;
        src.1 = 0.0;
        src.3 = src.3.min(ih);
    } else if src.1 + src.3 > sh - syo {
        src.1 -= syo;
        src.3 = sh - syo - src.1 - syo;
    } else {
        src.1 -= syo;
    }
    if src.0 < sxo {
        let off = sxo - src.0;
        src.2 -= off;
        dx = off;
        src.0 = 0.0;
        src.2 = src.2.min(iw);
    } else if src.0 + src.2 > sw - sxo {
        src.0 -= sxo;
        src.2 = sw - sxo - src.0 - sxo;
    } else {
        src.0 -= sxo;
    }
    if src.2 <= 0.0 || src.3 <= 0.0 {
        return None;
    }
    let r = |v: f64| v.round().max(0.0) as u32;
    Some((
        (r(src.0), r(src.1), r(src.2), r(src.3)),
        (r(dx * xs), r(dy * ys)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_complete() {
        assert!(DIACRITICS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(index('\u{0305}'), Some(0));
        assert_eq!(index('\u{030D}'), Some(1));
        assert_eq!(index('a'), None);
    }

    #[test]
    fn a_square_image_fills_a_square_grid() {
        // A 20x40 image over 2x2 cells of 10x20: the second row's two cells show its bottom
        // half.
        let run = Run {
            x: 0,
            y: 0,
            image_id: 1,
            placement_id: 0,
            row: 1,
            col: 0,
            width: 2,
        };
        let (src, off) = render(&run, (20, 40), (2, 2), (10, 20)).unwrap();
        assert_eq!((src, off), ((0, 20, 20, 20), (0, 0)));
    }
}
