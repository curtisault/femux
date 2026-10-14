//! The panel (REQ-UI-042, 0.10.0): a collapsible left column of stacked sections. `workspace`
//! is sessions → tabs → panes, `agents` every agent across sessions. `prefix b` shows or hides
//! it, `prefix ctrl+b` focuses it; its right edge drags to resize. It reads the navigators'
//! [`Directory`] for other sessions and the live model for the attached one.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use femux_plugin::{Span, Spans, Style};
use termwiz::color::ColorAttribute;

use crate::app::{App, NavTarget};
use crate::inbox::glyph;
use crate::layout::Layout;
use crate::nav::{DirSession, Directory, now_ms};
use crate::render::{Canvas, attr};
use crate::theme::Theme;

/// Narrowest and default widths; the widest is half the terminal.
pub const MIN_WIDTH: u16 = 16;
pub const DEFAULT_WIDTH: u16 = 28;

/// The panel's state; open state and width belong to this client.
#[derive(Debug, Clone, Default)]
pub struct PanelState {
    pub open: bool,
    pub width: u16,
    /// Keys go to the panel (`prefix ctrl+b`) rather than the pane.
    pub focused: bool,
    pub selected: usize,
    /// Sections collapsed to their header.
    pub collapsed: BTreeSet<String>,
    /// Sessions of `workspace` whose expansion differs from the default (the attached one
    /// open, the others closed).
    pub toggled: BTreeSet<String>,
    /// Agent pane -> its last non-blank line.
    pub lines: BTreeMap<String, String>,
    pub lines_fetched: Option<Instant>,
    /// The right edge is being dragged.
    pub dragging: bool,
    /// Screen row -> index into the rows, from the last draw.
    pub hits: Vec<(u16, usize)>,
    /// First row drawn (the panel scrolls to keep the selection in view).
    pub scroll: usize,
    pub git: GitCache,
}

/// What a row is and what `enter` does with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Header(String),
    Session(String),
    Tab {
        session: String,
        tab: String,
    },
    Pane {
        session: String,
        tab: String,
        pane: String,
    },
    Agent {
        session: String,
        tab: String,
        pane: String,
        blocked: bool,
        name: String,
    },
}

#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RowKind,
    pub depth: usize,
    pub left: Spans,
    pub right: Spans,
}

impl RowKind {
    /// Where `enter` (or a click) goes.
    pub fn target(&self) -> Option<NavTarget> {
        match self {
            RowKind::Header(_) => None,
            RowKind::Session(s) => Some(NavTarget::Session(s.clone())),
            RowKind::Tab { session, tab } => Some(NavTarget::Tab {
                session: session.clone(),
                tab: tab.clone(),
            }),
            RowKind::Pane { session, tab, pane }
            | RowKind::Agent {
                session, tab, pane, ..
            } => Some(NavTarget::Pane {
                session: session.clone(),
                tab: tab.clone(),
                pane: pane.clone(),
            }),
        }
    }
}

/// `4m12s`, `12s`, `1h04m`.
pub fn elapsed(since_ms: u64, now: u64) -> String {
    let s = now.saturating_sub(since_ms) / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Agent states in the order the `agents` section lists them.
const AGENT_ORDER: [&str; 4] = ["blocked", "error", "working", "done"];

/// One agent as the `agents` section shows it.
#[derive(Debug, Clone)]
struct AgentRow {
    session: String,
    tab: String,
    pane: String,
    name: String,
    state: String,
    since: Option<u64>,
}

fn agents(dir: &Directory) -> Vec<AgentRow> {
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
                if !AGENT_ORDER.contains(&state.as_str()) {
                    continue;
                }
                out.push(AgentRow {
                    session: s.summary.name.clone(),
                    tab: t.id.clone(),
                    pane: p.clone(),
                    name: info
                        .meta
                        .get("agent.name")
                        .cloned()
                        .unwrap_or_else(|| info.fg_cmd.clone()),
                    state: state.clone(),
                    since: info.meta.get("agent.since").and_then(|v| v.parse().ok()),
                });
            }
        }
    }
    let rank = |s: &str| AGENT_ORDER.iter().position(|x| *x == s).unwrap_or(9);
    out.sort_by(|a, b| {
        rank(&a.state)
            .cmp(&rank(&b.state))
            .then(a.since.cmp(&b.since))
            .then(a.pane.cmp(&b.pane))
    });
    out
}

