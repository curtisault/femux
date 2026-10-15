//! Where the client's chrome goes (REQ-UI-030, REQ-UI-042): the one bar at the top or the
//! bottom, the panel on the left, and the pane area the daemon sizes the view to.

use crate::config::{BarPosition, ClientConfig};

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
    /// The one bar: session chip, mode, tabs and modules in one row. Prompts, confirmations
    /// and messages use it too.
    pub bar: Area,
    /// The panel (REQ-UI-042), when open.
    pub panel: Option<Area>,
}

impl Layout {
    /// `panel`: the panel's width while it is open; never more than half the terminal.
    pub fn compute(cols: u16, rows: u16, cfg: &ClientConfig, panel: Option<u16>) -> Layout {
        let (mut top, mut bottom) = (0u16, rows);
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
        };
        let mut left = 0u16;
        let panel = panel.map(|w| {
            let w = w.min(cols / 2);
            let a = Area {
                x: 0,
                y: top,
                w,
                h: bottom.saturating_sub(top),
            };
            left = w;
            a
        });
        let panes = Area {
            x: left,
            y: top,
            w: cols.saturating_sub(left).max(1),
            h: bottom.saturating_sub(top).max(1),
        };
        Layout { panes, bar, panel }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(bar: BarPosition, panel: Option<u16>) -> Layout {
        let cfg = ClientConfig {
            bar,
            ..Default::default()
        };
        Layout::compute(100, 30, &cfg, panel)
    }

    #[test]
    fn ui_030_one_bar_at_the_top_or_bottom_and_the_panes_the_rest() {
        let l = at(BarPosition::Bottom, None);
        assert_eq!(
            l.bar,
            Area {
                x: 0,
                y: 29,
                w: 100,
                h: 1
            }
        );
        assert_eq!(
            l.panes,
            Area {
                x: 0,
                y: 0,
                w: 100,
                h: 29
            }
        );
        let l = at(BarPosition::Top, None);
        assert_eq!((l.bar.y, l.panes.y, l.panes.h), (0, 1, 29));
        // The panel sits left of the panes, beside the bar's row, at most half the width.
        let l = at(BarPosition::Top, Some(28));
        assert_eq!(
            l.panel.unwrap(),
            Area {
                x: 0,
                y: 1,
                w: 28,
                h: 29
            }
        );
        assert_eq!((l.panes.x, l.panes.w), (28, 72));
        assert_eq!(at(BarPosition::Top, Some(80)).panel.unwrap().w, 50);

        // Tiny terminals never underflow.
        let tiny = Layout::compute(3, 1, &ClientConfig::default(), Some(28));
        assert!(tiny.panes.w >= 1 && tiny.panes.h >= 1);
    }
}
