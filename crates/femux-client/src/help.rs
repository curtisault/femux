//! Help (REQ-UI-045, `?`): every bound key of every table, grouped like the which-key panel,
//! with what the prefix does, in a scrolling, filterable box over the panes. [`Help::build`]
//! is also what writes `docs/prefixes.md`, so the two never drift.

use femux_plugin::Table;

use crate::bindings::{Action, Bindings};
use crate::keys::Chord;
use crate::render::{Canvas, attr, bold};
use crate::theme::Theme;

/// One row: the chords, and the action and plugin they share.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub keys: Vec<Chord>,
    pub action: Action,
    pub plugin: Option<&'static str>,
}

fn parameterized(a: Action) -> bool {
    matches!(
        a,
        Action::Focus(_) | Action::Resize(_) | Action::Preselect(_) | Action::Tab(_)
    )
}

impl Row {
    /// Keys that differ only by direction or number share a row.
    pub fn folded(&self) -> bool {
        self.keys.len() > 1 && parameterized(self.action)
    }

    /// `1-9` for the tab numbers; otherwise the keys, space-separated, and a run of one
    /// modifier over single letters as `ctrl+h/j/k/l`.
    pub fn key_text(&self) -> String {
        let names: Vec<String> = self.keys.iter().map(|k| k.to_string()).collect();
        if names.len() == 9
            && names
                .iter()
                .enumerate()
                .all(|(i, n)| *n == (i + 1).to_string())
        {
            return "1-9".into();
        }
        if self.folded()
            && let Some((m, _)) = names[0].rsplit_once('+')
            && names.iter().all(|n| {
                n.rsplit_once('+')
                    .is_some_and(|(nm, k)| nm == m && k.chars().count() == 1)
            })
        {
            let ks: Vec<&str> = names
                .iter()
                .map(|n| n.rsplit_once('+').unwrap().1)
                .collect();
            return format!("{m}+{}", ks.join("/"));
        }
        names.join(" ")
    }

    /// The action's name; `focus <direction>` for a folded row.
    pub fn action_text(&self) -> String {
        let name = self.action.name();
        if self.folded() {
            let (head, _) = name.rsplit_once(' ').unwrap_or((&name, ""));
            let what = if matches!(self.action, Action::Tab(_)) {
                "n"
            } else {
                "direction"
            };
            return format!("{head} <{what}>");
        }
        name
    }

    /// What the row does.
    pub fn does(&self) -> String {
        if self.folded() {
            return match self.action {
                Action::Focus(_) => "focus the pane in that direction".into(),
                Action::Resize(_) => "grow the pane in that direction".into(),
                Action::Preselect(_) => "put the next split on that side".into(),
                _ => "select that tab".into(),
            };
        }
        self.action.describe()
    }
}

/// Chords for the same action share a row, as do adjacent ones of a family (`h j k l`, `1-9`).
pub fn rows(
    list: &[(Chord, Action)],
    plugin_of: impl Fn(&Chord) -> Option<&'static str>,
) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for (c, a) in list {
        let plugin = plugin_of(c);
        if let Some(same) = out
            .iter_mut()
            .find(|r| r.action == *a && r.plugin == plugin)
        {
            same.keys.push(c.clone());
            continue;
        }
        if let Some(last) = out.last_mut()
            && last.plugin == plugin
            && parameterized(*a)
            && std::mem::discriminant(&last.action) == std::mem::discriminant(a)
        {
            last.keys.push(c.clone());
            continue;
        }
        out.push(Row {
            keys: vec![c.clone()],
            action: *a,
            plugin,
        });
    }
    out
}

