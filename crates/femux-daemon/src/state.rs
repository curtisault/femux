//! The session model (REQ-SESS-*) and everything the request handlers mutate.
//!
//! One `State` sits behind one mutex. Pane output is processed by per-pane tasks that lock only
//! that pane's terminal; lock order is always State, then Bus, then a pane's terminal.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use femux_proto::msg::{self as m, Scheme, SplitDir};
use femux_vt::{HostCaps, Terminal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::bus::{Bus, Scope, now_ms};
use crate::frames::FrameState;
use crate::layout::{Node, PaneId, Rect};

pub type TabId = String;
pub type ViewId = String;

pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

pub struct Session {
    pub name: String,
    pub root: PathBuf,
    pub created: u64,
    pub tabs: Vec<TabId>,
    /// REQ-SESS-004: mark letter -> pane.
    pub marks: BTreeMap<char, PaneId>,
    /// Host terminal of the most recently attached client (REQ-QRY-002).
    pub host: Option<HostCaps>,
}

pub struct Tab {
    pub id: TabId,
    pub session: String,
    pub title: Option<String>,
    pub tree: Option<Node>,
    pub floats: Vec<(PaneId, Rect)>,
    pub zoomed: bool,
    pub focus: PaneId,
    pub scheme: Scheme,
    pub size: (u16, u16),
    /// Visible panes and their rectangles as last applied.
    pub rects: Vec<(PaneId, Rect)>,
    /// Bumped when `rects` change; frames for this tab then go full.
    pub layout_gen: u64,
}

impl Tab {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = self.tree.as_ref().map(Node::panes).unwrap_or_default();
        out.extend(self.floats.iter().map(|(p, _)| p.clone()));
        out
    }

    pub fn area(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.size.0,
            h: self.size.1,
        }
    }

    /// Rectangles of every pane (tiled, then floats clipped to the tab), ignoring zoom.
    /// The scheme (REQ-UI-010) decides how the tiled panes are arranged: `bsp` follows the
    /// tree, `master` and `scroll` derive from the tree's pane order.
    pub fn all_rects(&self) -> Vec<(PaneId, Rect)> {
        let area = self.area();
        let mut out = match (&self.tree, self.scheme) {
            (None, _) => Vec::new(),
            (Some(t), Scheme::Bsp) => t.rects(area),
            (Some(t), Scheme::Master) => crate::layout::master_rects(&t.panes(), area),
            (Some(t), Scheme::Scroll) => crate::layout::scroll_rects(&t.panes(), &self.focus, area),
        };
        for (p, r) in &self.floats {
            let x = r.x.min(area.w.saturating_sub(1));
            let y = r.y.min(area.h.saturating_sub(1));
            let w = r.w.clamp(1, area.w.saturating_sub(x).max(1));
            let h = r.h.clamp(1, area.h.saturating_sub(y).max(1));
            out.push((p.clone(), Rect { x, y, w, h }));
        }
        out
    }

    /// Rectangles of the panes a view shows (REQ-SESS-005: zoom shows only the focused pane).
    pub fn visible_rects(&self) -> Vec<(PaneId, Rect)> {
        if self.zoomed {
            vec![(self.focus.clone(), self.area())]
        } else {
            self.all_rects()
        }
    }

    pub fn layout_event(&self) -> m::LayoutChanged {
        m::LayoutChanged {
            tab_id: self.id.clone(),
            tree: self.tree.as_ref().map(Node::to_proto).unwrap_or_default(),
            floats: self.floats_proto(),
            zoomed: self.zoomed,
            scheme: self.scheme,
            ..Default::default()
        }
    }

    pub fn floats_proto(&self) -> Vec<m::FloatNode> {
        self.floats
            .iter()
            .map(|(p, r)| m::FloatNode {
                id: p.clone(),
                rect: (*r).into(),
            })
            .collect()
    }
}

pub struct Pane {
    pub id: PaneId,
    pub tab: TabId,
    pub vt: Arc<Mutex<Terminal>>,
    pub input: mpsc::UnboundedSender<Vec<u8>>,
    pub master: Arc<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>>,
    pub pid: u32,
    /// Incremented by respawn so a previous process's exit is ignored.
    pub spawn_gen: u64,
    pub size: (u16, u16),
    pub cmd: Option<String>,
    pub title: String,
    pub osc_cwd: Option<String>,
    pub cwd: String,
    pub fg_pid: u32,
    pub fg_cmd: String,
    pub meta: BTreeMap<String, String>,
    pub tasks: Vec<JoinHandle<()>>,
    /// Number of times the pane's PTY was resized (observable in tests for REQ-VIEW-004).
    pub resizes: u64,
}

impl Pane {
    pub fn info(&self, session: &str) -> m::PaneInfoOk {
        m::PaneInfoOk {
            id: self.id.clone(),
            tab: self.tab.clone(),
            session: session.to_owned(),
            pid: self.pid,
            cmd: self.cmd.clone(),
            cwd: self.cwd.clone(),
            fg_pid: self.fg_pid,
            fg_cmd: self.fg_cmd.clone(),
            title: self.title.clone(),
            exit: None,
            meta: self.meta.clone(),
        }
    }
}

pub struct View {
    pub id: ViewId,
    pub conn: u64,
    pub session: String,
    pub tab: TabId,
    pub cols: u16,
    pub rows: u16,
    pub frames: FrameState,
}

