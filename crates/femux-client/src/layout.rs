//! Where the client's chrome goes (REQ-UI-030, REQ-UI-042): the info bar on one of four sides,
//! the side nav, the tab strip, and the pane area the daemon sizes the view to.

use crate::config::{BarPosition, ClientConfig, TabsPosition};

/// Width of a `left`/`right` info bar.
pub const BAR_WIDTH: u16 = 24;
/// Width of the side nav.
pub const SIDENAV_WIDTH: u16 = 26;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Area {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The panes; its size is the view size.
    pub panes: Area,
    pub bar: Area,
    pub bar_vertical: bool,
    /// The tab strip row: `(x, y, width)`.
    pub strip: Option<(u16, u16, u16)>,
    pub sidenav: Option<Area>,
    /// The row for prompts, confirmations and messages: the bar itself when it is a row, else
    /// the bottom row of the pane area (drawn over it only while something is shown).
    pub status: (u16, u16, u16),
}

impl Layout {
    pub fn compute(cols: u16, rows: u16, cfg: &ClientConfig, sidenav: bool) -> Layout {
        let (mut top, mut bottom, mut left, mut right) = (0u16, rows, 0u16, cols);
        let bar_w = BAR_WIDTH.min(cols / 2);
        let bar = match cfg.bar {
            BarPosition::Top => {
                top = 1.min(rows);
                Area {
                    x: 0,
                    y: 0,
                    w: cols,
                    h: 1.min(rows),
                }
            }
            BarPosition::Bottom => {
                bottom = rows.saturating_sub(1);
                Area {
                    x: 0,
                    y: bottom,
                    w: cols,
                    h: 1.min(rows),
                }
            }
            BarPosition::Left => {
                left = bar_w;
                Area {
                    x: 0,
                    y: 0,
                    w: bar_w,
                    h: rows,
                }
            }
            BarPosition::Right => {
                right = cols.saturating_sub(bar_w);
                Area {
                    x: right,
                    y: 0,
                    w: bar_w,
                    h: rows,
                }
            }
        };
        let sidenav = sidenav.then(|| {
            let w = SIDENAV_WIDTH.min(right.saturating_sub(left) / 2);
            let a = Area {
                x: left,
                y: top,
                w,
                h: bottom.saturating_sub(top),
            };
            left += w;
            a
        });
        let width = right.saturating_sub(left);
        let strip = match cfg.tabs {
            TabsPosition::Top if bottom > top => {
                top += 1;
                Some((left, top - 1, width))
            }
            TabsPosition::Bottom if bottom > top => {
                bottom -= 1;
                Some((left, bottom, width))
            }
            _ => None,
        };
        let panes = Area {
            x: left,
            y: top,
            w: width.max(1),
            h: bottom.saturating_sub(top).max(1),
        };
        let bar_vertical = cfg.bar.vertical();
        let status = if bar_vertical {
            (panes.x, panes.y + panes.h - 1, panes.w)
        } else {
            (bar.x, bar.y, bar.w)
        };
        Layout {
            panes,
            bar,
            bar_vertical,
            strip,
            sidenav,
            status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(bar: BarPosition, tabs: TabsPosition, sidenav: bool) -> Layout {
        let cfg = ClientConfig {
            bar,
            tabs,
            ..Default::default()
        };
        Layout::compute(100, 30, &cfg, sidenav)
    }

    #[test]
    fn ui_030_the_bar_takes_one_side_and_the_panes_the_rest() {
        let l = at(BarPosition::Bottom, TabsPosition::Top, false);
        assert_eq!(
            l.bar,
            Area {
                x: 0,
                y: 29,
                w: 100,
                h: 1
            }
        );
        assert_eq!(l.strip, Some((0, 0, 100)));
        assert_eq!(
            l.panes,
            Area {
                x: 0,
                y: 1,
                w: 100,
                h: 28
            }
        );
        assert_eq!(l.status, (0, 29, 100));
        let l = at(BarPosition::Top, TabsPosition::Top, false);
        assert_eq!(
            (l.bar.y, l.strip, l.panes.y, l.panes.h),
            (0, Some((0, 1, 100)), 2, 28)
        );
        // Left and right bars are columns; the tab strip and panes sit beside them.
        let l = at(BarPosition::Left, TabsPosition::Top, false);
        assert!(l.bar_vertical);
        assert_eq!(
            l.bar,
            Area {
                x: 0,
                y: 0,
                w: BAR_WIDTH,
                h: 30
            }
        );
        assert_eq!(l.strip, Some((BAR_WIDTH, 0, 100 - BAR_WIDTH)));
        assert_eq!(
            l.panes,
            Area {
                x: BAR_WIDTH,
                y: 1,
                w: 100 - BAR_WIDTH,
                h: 29
            }
        );
        assert_eq!(l.status, (BAR_WIDTH, 29, 100 - BAR_WIDTH));
        let l = at(BarPosition::Right, TabsPosition::Bottom, false);
        assert_eq!(l.bar.x, 100 - BAR_WIDTH);
        assert_eq!(l.strip, Some((0, 29, 100 - BAR_WIDTH)));
        assert_eq!(
            l.panes,
            Area {
                x: 0,
                y: 0,
                w: 100 - BAR_WIDTH,
                h: 29
            }
        );
        // The side nav sits between a left bar and the panes.
        let l = at(BarPosition::Left, TabsPosition::Hidden, true);
        assert_eq!(l.sidenav.unwrap().x, BAR_WIDTH);
        assert_eq!(l.panes.x, BAR_WIDTH + SIDENAV_WIDTH);
        // Tiny terminals never underflow.
        let tiny = Layout::compute(3, 1, &ClientConfig::default(), true);
        assert!(tiny.panes.w >= 1 && tiny.panes.h >= 1);
    }
}