/// Every table, folded and grouped.
#[derive(Debug, Clone, PartialEq)]
pub struct Help {
    pub root: Vec<Row>,
    /// The prefix table by which-key group, groups in first-seen order.
    pub prefix: Vec<(&'static str, Vec<Row>)>,
    pub copy: Vec<Row>,
}

impl Help {
    /// `plugin_of` names the plugin that bound a chord, for the docs; the overlay passes none.
    pub fn build(b: &Bindings, plugin_of: impl Fn(Table, &Chord) -> Option<&'static str>) -> Help {
        let prefix_rows = rows(&b.prefix, |c| plugin_of(Table::Prefix, c));
        let mut prefix: Vec<(&'static str, Vec<Row>)> = Vec::new();
        for r in prefix_rows {
            let g = r.action.group();
            match prefix.iter_mut().find(|(n, _)| *n == g) {
                Some((_, v)) => v.push(r),
                None => prefix.push((g, vec![r])),
            }
        }
        Help {
            root: rows(&b.root, |c| plugin_of(Table::Root, c)),
            prefix,
            copy: rows(&b.copy, |c| plugin_of(Table::Copy, c)),
        }
    }

    /// The overlay's lines, filtered by `filter` (key, action name or description).
    pub fn lines(&self, filter: &str) -> Vec<Line> {
        let f = filter.to_lowercase();
        let keep = |key: &str, action: &str, does: &str| {
            f.is_empty()
                || key.to_lowercase().contains(&f)
                || action.to_lowercase().contains(&f)
                || does.to_lowercase().contains(&f)
        };
        let item = |r: &Row| Line::Row {
            key: r.key_text(),
            does: r.does(),
        };
        let mut out: Vec<Line> = Vec::new();
        let section = |title: &str, groups: Vec<(Option<&str>, Vec<Line>)>, out: &mut Vec<Line>| {
            let groups: Vec<_> = groups.into_iter().filter(|(_, v)| !v.is_empty()).collect();
            if groups.is_empty() {
                return;
            }
            if !out.is_empty() {
                out.push(Line::Blank);
            }
            out.push(Line::Header(title.to_owned()));
            for (g, v) in groups {
                if let Some(g) = g {
                    out.push(Line::Blank);
                    out.push(Line::Group(g.to_owned()));
                }
                out.extend(v);
            }
        };
        let how: Vec<Line> = HOW
            .iter()
            .filter(|(k, d)| keep(k, "", d))
            .map(|(k, d)| Line::Row {
                key: (*k).to_owned(),
                does: (*d).to_owned(),
            })
            .collect();
        section("how the prefix works", vec![(None, how)], &mut out);
        let groups = self
            .prefix
            .iter()
            .map(|(g, rs)| {
                let v = rs
                    .iter()
                    .filter(|r| keep(&r.key_text(), &r.action_text(), &r.does()))
                    .map(item)
                    .collect();
                (Some(*g), v)
            })
            .collect();
        section("prefix table", groups, &mut out);
        let copy = self
            .copy
            .iter()
            .filter(|r| keep(&r.key_text(), &r.action_text(), &r.does()))
            .map(item)
            .collect();
        section("copy mode", vec![(None, copy)], &mut out);
        let root = self
            .root
            .iter()
            .filter(|r| keep(&r.key_text(), &r.action_text(), &r.does()))
            .map(item)
            .collect();
        section("root table", vec![(None, root)], &mut out);
        out
    }
}

/// The `how the prefix works` rows.
pub const HOW: [(&str, &str); 4] = [
    ("prefix", "then a key from the prefix table below"),
    ("prefix prefix", "send the prefix to the pane"),
    ("prefix esc", "cancel"),
    ("prefix space", "keep the table with no prefix until esc"),
];

/// One line of the overlay's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Blank,
    /// A section: `prefix table`, `copy mode`, …
    Header(String),
    /// A which-key group inside the prefix table.
    Group(String),
    Row {
        key: String,
        does: String,
    },
}

/// An open help overlay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelpState {
    /// The first body line shown.
    pub scroll: usize,
    pub filter: String,
    pub filtering: bool,
}

/// What a key does to the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    Stay,
    Close,
}

