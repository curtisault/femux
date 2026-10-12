//! The agent inbox (REQ-UI-044, `prefix i`, agents plugin): every pane across sessions whose
//! agent is `blocked`, `error` or `done`, with one-key jump and one-key answers. It reads the
//! navigators' [`Directory`] and, for each listed pane, its last screen line.

use std::collections::BTreeMap;

use femux_plugin::{Span, Spans, Style};

use crate::nav::{DirSession, Directory, ago, now_ms};
use crate::render::{Canvas, attr, bold};
use crate::theme::Theme;

/// An open inbox.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InboxState {
    pub filter: String,
    pub filtering: bool,
    pub selected: usize,
    /// Pane id -> its last non-blank screen line (what the agent last said).
    pub lines: BTreeMap<String, String>,
}

/// One inbox row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub session: String,
    pub tab: String,
    /// 1-based tab number.
    pub tab_index: usize,
    pub pane: String,
    pub name: String,
    pub state: String,
    pub source: String,
    pub since: Option<u64>,
}

/// Inbox states in display order.
pub const STATES: &[&str] = &["blocked", "error", "done"];

pub fn glyph(state: &str) -> (&'static str, Style) {
    match state {
        "working" => ("◐", Style::Accent),
        "blocked" => ("■", Style::Warn),
        "done" => ("✓", Style::Ok),
        "error" => ("✗", Style::Error),
        _ => ("·", Style::Muted),
    }
}

/// The rows: blocked first, then error, then done; newest first within a state.
pub fn entries(dir: &Directory, st: &InboxState) -> Vec<Entry> {
    let f = st.filter.to_lowercase();
    let mut out = Vec::new();
    for s in &dir.sessions {
        for t in &s.tabs {
            for p in DirSession::tab_panes(t) {
                let Some(info) = s.panes.get(&p) else {
                    continue;
                };
                let Some(state) = info.meta.get("agent.state") else {
                    continue;
                };
                if !STATES.contains(&state.as_str()) {
                    continue;
                }
                let name = info
                    .meta
                    .get("agent.name")
                    .cloned()
                    .unwrap_or_else(|| info.fg_cmd.clone());
                let e = Entry {
                    session: s.summary.name.clone(),
                    tab: t.id.clone(),
                    tab_index: t.index as usize + 1,
                    pane: p.clone(),
                    name,
                    state: state.clone(),
                    source: info
                        .meta
                        .get("agent.source")
                        .cloned()
                        .unwrap_or_else(|| "hook".into()),
                    since: info.meta.get("agent.since").and_then(|v| v.parse().ok()),
                };
                let hay = format!(
                    "{} {} {} {}",
                    e.session,
                    e.name,
                    e.state,
                    st.lines.get(&p).map_or("", String::as_str)
                )
                .to_lowercase();
                if f.is_empty() || hay.contains(&f) {
                    out.push(e);
                }
            }
        }
    }
    let rank = |s: &str| STATES.iter().position(|x| *x == s).unwrap_or(9);
    out.sort_by(|a, b| {
        rank(&a.state)
            .cmp(&rank(&b.state))
            .then(b.since.cmp(&a.since))
            .then(a.pane.cmp(&b.pane))
    });
    out
}

/// `agent.state` -> panes in it, over every session.
pub fn counts(dir: &Directory) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for s in &dir.sessions {
        for (k, v) in s.agents() {
            *out.entry(k).or_insert(0) += v;
        }
    }
    out
}

/// The two lines of a row: what the agent said, and state · age · source.
pub fn row_lines(e: &Entry, said: Option<&str>, now: u64) -> (Spans, Spans) {
    let (g, style) = glyph(&e.state);
    let first = vec![
        Span::new(format!("{g} "), style),
        Span::new(
            format!("{:<12}", format!("{} · {}", e.session, e.tab_index)),
            Style::Text,
        ),
        Span::new(format!("{:<12} ", e.name), style),
        Span::text(said.unwrap_or("").to_owned()),
    ];
    let age = e.since.map_or("-".into(), |s| ago(s, now));
    let age = if e.state == "blocked" {
        format!("waiting {}", age.trim_end_matches(" ago"))
    } else {
        age
    };
    let second = vec![
        Span::text(" ".repeat(27)),
        Span::new(
            format!("{} · {age} · source: {}", e.state, e.source),
            Style::Muted,
        ),
    ];
    (first, second)
}

