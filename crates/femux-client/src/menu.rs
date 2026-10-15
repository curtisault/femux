//! The context menu (REQ-UI-046): a right click on a pane or a tab opens a small menu of
//! actions for it at the pointer; `enter` or a click runs the highlighted item, `esc` or a
//! click elsewhere closes it.

use crate::bindings::Action;
use crate::render::{Canvas, attr};
use crate::theme::Theme;

/// What an item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// The same action as its key, on the focused pane or tab.
    Run(Action),
    /// Toggle sending right clicks to this pane instead of opening the menu.
    RightClicks(String),
    /// Rename this tab.
    RenameTab(String),
    /// Close this tab, confirming when a pane of it runs a program.
    CloseTab(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub act: Act,
}

/// An open menu: its items, the highlighted one, and where its top-left corner is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    pub items: Vec<Item>,
    pub selected: usize,
    pub x: u16,
    pub y: u16,
}

/// What the pane menu needs to know about the pane.
pub struct PaneFacts {
    pub id: String,
    pub zoomed: bool,
    pub floating: bool,
    pub sending: bool,
}

/// The pane menu's items, in the spec's order.
pub fn pane_items(p: &PaneFacts) -> Vec<Item> {
    let run = |label: &str, a: Action| Item {
        label: label.into(),
        act: Act::Run(a),
    };
    vec![
        run("split right", Action::SplitRight),
        run("split below", Action::SplitDown),
        run(if p.zoomed { "unzoom" } else { "zoom" }, Action::Zoom),
        run(
            if p.floating { "tile" } else { "float" },
            Action::FloatToggle,
        ),
        run("rename tab", Action::RenameTab),
        run("mark pane", Action::SetMark),
        Item {
            label: if p.sending {
                "open the menu on right-click".into()
            } else {
                "send right-clicks to this pane".into()
            },
            act: Act::RightClicks(p.id.clone()),
        },
        run("close pane", Action::ClosePane),
    ]
}

/// The tab menu's items.
pub fn tab_items(tab: &str) -> Vec<Item> {
    vec![
        Item {
            label: "new tab".into(),
            act: Act::Run(Action::NewTab),
        },
        Item {
            label: "rename tab".into(),
            act: Act::RenameTab(tab.to_owned()),
        },
        Item {
            label: "close tab".into(),
            act: Act::CloseTab(tab.to_owned()),
        },
    ]
}

impl MenuState {
    /// Opens at the pointer `(x, y)`, moved only as far as keeping it whole inside a terminal
    /// of `cols`×`rows` needs.
    pub fn open(items: Vec<Item>, x: u16, y: u16, cols: u16, rows: u16) -> MenuState {
        let mut m = MenuState {
            items,
            selected: 0,
            x,
            y,
        };
        let (w, h) = m.size();
        m.x = x.min(cols.saturating_sub(w));
        m.y = y.min(rows.saturating_sub(h));
        m
    }

    /// Width and height with the border: one space either side of the longest label.
    pub fn size(&self) -> (u16, u16) {
        let longest = self
            .items
            .iter()
            .map(|i| i.label.chars().count())
            .max()
            .unwrap_or(0);
        (longest as u16 + 4, self.items.len() as u16 + 2)
    }

    /// The item under `(x, y)`, if any.
    pub fn item_at(&self, x: u16, y: u16) -> Option<usize> {
        let (w, h) = self.size();
        let inside = x > self.x && x < self.x + w - 1 && y > self.y && y < self.y + h - 1;
        inside.then(|| (y - self.y - 1) as usize)
    }

    /// Whether `(x, y)` is on the menu, border included.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        let (w, h) = self.size();
        x >= self.x && x < self.x + w && y >= self.y && y < self.y + h
    }

    pub fn down(&mut self) {
        self.selected = (self.selected + 1) % self.items.len().max(1);
    }

    pub fn up(&mut self) {
        let n = self.items.len().max(1);
        self.selected = (self.selected + n - 1) % n;
    }
}

pub(crate) fn draw(st: &MenuState, c: &mut Canvas, theme: &Theme) {
    let (w, h) = st.size();
    let (x0, y0, w, h) = (st.x as usize, st.y as usize, w as usize, h as usize);
    let bg = theme.overlay_bg;
    let frame = attr(theme.accent, bg);
    c.fill(x0 + 1, y0, w - 2, '─', &frame);
    c.fill(x0 + 1, y0 + h - 1, w - 2, '─', &frame);
    c.put(x0, y0, "┌", &frame);
    c.put(x0 + w - 1, y0, "┐", &frame);
    c.put(x0, y0 + h - 1, "└", &frame);
    c.put(x0 + w - 1, y0 + h - 1, "┘", &frame);
    for (i, item) in st.items.iter().enumerate() {
        let y = y0 + 1 + i;
        let rbg = if i == st.selected {
            theme.selection
        } else {
            bg
        };
        c.put(x0, y, "│", &frame);
        c.fill(x0 + 1, y, w - 2, ' ', &attr(theme.text, rbg));
        c.put(x0 + 2, y, &item.label, &attr(theme.text, rbg));
        c.put(x0 + w - 1, y, "│", &frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(zoomed: bool, floating: bool, sending: bool) -> PaneFacts {
        PaneFacts {
            id: "P".into(),
            zoomed,
            floating,
            sending,
        }
    }

    #[test]
    fn ui_046_items_follow_the_pane() {
        let labels =
            |p: &PaneFacts| -> Vec<String> { pane_items(p).into_iter().map(|i| i.label).collect() };
        assert_eq!(
            labels(&facts(false, false, false)),
            [
                "split right",
                "split below",
                "zoom",
                "float",
                "rename tab",
                "mark pane",
                "send right-clicks to this pane",
                "close pane"
            ]
        );
        let l = labels(&facts(true, true, true));
        assert_eq!(l[2], "unzoom");
        assert_eq!(l[3], "tile");
        assert_eq!(l[6], "open the menu on right-click");
        let t: Vec<String> = tab_items("T").into_iter().map(|i| i.label).collect();
        assert_eq!(t, ["new tab", "rename tab", "close tab"]);
    }

    #[test]
    fn ui_046_the_menu_opens_at_the_pointer_and_stays_whole() {
        let m = MenuState::open(tab_items("T"), 10, 5, 80, 24);
        assert_eq!((m.x, m.y), (10, 5));
        assert_eq!(m.size(), (14, 5));
        // Near the bottom-right corner it moves only as far as it must.
        let m = MenuState::open(tab_items("T"), 75, 22, 80, 24);
        assert_eq!((m.x, m.y), (66, 19));
        assert_eq!(m.item_at(68, 20), Some(0));
        assert_eq!(m.item_at(68, 22), Some(2));
        assert_eq!(m.item_at(66, 20), None, "the border");
        assert!(m.contains(66, 19) && !m.contains(65, 19));
        let mut m = m;
        m.up();
        assert_eq!(m.selected, 2);
        m.down();
        assert_eq!(m.selected, 0);
    }
}