/// Where the box goes in `area` (the panes' rectangle), for `lines` of body.
pub fn geometry(
    area: &crate::layout::Area,
    lines: &[Line],
    cols: usize,
) -> (usize, usize, usize, usize) {
    let longest = lines
        .iter()
        .map(|l| match l {
            Line::Row { key, does } => key.chars().count() + does.chars().count() + 6,
            Line::Header(t) | Line::Group(t) => t.chars().count() + 4,
            Line::Blank => 0,
        })
        .max()
        .unwrap_or(0)
        // The title row needs room for its three parts.
        .max(TITLE_MIN);
    let (aw, ah) = (area.w as usize, area.h as usize);
    let bw = (longest + 4).min(80).min(aw.max(1)).min(cols.max(1));
    let bh = (ah * 4 / 5).max(8).min(ah.max(1));
    let x0 = area.x as usize + aw.saturating_sub(bw) / 2;
    let y0 = area.y as usize + ah.saturating_sub(bh) / 2;
    (x0, y0, bw, bh)
}

/// `help  prefix is ctrl+space`, `12–38 of 94` and the keys, with room between.
const TITLE_MIN: usize = 76;
pub const KEYS_HINT: &str = "j/k scroll · / filter · q close";

/// How many body lines fit in a box `bh` rows high: borders, title and rule take four.
pub fn body_rows(bh: usize) -> usize {
    bh.saturating_sub(4).max(1)
}

impl HelpState {
    /// The furthest `scroll` can go for `total` lines in `fit` rows.
    fn clamp(&mut self, total: usize, fit: usize) {
        self.scroll = self.scroll.min(total.saturating_sub(fit));
    }

    /// Handles a key; `total` body lines and `fit` visible rows bound the scroll.
    pub fn key(&mut self, chord: &str, text: Option<&str>, total: usize, fit: usize) -> KeyOutcome {
        if self.filtering {
            match chord {
                "escape" => {
                    self.filtering = false;
                    self.filter.clear();
                }
                "enter" => self.filtering = false,
                "backspace" => {
                    self.filter.pop();
                }
                _ => {
                    if let Some(t) = text.filter(|t| !t.chars().any(char::is_control)) {
                        self.filter.push_str(t);
                    }
                }
            }
            self.scroll = 0;
            return KeyOutcome::Stay;
        }
        let half = (fit / 2).max(1);
        match chord {
            "escape" if !self.filter.is_empty() => {
                self.filter.clear();
                self.scroll = 0;
            }
            "q" | "escape" | "?" => return KeyOutcome::Close,
            "j" | "down" => self.scroll += 1,
            "k" | "up" => self.scroll = self.scroll.saturating_sub(1),
            "ctrl+d" | "pagedown" => self.scroll += half,
            "ctrl+u" | "pageup" => self.scroll = self.scroll.saturating_sub(half),
            "g" | "home" => self.scroll = 0,
            "G" | "end" => self.scroll = usize::MAX,
            "/" => {
                self.filtering = true;
                self.scroll = 0;
            }
            _ => {}
        }
        self.clamp(total, fit);
        KeyOutcome::Stay
    }

    /// The mouse wheel over the box: three rows a notch.
    pub fn wheel(&mut self, down: bool, total: usize, fit: usize) {
        self.scroll = if down {
            self.scroll + 3
        } else {
            self.scroll.saturating_sub(3)
        };
        self.clamp(total, fit);
    }
}

