//! The session navigator (REQ-UI-040), the window navigator (REQ-UI-041) and the side nav
//! (REQ-UI-042). All three read a [`Directory`] of every session the daemon holds, refreshed
//! through the protocol while one of them is open.

use std::collections::{BTreeMap, BTreeSet};

use femux_plugin::{Span, Spans, Style};
use femux_proto::msg as m;
use termwiz::color::ColorAttribute;

use crate::app::{App, NavTarget};
use crate::layout::Layout;
use crate::model::tree_panes;
use crate::render::{Canvas, attr, attributes, bold};
use crate::theme::Theme;

/// One session as the navigators see it.
#[derive(Debug, Clone, Default)]
pub struct DirSession {
    pub summary: m::SessionSummary,
    pub tabs: Vec<m::TabSnapshot>,
    pub panes: BTreeMap<String, m::PaneInfoOk>,
}

impl DirSession {
    pub fn tab_panes(t: &m::TabSnapshot) -> Vec<String> {
        let mut out = Vec::new();
        tree_panes(&t.tree, &mut out);
        out.extend(t.floats.iter().map(|f| f.id.clone()));
        out
    }

    /// `agent.state` -> count over the session's panes.
    pub fn agents(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for p in self.panes.values() {
            if let Some(s) = p.meta.get("agent.state") {
                *out.entry(s.clone()).or_insert(0) += 1;
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub sessions: Vec<DirSession>,
}

impl Directory {
    pub fn session(&self, name: &str) -> Option<&DirSession> {
        self.sessions.iter().find(|s| s.summary.name == name)
    }

    /// New `session.list` answer: keeps what is known of sessions still there.
    pub fn on_list(&mut self, list: Vec<m::SessionSummary>) {
        let mut old: BTreeMap<String, DirSession> = std::mem::take(&mut self.sessions)
            .into_iter()
            .map(|s| (s.summary.name.clone(), s))
            .collect();
        self.sessions = list
            .into_iter()
            .map(|summary| {
                let mut s = old.remove(&summary.name).unwrap_or_default();
                s.summary = summary;
                s
            })
            .collect();
    }

    pub fn on_tabs(&mut self, session: &str, tabs: Vec<m::TabSnapshot>) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.summary.name == session) {
            let keep: BTreeSet<String> = tabs.iter().flat_map(DirSession::tab_panes).collect();
            s.panes.retain(|p, _| keep.contains(p));
            s.tabs = tabs;
        }
    }

    pub fn on_pane(&mut self, session: &str, info: m::PaneInfoOk) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.summary.name == session) {
            s.panes.insert(info.id.clone(), info);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKind {
    /// `S`: sessions, expandable to their tabs (REQ-UI-040).
    Sessions,
    /// `W`: sessions → tabs → panes (REQ-UI-041).
    Windows,
}

/// An open navigator.
#[derive(Debug, Clone, PartialEq)]
pub struct NavState {
    pub kind: NavKind,
    pub filter: String,
    /// `/` is taking the filter text.
    pub filtering: bool,
    pub selected: usize,
    /// Sessions shown expanded (the session navigator; the window navigator shows all).
    pub expanded: BTreeSet<String>,
    /// The pane shown in the preview and its screen lines (REQ-UI-041: a live preview).
    pub preview: Option<(String, Vec<Vec<m::Cell>>)>,
}

impl NavState {
    pub fn new(kind: NavKind, current: &str) -> NavState {
        NavState {
            kind,
            filter: String::new(),
            filtering: false,
            selected: 0,
            expanded: [current.to_owned()].into(),
            preview: None,
        }
    }
}

/// One line of a navigator list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub target: NavTarget,
    pub label: Spans,
    pub right: String,
}

fn agent_spans(agents: &BTreeMap<String, usize>) -> Spans {
    let mut out = Vec::new();
    for (state, glyph, style) in [
        ("working", "◐", Style::Accent),
        ("blocked", "■", Style::Warn),
        ("done", "✓", Style::Ok),
        ("error", "✗", Style::Error),
    ] {
        if let Some(n) = agents.get(state).filter(|n| **n > 0) {
            out.push(Span::new(format!("  {glyph} "), style));
            out.push(Span::text(n.to_string()));
        }
    }
    out
}

fn short_path(p: &str) -> String {
    let home = femux_home();
    match p.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => p.to_owned(),
    }
}