/// What the right edge of a row offers.
pub fn row_hint(e: &Entry) -> &'static str {
    match e.state.as_str() {
        "blocked" => "y  n",
        "error" => "enter to open",
        _ => "x dismiss",
    }
}

pub const HINTS: &str = "enter jump · y/n answer · a always · x dismiss · / filter · q close";

/// A centered box over the panes (`inbox.png`).
pub(crate) fn draw_inbox(
    dir: &Directory,
    st: &InboxState,
    c: &mut Canvas,
    theme: &Theme,
    cursor: &mut Option<(usize, usize)>,
) {
    let rows = entries(dir, st);
    let bw = c.w.saturating_sub(4).clamp(20, 110).min(c.w);
    let body = (rows.len().max(1)) * 2;
    let bh = (body + 6).min(c.h.saturating_sub(2)).max(5).min(c.h);
    let x0 = c.w.saturating_sub(bw) / 2;
    let y0 = c.h.saturating_sub(bh) / 2;
    let bg = theme.overlay_bg;
    let frame = attr(theme.accent, bg);
    for y in y0..y0 + bh {
        c.fill(x0, y, bw, ' ', &attr(theme.text, bg));
    }
    c.fill(x0, y0, bw, '─', &frame);
    c.fill(x0, y0 + bh - 1, bw, '─', &frame);
    // Header.
    let sessions: std::collections::BTreeSet<&str> =
        rows.iter().map(|e| e.session.as_str()).collect();
    let x = c.put(x0 + 1, y0 + 1, "Inbox", &bold(attr(theme.text, bg)));
    let x = c.put(
        x + 2,
        y0 + 1,
        &format!("{} across {} sessions", rows.len(), sessions.len()),
        &attr(theme.muted, bg),
    );
    if st.filtering || !st.filter.is_empty() {
        let fx = c.put(x + 2, y0 + 1, "/ ", &attr(theme.muted, bg));
        let fx = c.put(fx, y0 + 1, &st.filter, &attr(theme.text, bg));
        *cursor = st.filtering.then_some((fx, y0 + 1));
    } else {
        *cursor = None;
    }
    if bw > HINTS.chars().count() + 30 {
        c.put(
            x0 + bw - HINTS.chars().count() - 1,
            y0 + 1,
            HINTS,
            &attr(theme.muted, bg),
        );
    }
    c.fill(
        x0 + 1,
        y0 + 2,
        bw.saturating_sub(2),
        '─',
        &attr(theme.muted, bg),
    );
    // Rows, scrolled so the selection shows.
    let top = y0 + 3;
    let fit = (bh.saturating_sub(6) / 2).max(1);
    let first = st.selected.saturating_sub(fit - 1);
    let now = now_ms();
    if rows.is_empty() {
        c.put(
            x0 + 2,
            top,
            "no agent is blocked, done or failing",
            &attr(theme.muted, bg),
        );
    }
    for (i, e) in rows.iter().enumerate().skip(first).take(fit) {
        let y = top + (i - first) * 2;
        let selected = i == st.selected;
        let rbg = if selected { theme.selection } else { bg };
        if selected {
            c.fill(x0 + 1, y, bw.saturating_sub(2), ' ', &attr(theme.text, rbg));
            c.fill(
                x0 + 1,
                y + 1,
                bw.saturating_sub(2),
                ' ',
                &attr(theme.text, rbg),
            );
            c.put(x0, y, "▌", &attr(theme.warn, rbg));
            c.put(x0, y + 1, "▌", &attr(theme.warn, rbg));
        }
        let hint = row_hint(e);
        let room = bw.saturating_sub(hint.chars().count() + 6);
        let (a, b) = row_lines(e, st.lines.get(&e.pane).map(String::as_str), now);
        c.spans(x0 + 2, y, &crate::render::clip(&a, room), theme, rbg);
        c.spans(x0 + 2, y + 1, &crate::render::clip(&b, room), theme, rbg);
        let hx = x0 + bw - hint.chars().count() - 2;
        if e.state == "blocked" {
            c.put(hx, y, "y", &bold(attr(theme.accent_fg, theme.warn)));
            c.put(hx + 3, y, "n", &attr(theme.text, rbg));
        } else {
            c.put(hx, y, hint, &attr(theme.muted, rbg));
        }
    }
    // Footer: counts over every session.
    let fy = y0 + bh - 2;
    c.fill(
        x0 + 1,
        fy - 1,
        bw.saturating_sub(2),
        '─',
        &attr(theme.muted, bg),
    );
    let n = counts(dir);
    let mut foot: Spans = Vec::new();
    for s in ["working", "blocked", "error", "done"] {
        let (g, style) = glyph(s);
        foot.push(Span::new(format!("{g} "), style));
        foot.push(Span::new(
            format!("{} {s}   ", n.get(s).copied().unwrap_or(0)),
            Style::Muted,
        ));
    }
    foot.push(Span::new(
        "source: hook › osc › process › heuristic",
        Style::Muted,
    ));
    c.spans(
        x0 + 2,
        fy,
        &crate::render::clip(&foot, bw.saturating_sub(4)),
        theme,
        bg,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use femux_proto::msg as m;

    fn info(id: &str, meta: &[(&str, &str)]) -> m::PaneInfoOk {
        m::PaneInfoOk {
            id: id.into(),
            fg_cmd: "sh".into(),
            meta: meta
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..Default::default()
        }
    }

    fn tab(id: &str, index: u32, panes: &[&str]) -> m::TabSnapshot {
        m::TabSnapshot {
            id: id.into(),
            index,
            title: id.into(),
            focus: panes[0].into(),
            tree: if panes.len() == 1 {
                m::LayoutNode::PaneNode(m::PaneNode {
                    id: panes[0].into(),
                })
            } else {
                m::LayoutNode::SplitNode(m::SplitNode {
                    children: panes
                        .iter()
                        .map(|p| m::LayoutNode::PaneNode(m::PaneNode { id: (*p).into() }))
                        .collect(),
                    ..Default::default()
                })
            },
            ..Default::default()
        }
    }

    fn dir() -> Directory {
        let mut api = DirSession {
            summary: m::SessionSummary {
                name: "api".into(),
                ..Default::default()
            },
            tabs: vec![tab("T1", 1, &["A", "B", "C"])],
            ..Default::default()
        };
        for i in [
            info(
                "A",
                &[
                    ("agent.state", "done"),
                    ("agent.name", "opencode"),
                    ("agent.since", "100"),
                ],
            ),
            info(
                "B",
                &[
                    ("agent.state", "blocked"),
                    ("agent.name", "codex"),
                    ("agent.since", "50"),
                ],
            ),
            info("C", &[("agent.state", "working"), ("agent.name", "claude")]),
        ] {
            api.panes.insert(i.id.clone(), i);
        }
        let mut infra = DirSession {
            summary: m::SessionSummary {
                name: "infra".into(),
                ..Default::default()
            },
            tabs: vec![tab("T9", 3, &["D"])],
            ..Default::default()
        };
        infra.panes.insert(
            "D".into(),
            info(
                "D",
                &[
                    ("agent.state", "error"),
                    ("agent.source", "process"),
                    ("agent.since", "70"),
                ],
            ),
        );
        Directory {
            sessions: vec![api, infra],
        }
    }

    #[test]
    fn ui_044_lists_blocked_error_done_across_sessions() {
        let d = dir();
        let mut st = InboxState::default();
        let e = entries(&d, &st);
        let got: Vec<(&str, &str, &str)> = e
            .iter()
            .map(|e| (e.pane.as_str(), e.state.as_str(), e.name.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("B", "blocked", "codex"),
                ("D", "error", "sh"),
                ("A", "done", "opencode")
            ]
        );
        assert_eq!(e[1].session, "infra");
        assert_eq!(e[1].tab_index, 4);
        assert_eq!(e[1].source, "process");
        assert_eq!(e[0].source, "hook");
        let c = counts(&d);
        assert_eq!(
            (c["working"], c["blocked"], c["error"], c["done"]),
            (1, 1, 1, 1)
        );
        // The filter matches session, agent, state and what the agent said.
        st.filter = "infra".into();
        assert_eq!(entries(&d, &st).len(), 1);
        st.filter = "compose".into();
        st.lines
            .insert("B".into(), "Run docker compose up -d db?".into());
        assert_eq!(entries(&d, &st)[0].pane, "B");
        let (a, b) = row_lines(
            &e[0],
            st.lines.get("B").map(String::as_str),
            50_000 + 64_000,
        );
        let a: String = a.iter().map(|s| s.text.as_str()).collect();
        let b: String = b.iter().map(|s| s.text.as_str()).collect();
        assert!(a.starts_with("■ api · 2"), "{a}");
        assert!(
            a.contains("codex") && a.ends_with("Run docker compose up -d db?"),
            "{a}"
        );
        assert!(b.contains("blocked · waiting 1m · source: hook"), "{b}");
    }
}