#[derive(Default)]
pub struct ConnInfo {
    pub caps: Option<m::TermCaps>,
}

#[derive(Default)]
pub struct State {
    pub sessions: BTreeMap<String, Session>,
    pub tabs: HashMap<TabId, Tab>,
    pub panes: HashMap<PaneId, Pane>,
    pub views: HashMap<ViewId, View>,
    pub conns: HashMap<u64, ConnInfo>,
    pub pending_resize: HashMap<TabId, Instant>,
    /// LIFE-002 applies only after the daemon has had a session.
    pub had_session: bool,
}

/// A handler error: an error code plus message.
pub type Fail = (m::ErrorCode, String);
pub type R<T> = Result<T, Fail>;

pub fn not_found(what: impl std::fmt::Display) -> Fail {
    (m::ErrorCode::NotFound, format!("no such {what}"))
}

pub fn invalid(msg: impl Into<String>) -> Fail {
    (m::ErrorCode::Invalid, msg.into())
}

pub fn conflict(msg: impl Into<String>) -> Fail {
    (m::ErrorCode::Conflict, msg.into())
}

impl State {
    pub fn session(&self, name: &str) -> R<&Session> {
        self.sessions
            .get(name)
            .ok_or_else(|| not_found(format_args!("session {name:?}")))
    }

    pub fn tab(&self, id: &str) -> R<&Tab> {
        self.tabs
            .get(id)
            .ok_or_else(|| not_found(format_args!("tab {id}")))
    }

    pub fn tab_mut(&mut self, id: &str) -> R<&mut Tab> {
        self.tabs
            .get_mut(id)
            .ok_or_else(|| not_found(format_args!("tab {id}")))
    }

    pub fn pane(&self, id: &str) -> R<&Pane> {
        self.panes
            .get(id)
            .ok_or_else(|| not_found(format_args!("pane {id}")))
    }

    pub fn view(&self, id: &str) -> R<&View> {
        self.views
            .get(id)
            .ok_or_else(|| not_found(format_args!("view {id}")))
    }

    pub fn view_mut(&mut self, id: &str) -> R<&mut View> {
        self.views
            .get_mut(id)
            .ok_or_else(|| not_found(format_args!("view {id}")))
    }

    pub fn session_of_pane(&self, pane: &str) -> Option<String> {
        let tab = &self.panes.get(pane)?.tab;
        Some(self.tabs.get(tab)?.session.clone())
    }

    pub fn pane_scope(&self, pane: &str) -> Scope {
        Scope {
            session: self.session_of_pane(pane),
            pane: Some(pane.to_owned()),
        }
    }

    pub fn session_scope(name: &str) -> Scope {
        Scope {
            session: Some(name.to_owned()),
            pane: None,
        }
    }

    /// The tab size implied by its viewers (REQ-VIEW-002/003), or `None` if nobody views it.
    pub fn viewer_size(&self, tab: &str) -> Option<(u16, u16)> {
        self.views
            .values()
            .filter(|v| v.tab == tab)
            .map(|v| (v.cols.max(1), v.rows.max(1)))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)))
    }

    /// Title for the tab strip: user rename > OSC 2 > fg command > cwd basename (REQ-UI-021).
    pub fn tab_title(&self, tab: &Tab) -> String {
        if let Some(t) = &tab.title {
            return t.clone();
        }
        let Some(p) = self.panes.get(&tab.focus) else {
            return String::new();
        };
        if !p.title.is_empty() {
            return p.title.clone();
        }
        if !p.fg_cmd.is_empty() {
            return p.fg_cmd.clone();
        }
        p.cwd
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("/")
            .to_owned()
    }

    pub fn snapshot(&self, session: &str, current_tab: &str) -> R<m::SessionSnapshot> {
        let s = self.session(session)?;
        let tabs = s
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, id)| self.tabs.get(id).map(|t| (i, t)))
            .map(|(i, t)| m::TabSnapshot {
                id: t.id.clone(),
                title: self.tab_title(t),
                custom_title: t.title.clone(),
                index: i as u32,
                tree: t.tree.as_ref().map(Node::to_proto).unwrap_or_default(),
                floats: t.floats_proto(),
                zoomed: t.zoomed,
                focus: t.focus.clone(),
                scheme: t.scheme,
            })
            .collect();
        let marks = s
            .marks
            .iter()
            .map(|(k, p)| (k.to_string(), p.clone()))
            .collect();
        Ok(m::SessionSnapshot {
            session: session.to_owned(),
            tabs,
            current_tab: current_tab.to_owned(),
            marks,
        })
    }
}

/// Emits `event` on `bus` with `scope` (REQ-SESS-003: called after the mutation is applied).
pub fn emit(bus: &Mutex<Bus>, event: impl Into<femux_proto::Event>, scope: Scope) {
    bus.lock().unwrap().emit(event, scope);
}

/// Default ratio for a new split.
pub fn half() -> crate::layout::Ratio {
    crate::layout::Ratio::HALF
}

/// `SplitDir` and whether the new pane goes first, from a preselected direction.
pub fn preselect(dir: m::Direction) -> (SplitDir, bool) {
    match dir {
        m::Direction::H => (SplitDir::H, true),
        m::Direction::L => (SplitDir::H, false),
        m::Direction::K => (SplitDir::V, true),
        m::Direction::J => (SplitDir::V, false),
    }
}

pub fn created_now() -> u64 {
    now_ms()
}