/// The panes `agents` lists, for fetching their last lines.
pub fn agent_panes(dir: &Directory) -> Vec<String> {
    agents(dir).into_iter().map(|a| a.pane).collect()
}

/// `◐3 ■1 ✓1`: a collapsed `agents` header's summary.
fn agent_counts(dir: &Directory) -> Spans {
    let all = agents(dir);
    let mut out = Vec::new();
    for state in ["working", "blocked", "done", "error"] {
        let n = all.iter().filter(|a| a.state == state).count();
        if n == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push(Span::text(" "));
        }
        let (g, style) = glyph(state);
        out.push(Span::new(format!("{g}{n}"), style));
    }
    out
}

/// A pane row's glyph, program and title: `◐ claude  paginate.rs`, `$ fish` for a shell at
/// its prompt.
fn pane_spans(meta: &BTreeMap<String, String>, fg_cmd: &str, title: &str, shell_fg: bool) -> Spans {
    if let Some(state) = meta.get("agent.state").filter(|s| s.as_str() != "idle") {
        let (g, style) = glyph(state);
        let name = meta.get("agent.name").map_or(fg_cmd, String::as_str);
        let mut out = vec![
            Span::new(format!("{g} "), style),
            Span::text(name.to_owned()),
        ];
        if !title.is_empty() && title != name {
            out.push(Span::new(format!("  {title}"), Style::Muted));
        }
        return out;
    }
    if shell_fg {
        return vec![Span::new("$ ", Style::Muted), Span::text(fg_cmd.to_owned())];
    }
    let mut out = vec![Span::new("  ", Style::Muted), Span::text(fg_cmd.to_owned())];
    if !title.is_empty() && title != fg_cmd {
        out.push(Span::new(format!("  {title}"), Style::Muted));
    }
    out
}

/// Every row of every section, collapsed ones down to their header.
pub fn rows(app: &App) -> Vec<Row> {
    let st = &app.panel;
    let mut out = Vec::new();
    for section in &app.cfg.panel_sections {
        let open = !st.collapsed.contains(section);
        let right = match (section.as_str(), open) {
            ("agents", false) => agent_counts(&app.dir),
            _ => Vec::new(),
        };
        out.push(Row {
            kind: RowKind::Header(section.clone()),
            depth: 0,
            left: vec![
                Span::new(if open { "▾ " } else { "▸ " }, Style::Muted),
                Span::new(section.clone(), Style::Title),
            ],
            right,
        });
        if !open {
            continue;
        }
        match section.as_str() {
            "workspace" => workspace(app, &mut out),
            "agents" => agent_rows(app, &mut out),
            _ => {}
        }
    }
    out
}

