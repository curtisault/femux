//! Kitty graphics placements (REQ-UI-052): a placement arrives in pane cells; the client
//! offsets it to screen cells, clips it to the pane's rectangle and re-emits it as a Kitty
//! `a=p` command. Image data itself is the daemon's (GFX-*, 0.7.0); until then `frame.images`
//! is empty and this is the geometry the 0.7 client will use.

use femux_proto::msg as m;

/// A clipped placement in screen cells plus the part of the image it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub id: u32,
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    /// Cells of the image cut off at the left / top.
    pub skip_cols: u16,
    pub skip_rows: u16,
    /// Cells of the full placement.
    pub total_cols: u16,
    pub total_rows: u16,
}

/// Offsets `p` by the pane's screen rectangle and clips it to that rectangle.
pub fn place(p: &m::ImagePlacement, pane: &m::Rect) -> Option<Placed> {
    let x0 = pane.x as i32 + p.x;
    let y0 = pane.y as i32 + p.y;
    let x1 = x0 + p.cols as i32;
    let y1 = y0 + p.rows as i32;
    let cx0 = x0.max(pane.x as i32);
    let cy0 = y0.max(pane.y as i32);
    let cx1 = x1.min(pane.x as i32 + pane.w as i32);
    let cy1 = y1.min(pane.y as i32 + pane.h as i32);
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    Some(Placed {
        id: p.id,
        x: cx0 as u16,
        y: cy0 as u16,
        cols: (cx1 - cx0) as u16,
        rows: (cy1 - cy0) as u16,
        skip_cols: (cx0 - x0) as u16,
        skip_rows: (cy0 - y0) as u16,
        total_cols: p.cols,
        total_rows: p.rows,
    })
}

/// The escape sequence that shows `p`: move the cursor, then a Kitty put command with a
/// source crop when the image's pixel size is known (`px`), scaled otherwise. `C=1` keeps
/// the cursor where it is.
pub fn kitty_put(p: &Placed, px: Option<(u32, u32)>) -> String {
    let mut cmd = format!(
        "\x1b[{};{}H\x1b_Ga=p,i={},c={},r={},C=1",
        p.y + 1,
        p.x + 1,
        p.id,
        p.cols,
        p.rows
    );
    if let Some((w, h)) = px
        && (p.skip_cols > 0 || p.skip_rows > 0 || p.cols < p.total_cols || p.rows < p.total_rows)
    {
        let cw = w as f64 / p.total_cols.max(1) as f64;
        let ch = h as f64 / p.total_rows.max(1) as f64;
        cmd.push_str(&format!(
            ",x={},y={},w={},h={}",
            (p.skip_cols as f64 * cw).round() as u32,
            (p.skip_rows as f64 * ch).round() as u32,
            (p.cols as f64 * cw).round() as u32,
            (p.rows as f64 * ch).round() as u32
        ));
    }
    cmd.push_str("\x1b\\");
    cmd
}

/// Deletes every visible placement of image `id` (used before re-placing on a layout change).
pub fn kitty_delete(id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={id}\x1b\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(x: i32, y: i32, cols: u16, rows: u16) -> m::ImagePlacement {
        m::ImagePlacement {
            id: 7,
            pane_id: "P".into(),
            x,
            y,
            cols,
            rows,
        }
    }

    #[test]
    fn placements_are_offset_and_clipped() {
        let pane = m::Rect {
            x: 10,
            y: 2,
            w: 20,
            h: 5,
        };
        let inside = place(&pl(1, 1, 4, 2), &pane).unwrap();
        assert_eq!(
            (inside.x, inside.y, inside.cols, inside.rows),
            (11, 3, 4, 2)
        );
        assert_eq!((inside.skip_cols, inside.skip_rows), (0, 0));
        // Overhanging right and bottom edges are cut.
        let edge = place(&pl(15, 3, 10, 10), &pane).unwrap();
        assert_eq!((edge.x, edge.y, edge.cols, edge.rows), (25, 5, 5, 2));
        // Scrolled partly above the pane: the top rows are skipped.
        let above = place(&pl(0, -2, 6, 4), &pane).unwrap();
        assert_eq!((above.y, above.rows, above.skip_rows), (2, 2, 2));
        assert_eq!(place(&pl(0, -9, 6, 4), &pane), None);
        assert_eq!(place(&pl(20, 0, 6, 4), &pane), None);
        assert_eq!(place(&pl(18, 0, 6, 4), &pane).map(|p| p.cols), Some(2));
    }

    #[test]
    fn kitty_commands_carry_offsets_and_crops() {
        let pane = m::Rect {
            x: 10,
            y: 2,
            w: 20,
            h: 5,
        };
        let p = place(&pl(0, -2, 6, 4), &pane).unwrap();
        assert_eq!(
            kitty_put(&p, None),
            "\x1b[3;11H\x1b_Ga=p,i=7,c=6,r=2,C=1\x1b\\"
        );
        assert_eq!(
            kitty_put(&p, Some((60, 80))),
            "\x1b[3;11H\x1b_Ga=p,i=7,c=6,r=2,C=1,x=0,y=40,w=60,h=40\x1b\\"
        );
        assert_eq!(kitty_delete(7), "\x1b_Ga=d,d=i,i=7\x1b\\");
    }
}