fn femux_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

fn matches(filter: &str, text: &str) -> bool {
    filter.is_empty() || text.to_lowercase().contains(&filter.to_lowercase())
}

/// What a navigator lists, filtered and expanded as `nav` says.
pub fn rows(nav: &NavState, dir: &Directory, current: &str) -> Vec<Row> {
    let mut out = Vec::new();
    for s in &dir.sessions {
        let name = &s.summary.name;
        let tab_hit =
            |t: &m::TabSnapshot| matches(&nav.filter, &t.title) || matches(&nav.filter, name);
        if !matches(&nav.filter, name) && !s.tabs.iter().any(tab_hit) {
            continue;
        }
        let expanded =
            nav.kind == NavKind::Windows || nav.expanded.contains(name) || !nav.filter.is_empty();
        let mut label = vec![
            Span::new(if expanded { "▾ " } else { "▸ " }, Style::Muted),
            Span::new(
                name.clone(),
                if name == current {
                    Style::Title
                } else {
                    Style::Text
                },
            ),
            Span::new(format!(" {}", short_path(&s.summary.root)), Style::Muted),
        ];
        label.extend(agent_spans(&s.agents()));
        let right = if name == current {
            "attached".to_owned()
        } else {
            plural(s.summary.tabs as usize, "tab")
        };
        out.push(Row {
            depth: 0,
            target: NavTarget::Session(name.clone()),
            label,
            right,
        });
        if !expanded {
            continue;
        }
        for t in s.tabs.iter().filter(|t| tab_hit(t)) {
            let panes = DirSession::tab_panes(t);
            let mut label = vec![Span::text(format!("{} {}", t.index + 1, t.title))];
            if t.zoomed {
                label.push(Span::new(" Z", Style::Muted));
            }
            let agents: Vec<(String, String)> = panes
                .iter()
                .filter_map(|p| s.panes.get(p))
                .filter_map(|p| {
                    Some((
                        p.meta.get("agent.state")?.clone(),
                        p.meta.get("agent.name")?.clone(),
                    ))
                })
                .collect();
            for (state, who) in agents {
                let (glyph, style) = match state.as_str() {
                    "working" => ("◐", Style::Accent),
                    "blocked" => ("■", Style::Warn),
                    "done" => ("✓", Style::Ok),
                    "error" => ("✗", Style::Error),
                    _ => continue,
                };
                label.push(Span::new(format!("  {glyph} "), style));
                label.push(Span::text(who));
            }
            out.push(Row {
                depth: 1,
                target: NavTarget::Tab {
                    session: name.clone(),
                    tab: t.id.clone(),
                },
                label,
                right: plural(panes.len(), "pane"),
            });
            if nav.kind != NavKind::Windows {
                continue;
            }
            for p in panes {
                let info = s.panes.get(&p);
                let what = info
                    .map(|i| {
                        if !i.title.is_empty() {
                            i.title.clone()
                        } else {
                            i.fg_cmd.clone()
                        }
                    })
                    .unwrap_or_default();
                let cwd = info.map(|i| short_path(&i.cwd)).unwrap_or_default();
                let focus = if p == t.focus { "● " } else { "  " };
                out.push(Row {
                    depth: 2,
                    target: NavTarget::Pane {
                        session: name.clone(),
                        tab: t.id.clone(),
                        pane: p.clone(),
                    },
                    label: vec![
                        Span::new(focus, Style::Accent),
                        Span::text(what),
                        Span::new(format!(" {cwd}"), Style::Muted),
                    ],
                    right: String::new(),
                });
            }
        }
    }
    out
}

/// The pane a row previews: a pane itself, a tab's focus, a session's first tab's focus.
pub fn preview_pane(row: &NavTarget, dir: &Directory) -> Option<String> {
    match row {
        NavTarget::Pane { pane, .. } => Some(pane.clone()),
        NavTarget::Tab { session, tab } => dir
            .session(session)?
            .tabs
            .iter()
            .find(|t| t.id == *tab)
            .map(|t| t.focus.clone()),
        NavTarget::Session(s) => dir.session(s)?.tabs.first().map(|t| t.focus.clone()),
    }
}