fn workspace(app: &App, out: &mut Vec<Row>) {
    let st = &app.panel;
    let current = &app.model.session;
    let mut names: Vec<String> = app
        .dir
        .sessions
        .iter()
        .map(|s| s.summary.name.clone())
        .collect();
    // The attached session first (panel.png), the others in the daemon's order.
    names.retain(|n| n != current);
    names.insert(0, current.clone());
    for name in names {
        let attached = name == *current;
        let expanded = attached != st.toggled.contains(&name);
        let label = match (&app.remote, attached) {
            (Some(h), _) => vec![
                Span::new(h.clone(), Style::Accent),
                Span::text(format!(":{name}")),
            ],
            (None, true) => vec![Span::new(name.clone(), Style::Title)],
            (None, false) => vec![Span::text(name.clone())],
        };
        let mut left = vec![Span::new(if expanded { "▾ " } else { "▸ " }, Style::Muted)];
        left.extend(label);
        out.push(Row {
            kind: RowKind::Session(name.clone()),
            depth: 1,
            left,
            right: Vec::new(),
        });
        if !expanded {
            continue;
        }
        if attached {
            // The attached session from the live model.
            for (i, t) in app.model.tabs.iter().enumerate() {
                let mut marks = String::new();
                if app.model.tab_running(t) {
                    marks.push(' ');
                    marks.push_str(&app.cfg.running_indicator);
                }
                if t.urgent {
                    marks.push_str(" !");
                }
                if t.zoomed {
                    marks.push_str(" Z");
                }
                let style = if t.id == app.model.current {
                    Style::Title
                } else if t.urgent {
                    Style::Warn
                } else {
                    Style::Text
                };
                let cwd = app
                    .model
                    .panes
                    .get(&t.focus)
                    .map(|p| p.cwd.clone())
                    .unwrap_or_default();
                out.push(Row {
                    kind: RowKind::Tab {
                        session: name.clone(),
                        tab: t.id.clone(),
                    },
                    depth: 2,
                    left: vec![
                        Span::new(format!("{} {}", i + 1, app.model.tab_title(t)), style),
                        Span::new(marks, Style::Accent),
                    ],
                    right: st.git.branch(&cwd),
                });
                for p in t.panes() {
                    let Some(pane) = app.model.panes.get(&p) else {
                        continue;
                    };
                    let shell_fg = pane.known && !pane.running();
                    out.push(Row {
                        kind: RowKind::Pane {
                            session: name.clone(),
                            tab: t.id.clone(),
                            pane: p.clone(),
                        },
                        depth: 3,
                        left: pane_spans(&pane.meta, &pane.fg_cmd, &pane.title, shell_fg),
                        right: Vec::new(),
                    });
                }
            }
            continue;
        }
        let Some(s) = app.dir.session(&name) else {
            continue;
        };
        for t in &s.tabs {
            let panes = DirSession::tab_panes(t);
            let running = panes.iter().any(|p| {
                s.panes
                    .get(p)
                    .is_some_and(|i| i.pid != 0 && i.fg_pid != i.pid)
            });
            let cwd = s
                .panes
                .get(&t.focus)
                .map(|i| i.cwd.clone())
                .unwrap_or_default();
            let mut marks = String::new();
            if running {
                marks.push(' ');
                marks.push_str(&app.cfg.running_indicator);
            }
            out.push(Row {
                kind: RowKind::Tab {
                    session: name.clone(),
                    tab: t.id.clone(),
                },
                depth: 2,
                left: vec![
                    Span::text(format!("{} {}", t.index + 1, s.tab_title(t))),
                    Span::new(marks, Style::Accent),
                ],
                right: st.git.branch(&cwd),
            });
            for p in panes {
                let Some(i) = s.panes.get(&p) else { continue };
                let shell_fg = i.pid != 0 && i.fg_pid == i.pid;
                out.push(Row {
                    kind: RowKind::Pane {
                        session: name.clone(),
                        tab: t.id.clone(),
                        pane: p.clone(),
                    },
                    depth: 3,
                    left: pane_spans(&i.meta, &i.fg_cmd, &i.title, shell_fg),
                    right: Vec::new(),
                });
            }
        }
    }
}

fn agent_rows(app: &App, out: &mut Vec<Row>) {
    let now = now_ms();
    for a in agents(&app.dir) {
        let (g, style) = glyph(&a.state);
        let mut left = vec![
            Span::new(format!("{g} "), style),
            Span::new(a.name.clone(), style),
            Span::new(
                format!("  {}", a.since.map_or("-".into(), |s| elapsed(s, now))),
                Style::Muted,
            ),
        ];
        if let Some(line) = app.panel.lines.get(&a.pane).filter(|l| !l.is_empty()) {
            left.push(Span::text(format!("  {line}")));
        }
        let blocked = a.state == "blocked";
        out.push(Row {
            kind: RowKind::Agent {
                session: a.session,
                tab: a.tab,
                pane: a.pane,
                blocked,
                name: a.name,
            },
            depth: 1,
            left,
            right: if blocked {
                vec![Span::new("[y] [n]", Style::Warn)]
            } else {
                Vec::new()
            },
        });
    }
}