/// Draws the box (`spec/SPEC.md` REQ-UI-045's mock).
pub(crate) fn draw(
    help: &Help,
    st: &HelpState,
    prefix: &str,
    area: &crate::layout::Area,
    c: &mut Canvas,
    theme: &Theme,
    cursor: &mut Option<(usize, usize)>,
) {
    let lines = help.lines(&st.filter);
    let (x0, y0, bw, bh) = geometry(area, &lines, c.w);
    if bw < 4 || bh < 4 {
        return;
    }
    let bg = theme.overlay_bg;
    let frame = attr(theme.accent, bg);
    for y in y0..y0 + bh {
        c.fill(x0, y, bw, ' ', &attr(theme.text, bg));
        c.put(x0, y, "│", &frame);
        c.put(x0 + bw - 1, y, "│", &frame);
    }
    c.fill(x0 + 1, y0, bw - 2, '─', &frame);
    c.fill(x0 + 1, y0 + bh - 1, bw - 2, '─', &frame);
    c.put(x0, y0, "╭", &frame);
    c.put(x0 + bw - 1, y0, "╮", &frame);
    c.put(x0, y0 + bh - 1, "╰", &frame);
    c.put(x0 + bw - 1, y0 + bh - 1, "╯", &frame);

    let fit = body_rows(bh);
    let total = lines.len();
    let first = st.scroll.min(total.saturating_sub(fit));
    let last = (first + fit).min(total);
    // Title row: what the prefix is, where we are, the keys.
    let ty = y0 + 1;
    let inner = bw - 4;
    let x = c.put(x0 + 2, ty, "help", &bold(attr(theme.text, bg)));
    let x = c.put(
        x + 2,
        ty,
        &format!("prefix is {prefix}"),
        &attr(theme.muted, bg),
    );
    let pos = if total == 0 {
        "nothing matches".to_owned()
    } else {
        format!("{}–{} of {total}", first + 1, last)
    };
    let right = if st.filtering || !st.filter.is_empty() {
        format!("/{}", st.filter)
    } else {
        KEYS_HINT.to_owned()
    };
    // The keys before the position when the row is short.
    let rw = right.chars().count();
    let end = x0 + 2 + inner;
    let rx = end.saturating_sub(rw);
    *cursor = None;
    if x + 2 + rw <= end {
        c.put(rx, ty, &right, &attr(theme.muted, bg));
        *cursor = st.filtering.then_some((rx + rw, ty));
        let pw = pos.chars().count();
        let mid = (x0 + (bw.saturating_sub(pw)) / 2).max(x + 2);
        if mid + pw + 2 <= rx {
            c.put(mid, ty, &pos, &attr(theme.muted, bg));
        }
    }
    c.fill(x0 + 1, y0 + 2, bw - 2, '─', &attr(theme.muted, bg));

    // Keys pad to the widest of their section.
    let mut widths = vec![0usize; total];
    let mut start = 0;
    for i in 0..=total {
        if i == total || matches!(lines[i], Line::Header(_)) {
            let w = lines[start..i]
                .iter()
                .filter_map(|l| match l {
                    Line::Row { key, .. } => Some(key.chars().count()),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            widths[start..i].iter_mut().for_each(|x| *x = w);
            start = i;
        }
    }
    for (row, i) in (first..last).enumerate() {
        let y = y0 + 3 + row;
        let x = x0 + 2;
        match &lines[i] {
            Line::Blank => {}
            Line::Header(t) => {
                c.put(x, y, t, &bold(attr(theme.accent, bg)));
            }
            Line::Group(t) => {
                c.put(x, y, t, &bold(attr(theme.text, bg)));
            }
            Line::Row { key, does } => {
                let kx = c.put(x, y, key, &attr(theme.accent, bg));
                let dx = (x + widths[i] + 2).max(kx + 1);
                let room = (x0 + bw - 2).saturating_sub(dx);
                let d: String = does.chars().take(room).collect();
                c.put(dx, y, &d, &attr(theme.text, bg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientConfig;

    fn help() -> Help {
        Help::build(&Bindings::from_config(&ClientConfig::default()), |_, _| {
            None
        })
    }

    #[test]
    fn ui_045_rows_fold_and_group_in_which_key_order() {
        let h = help();
        let groups: Vec<&str> = h.prefix.iter().map(|(g, _)| *g).collect();
        assert_eq!(groups, ["panes", "splits", "tabs", "session", "ui"]);
        let all: Vec<&Row> = h.prefix.iter().flat_map(|(_, v)| v).collect();
        let text = |a: Action| {
            all.iter()
                .find(|r| std::mem::discriminant(&r.action) == std::mem::discriminant(&a))
                .map(|r| (r.key_text(), r.does()))
                .unwrap()
        };
        assert_eq!(
            text(Action::Focus(femux_proto::msg::Direction::H)),
            ("h j k l".into(), "focus the pane in that direction".into())
        );
        assert_eq!(
            text(Action::Preselect(femux_proto::msg::Direction::H)).0,
            "ctrl+h/j/k/l"
        );
        assert_eq!(
            text(Action::Tab(1)),
            ("1-9".into(), "select that tab".into())
        );
        assert!(h.root.is_empty());
        assert!(!h.copy.is_empty());
    }

    #[test]
    fn ui_045_lines_have_sections_and_filter_by_key_action_or_description() {
        let h = help();
        let lines = h.lines("");
        let headers: Vec<&str> = lines
            .iter()
            .filter_map(|l| match l {
                Line::Header(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        // No root table: nothing is bound there by default.
        assert_eq!(
            headers,
            ["how the prefix works", "prefix table", "copy mode"]
        );
        assert_eq!(lines[0], Line::Header("how the prefix works".into()));
        // A blank row before every section header but the first.
        for (i, l) in lines.iter().enumerate().skip(1) {
            if matches!(l, Line::Header(_) | Line::Group(_)) {
                assert_eq!(lines[i - 1], Line::Blank, "{l:?}");
            }
        }
        // By description, by key, by action name.
        let only = |f: &str| {
            h.lines(f)
                .into_iter()
                .filter_map(|l| match l {
                    Line::Row { key, .. } => Some(key),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(only("zoom").contains(&"z".to_owned()), "{:?}", only("zoom"));
        assert_eq!(only("ctrl+h/j"), ["ctrl+h/j/k/l"]);
        assert!(only("preselect").contains(&"ctrl+h/j/k/l".to_owned()));
        assert!(h.lines("no such thing at all").is_empty());
    }

    #[test]
    fn ui_045_keys_scroll_filter_and_close() {
        let mut s = HelpState::default();
        assert_eq!(s.key("j", None, 50, 10), KeyOutcome::Stay);
        assert_eq!(s.scroll, 1);
        s.key("ctrl+d", None, 50, 10);
        assert_eq!(s.scroll, 6);
        s.key("G", None, 50, 10);
        assert_eq!(s.scroll, 40);
        s.key("g", None, 50, 10);
        assert_eq!(s.scroll, 0);
        s.wheel(true, 50, 10);
        assert_eq!(s.scroll, 3);
        s.key("/", None, 50, 10);
        s.key("z", Some("z"), 50, 10);
        assert_eq!((s.filtering, s.filter.as_str()), (true, "z"));
        s.key("enter", None, 50, 10);
        // esc clears the filter before it closes.
        assert_eq!(s.key("escape", None, 50, 10), KeyOutcome::Stay);
        assert!(s.filter.is_empty());
        assert_eq!(s.key("escape", None, 50, 10), KeyOutcome::Close);
        assert_eq!(s.key("q", None, 50, 10), KeyOutcome::Close);
        assert_eq!(s.key("?", None, 50, 10), KeyOutcome::Close);
    }

    #[test]
    fn ui_045_the_box_is_four_fifths_high_and_at_most_80_wide() {
        let lines = help().lines("");
        let area = crate::layout::Area {
            x: 0,
            y: 0,
            w: 200,
            h: 50,
        };
        let (x0, y0, bw, bh) = geometry(&area, &lines, 200);
        assert_eq!(bh, 40);
        assert!((TITLE_MIN..=80).contains(&bw));
        assert_eq!((x0, y0), ((200 - bw) / 2, 5));
        let small = crate::layout::Area {
            x: 0,
            y: 0,
            w: 40,
            h: 9,
        };
        let (_, _, bw, bh) = geometry(&small, &lines, 40);
        assert_eq!((bw, bh), (40, 8));
    }
}