/// "14s ago", "2h ago".
pub fn ago(ms: u64, now_ms: u64) -> String {
    let s = now_ms.saturating_sub(ms) / 1000;
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The side nav (REQ-UI-042): every session, the current one expanded to its tabs; a click
/// switches to the session or tab. `app.sidenav_hits` is refreshed for the mouse.
pub(crate) fn draw_sidenav(app: &mut App, c: &mut Canvas, theme: &Theme, l: &Layout) {
    app.sidenav_hits.clear();
    let Some(a) = l.sidenav else { return };
    let bg = theme.overlay_bg;
    let (x0, w) = (a.x as usize, a.w as usize);
    for y in a.y..a.y + a.h {
        c.fill(x0, y as usize, w, ' ', &attr(theme.text, bg));
    }
    // A separator on the right edge.
    for y in a.y..a.y + a.h {
        c.put(
            x0 + w - 1,
            y as usize,
            "│",
            &attr(theme.muted, ColorAttribute::Default),
        );
    }
    let bottom = (a.y + a.h) as usize;
    let mut y = a.y as usize + 1;
    c.put(x0 + 2, y, "sessions", &attr(theme.muted, bg));
    y += 1;
    let current = app.model.session.clone();
    let mut names: Vec<String> = app
        .dir
        .sessions
        .iter()
        .map(|s| s.summary.name.clone())
        .collect();
    if !names.contains(&current) {
        names.insert(0, current.clone());
    }
    for name in names {
        if y + 2 >= bottom {
            break;
        }
        let is_current = name == current;
        let a1 = if is_current {
            bold(attr(theme.text, bg))
        } else {
            attr(theme.text, bg)
        };
        let glyph = if is_current { "▾ " } else { "▸ " };
        c.put(x0 + 2, y, glyph, &attr(theme.muted, bg));
        c.put(x0 + 4, y, &name, &a1);
        app.sidenav_hits
            .push((y as u16, NavTarget::Session(name.clone())));
        y += 1;
        if !is_current {
            continue;
        }
        let tabs = app.model.tabs.clone();
        for (i, t) in tabs.iter().enumerate() {
            if y + 2 >= bottom {
                break;
            }
            let mut label = format!("{} {}", i + 1, app.model.tab_title(t));
            if app.model.tab_running(t) {
                label.push_str(" ●");
            }
            if t.urgent {
                label.push_str(" !");
            }
            if t.zoomed {
                label.push_str(" Z");
            }
            let label: String = label.chars().take(w.saturating_sub(6)).collect();
            let row_attr = if t.id == app.model.current {
                attr(theme.text, theme.selection)
            } else if t.urgent {
                attr(theme.warn, bg)
            } else {
                attr(theme.text, bg)
            };
            if t.id == app.model.current {
                c.fill(x0 + 1, y, w.saturating_sub(2), ' ', &row_attr);
            }
            c.put(x0 + 4, y, &label, &row_attr);
            app.sidenav_hits.push((
                y as u16,
                NavTarget::Tab {
                    session: name.clone(),
                    tab: t.id.clone(),
                },
            ));
            y += 1;
        }
        y += 1;
    }
    c.put(
        x0 + 2,
        bottom - 1,
        &format!("{} b hides", app.prefix_label()),
        &attr(theme.muted, bg),
    );
}

/// A navigator over the whole screen: the list on the left, a preview of the selected pane on
/// the right, details at its foot (REQ-UI-040/041).
pub(crate) fn draw_navigator(
    app: &App,
    nav: &NavState,
    c: &mut Canvas,
    theme: &Theme,
    cursor: &mut Option<(usize, usize)>,
) {
    let (w, h) = (c.w, c.h.saturating_sub(1));
    let bg = ColorAttribute::Default;
    for y in 0..h {
        c.fill(0, y, w, ' ', &attr(theme.text, bg));
    }
    let title = match nav.kind {
        NavKind::Sessions => "Sessions",
        NavKind::Windows => "Windows",
    };
    let x = c.put(1, 0, title, &bold(attr(theme.text, bg)));
    c.put(
        x + 2,
        0,
        &plural(app.dir.sessions.len(), "session"),
        &attr(theme.muted, bg),
    );
    let hints = match nav.kind {
        NavKind::Sessions => {
            "j/k move · enter attach · n new · r rename · x kill · tab expand · / filter · q close"
        }
        NavKind::Windows => "j/k move · enter go · / filter · q close",
    };
    c.put(
        w.saturating_sub(hints.chars().count() + 1),
        0,
        hints,
        &attr(theme.muted, bg),
    );
    let list_w = (w * 45 / 100).max(20).min(w);
    // The filter line.
    let fy = 2;
    let fx = c.put(2, fy, "/ ", &attr(theme.muted, bg));
    let fx = c.put(fx, fy, &nav.filter, &attr(theme.text, bg));
    if nav.filtering {
        *cursor = Some((fx, fy));
    } else {
        *cursor = None;
    }
    c.fill(
        1,
        fy + 1,
        list_w.saturating_sub(2),
        '─',
        &attr(theme.muted, bg),
    );
    let rows = rows(nav, &app.dir, &app.model.session);
    let top = fy + 2;
    let visible = h.saturating_sub(top);
    let first = nav.selected.saturating_sub(visible.saturating_sub(1));
    for (i, row) in rows.iter().enumerate().skip(first).take(visible) {
        let y = top + i - first;
        let selected = i == nav.selected;
        let rbg = if selected { theme.selection } else { bg };
        if selected {
            c.fill(1, y, list_w.saturating_sub(2), ' ', &attr(theme.text, rbg));
        }
        let x = 2 + row.depth * 2;
        let label: Spans = row.label.clone();
        c.spans(x, y, &label, theme, rbg);
        if !row.right.is_empty() {
            c.put(
                list_w.saturating_sub(row.right.chars().count() + 2),
                y,
                &row.right,
                &attr(theme.muted, rbg),
            );
        }
    }
    // The preview.
    let px = list_w + 1;
    let pw = w.saturating_sub(px + 1);
    for y in 1..h {
        c.put(list_w, y, "│", &attr(theme.muted, bg));
    }
    let Some(sel) = rows.get(nav.selected) else {
        return;
    };
    let (session, tab_index, pane) = match &sel.target {
        NavTarget::Session(s) => (s.clone(), None, preview_pane(&sel.target, &app.dir)),
        NavTarget::Tab { session, tab } | NavTarget::Pane { session, tab, .. } => (
            session.clone(),
            app.dir
                .session(session)
                .and_then(|s| s.tabs.iter().find(|t| t.id == *tab))
                .map(|t| t.index + 1),
            preview_pane(&sel.target, &app.dir),
        ),
    };
    let mut head = session.clone();
    if let Some(i) = tab_index {
        head.push_str(&format!(" · tab {i}"));
    }
    let ds = app.dir.session(&session);
    if let Some(info) = pane.as_ref().and_then(|p| ds.and_then(|s| s.panes.get(p))) {
        head.push_str(&format!(" · pane {}", info.fg_cmd));
    }
    let x = c.put(px + 1, 2, "preview ", &bold(attr(theme.text, bg)));
    c.put(x, 2, &head, &attr(theme.muted, bg));
    c.put(px + pw.saturating_sub(5), 2, "live", &attr(theme.muted, bg));
    let foot = h.saturating_sub(1);
    if let Some((p, lines)) = &nav.preview
        && Some(p) == pane.as_ref()
    {
        for (i, line) in lines.iter().enumerate() {
            let y = 4 + i;
            if y + 1 >= foot {
                break;
            }
            let mut x = px + 1;
            for cell in line {
                if x >= px + pw {
                    break;
                }
                let text = if cell.ch.is_empty() { " " } else { &cell.ch };
                x = c.put(x, y, text, &attributes(cell));
            }
        }
    }
    if let Some(s) = ds {
        let panes: usize = s.tabs.iter().map(|t| DirSession::tab_panes(t).len()).sum();
        let now = now_ms();
        let details = format!(
            "created {}   last activity {}   {} · {}   {}",
            ago(s.summary.created, now),
            ago(s.summary.last_activity, now),
            plural(s.tabs.len(), "tab"),
            plural(panes, "pane"),
            plural(s.summary.clients as usize, "client"),
        );
        c.fill(px, foot.saturating_sub(1), pw, '─', &attr(theme.muted, bg));
        c.put(px + 1, foot, &details, &attr(theme.muted, bg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> Directory {
        let tab = |id: &str, index: u32, title: &str, panes: &[&str]| {
            let leaf = |p: &str| m::LayoutNode::PaneNode(m::PaneNode { id: p.into() });
            let tree = if panes.len() == 1 {
                leaf(panes[0])
            } else {
                m::LayoutNode::SplitNode(m::SplitNode {
                    dir: m::SplitDir::H,
                    ratio: m::Ratio { num: 1, den: 2 },
                    children: vec![leaf(panes[0]), leaf(panes[1])],
                })
            };
            m::TabSnapshot {
                id: id.into(),
                title: title.into(),
                index,
                tree,
                focus: panes[0].into(),
                ..Default::default()
            }
        };
        let mut d = Directory::default();
        d.on_list(vec![
            m::SessionSummary {
                name: "api".into(),
                root: "/code/api".into(),
                tabs: 2,
                ..Default::default()
            },
            m::SessionSummary {
                name: "writing".into(),
                root: "/w".into(),
                tabs: 1,
                ..Default::default()
            },
        ]);
        d.on_tabs(
            "api",
            vec![
                tab("T1", 0, "shell", &["A"]),
                tab("T2", 1, "agents", &["B", "C"]),
            ],
        );
        d.on_tabs("writing", vec![tab("T3", 0, "notes", &["D"])]);
        d.on_pane(
            "api",
            m::PaneInfoOk {
                id: "B".into(),
                fg_cmd: "claude".into(),
                meta: [
                    ("agent.state".to_owned(), "blocked".to_owned()),
                    ("agent.name".to_owned(), "claude".to_owned()),
                ]
                .into(),
                ..Default::default()
            },
        );
        d
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                format!(
                    "{}{}",
                    "  ".repeat(r.depth),
                    r.label.iter().map(|s| s.text.as_str()).collect::<String>()
                )
            })
            .collect()
    }

    #[test]
    fn ui_040_sessions_expand_to_tabs_and_filter() {
        let d = dir();
        let mut nav = NavState::new(NavKind::Sessions, "api");
        let r = rows(&nav, &d, "api");
        assert_eq!(
            texts(&r),
            [
                "▾ api /code/api  ■ 1",
                "  1 shell",
                "  2 agents  ■ claude",
                "▸ writing /w"
            ]
        );
        assert_eq!(
            (r[0].right.as_str(), r[3].right.as_str()),
            ("attached", "1 tab")
        );
        assert_eq!(r[2].right, "2 panes");
        nav.expanded.clear();
        assert_eq!(rows(&nav, &d, "api").len(), 2);
        // A filter matches session names and tab titles, and opens what it matches.
        nav.filter = "NOT".into();
        assert_eq!(texts(&rows(&nav, &d, "api")), ["▾ writing /w", "  1 notes"]);
        nav.filter = "zzz".into();
        assert!(rows(&nav, &d, "api").is_empty());
    }

    #[test]
    fn ui_041_windows_list_every_pane_and_preview_the_selection() {
        let d = dir();
        let nav = NavState::new(NavKind::Windows, "api");
        let r = rows(&nav, &d, "api");
        let t = texts(&r);
        // Two sessions, three tabs, four panes.
        assert_eq!(t.len(), 2 + 3 + 4, "{t:#?}");
        assert!(t[4].starts_with("    ● claude"), "{t:#?}");
        assert_eq!(
            r[4].target,
            NavTarget::Pane {
                session: "api".into(),
                tab: "T2".into(),
                pane: "B".into()
            }
        );
        assert_eq!(preview_pane(&r[0].target, &d).as_deref(), Some("A"));
        assert_eq!(preview_pane(&r[3].target, &d).as_deref(), Some("B"));
        assert_eq!(preview_pane(&r[5].target, &d).as_deref(), Some("C"));
        assert_eq!(ago(0, 14_000), "14s ago");
        assert_eq!(ago(0, 2 * 3600 * 1000 + 5), "2h ago");
    }
}