/// Draws the panel into its area. `app.panel.hits` is refreshed for the mouse.
pub(crate) fn draw(app: &mut App, c: &mut Canvas, theme: &Theme, l: &Layout) {
    let Some(a) = l.panel else { return };
    let rows = rows(app);
    let st = &mut app.panel;
    st.hits.clear();
    let bg = theme.overlay_bg;
    let (x0, w) = (a.x as usize, a.w as usize);
    let (top, h) = (a.y as usize, a.h as usize);
    for y in top..top + h {
        c.fill(x0, y, w.saturating_sub(1), ' ', &attr(theme.text, bg));
        c.put(
            x0 + w - 1,
            y,
            "│",
            &attr(theme.muted, ColorAttribute::Default),
        );
    }
    // The drag handle halfway down the right edge.
    c.put(
        x0 + w - 1,
        top + h / 2,
        "┇",
        &attr(theme.muted, ColorAttribute::Default),
    );
    let inner = w.saturating_sub(3);
    st.selected = st.selected.min(rows.len().saturating_sub(1));
    // Keep the selection in view.
    if st.selected < st.scroll {
        st.scroll = st.selected;
    } else if h > 0 && st.selected >= st.scroll + h {
        st.scroll = st.selected + 1 - h;
    }
    for (i, row) in rows.iter().enumerate().skip(st.scroll).take(h) {
        let y = top + i - st.scroll;
        let selected = st.focused && i == st.selected;
        let rbg = if selected { theme.selection } else { bg };
        if selected {
            c.fill(x0, y, w.saturating_sub(1), ' ', &attr(theme.text, rbg));
        }
        let indent = 1 + 2 * row.depth.saturating_sub(1).min(3);
        let indent = if matches!(row.kind, RowKind::Header(_)) {
            1
        } else {
            indent + 1
        };
        let right_w = femux_plugin::width(&row.right);
        let room = inner.saturating_sub(indent);
        let left_room = if right_w > 0 && right_w + 2 <= room {
            room - right_w - 1
        } else {
            room
        };
        let left = crate::render::clip(&row.left, left_room);
        let end = c.spans(x0 + indent, y, &left, theme, rbg);
        if right_w > 0 && right_w + 2 <= room {
            let rx = x0 + 1 + inner - right_w;
            if rx > end {
                c.spans(rx, y, &row.right, theme, rbg);
            }
        }
        st.hits.push((y as u16, i));
    }
}

/// A repository's branch and how many commits it is ahead of its upstream.
type Branch = Option<(String, u32)>;

/// Branches of the repositories tabs are in, refreshed off the UI thread.
#[derive(Debug, Clone, Default)]
pub struct GitCache {
    /// cwd -> its branch; `None`: not a repository.
    known: Arc<Mutex<BTreeMap<String, Branch>>>,
    asked: BTreeMap<String, Instant>,
}

impl GitCache {
    /// `main +3`, or nothing outside a repository (or before the first answer).
    pub fn branch(&self, cwd: &str) -> Spans {
        let known = self.known.lock().unwrap();
        match known.get(cwd) {
            Some(Some((b, ahead))) => {
                let mut out = vec![Span::new(b.clone(), Style::Muted)];
                if *ahead > 0 {
                    out.push(Span::new(format!(" +{ahead}"), Style::Warn));
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// Asks `git` about each of `cwds` at most every 5 s, in the background.
    pub fn refresh(&mut self, cwds: impl IntoIterator<Item = String>, now: Instant) {
        for cwd in cwds {
            if cwd.is_empty()
                || self
                    .asked
                    .get(&cwd)
                    .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(5))
            {
                continue;
            }
            self.asked.insert(cwd.clone(), now);
            let known = self.known.clone();
            std::thread::spawn(move || {
                let got = git_branch(&cwd);
                known.lock().unwrap().insert(cwd, got);
            });
        }
    }
}

/// The branch and how far ahead of its upstream it is, from `git status`.
fn git_branch(cwd: &str) -> Branch {
    let out = std::process::Command::new("git")
        .args(["-C", cwd, "status", "--porcelain=v2", "--branch"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_branch(&String::from_utf8_lossy(&out.stdout))
}

/// `# branch.head main` and `# branch.ab +3 -0` from `git status --porcelain=v2 --branch`.
pub fn parse_branch(status: &str) -> Branch {
    let mut head = None;
    let mut ahead = 0;
    for line in status.lines() {
        if let Some(h) = line.strip_prefix("# branch.head ") {
            head = Some(h.trim().to_owned());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            ahead = ab
                .split_whitespace()
                .next()
                .and_then(|a| a.trim_start_matches('+').parse().ok())
                .unwrap_or(0);
        }
    }
    head.map(|h| (h, ahead))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_and_elapsed_times_read_compactly() {
        assert_eq!(
            parse_branch(
                "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +3 -0\n"
            ),
            Some(("main".into(), 3))
        );
        assert_eq!(
            parse_branch("# branch.head work\n"),
            Some(("work".into(), 0))
        );
        assert_eq!(parse_branch(""), None);
        assert_eq!(elapsed(0, 12_000), "12s");
        assert_eq!(elapsed(0, 252_000), "4m12s");
        assert_eq!(elapsed(0, 3_840_000), "1h04m");
    }
}
