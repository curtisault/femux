//! The client's picture of its session: tabs in order, panes with their screens and process
//! info, marks and focus. Built from the attach snapshot, kept current by events and frames
//! (REQ-ARCH-002: nothing here comes from anywhere but the protocol).

use std::collections::{BTreeMap, HashMap};

use femux_proto::Event;
use femux_proto::msg as m;

/// One pane's cells as received in frames.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaneScreen {
    pub rect: m::Rect,
    pub rows: Vec<Vec<Option<m::Cell>>>,
    pub cursor: m::Cursor,
}

impl PaneScreen {
    pub fn apply(&mut self, pf: &m::PaneFrame) {
        self.rect = pf.rect.clone();
        self.cursor = pf.cursor.clone();
        let (w, h) = (pf.rect.w as usize, pf.rect.h as usize);
        self.rows.resize_with(h, Vec::new);
        for r in &mut self.rows {
            r.resize(w, None);
        }
        for run in &pf.cells {
            let Some(row) = self.rows.get_mut(run.y as usize) else {
                continue;
            };
            let mut x = run.x as usize;
            for c in &run.run {
                if x < row.len() {
                    row[x] = Some(c.clone());
                }
                if c.width == 2 && x + 1 < row.len() {
                    row[x + 1] = None;
                }
                x += c.width.max(1) as usize;
            }
        }
    }

    /// Plain text of row `y`, trailing blanks trimmed.
    pub fn row_text(&self, y: usize) -> String {
        let Some(row) = self.rows.get(y) else {
            return String::new();
        };
        let mut s = String::new();
        for c in row.iter().flatten() {
            if c.ch.is_empty() {
                s.push(' ');
            } else {
                s.push_str(&c.ch);
            }
        }
        s.trim_end().to_owned()
    }
}

/// `~` for the home directory, else the last path component (`/` for the root).
pub fn short_dir(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    if let Some(home) = std::env::var_os("HOME")
        && std::path::Path::new(cwd) == std::path::Path::new(&home)
    {
        return "~".into();
    }
    cwd.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("/")
        .to_owned()
}

#[derive(Debug, Clone, Default)]
pub struct Pane {
    pub id: String,
    pub tab: String,
    /// The pane's own process (its shell); 0 until `pane.info` answers.
    pub pid: u32,
    pub fg_pid: u32,
    pub fg_cmd: String,
    pub cwd: String,
    pub title: String,
    pub cmd: Option<String>,
    pub meta: BTreeMap<String, String>,
    pub screen: Option<PaneScreen>,
    /// Came back from a snapshot as a plain shell; no command has run since (REQ-STORE-008).
    pub restored: bool,
    /// `pane.info` has answered at least once.
    pub known: bool,
}

impl Pane {
    /// REQ-UI-020: a program other than the shell is in the foreground.
    pub fn running(&self) -> bool {
        self.known && self.fg_pid != 0 && self.fg_pid != self.pid
    }
}

#[derive(Debug, Clone, Default)]
pub struct Tab {
    pub id: String,
    pub custom_title: Option<String>,
    pub tree: Option<m::LayoutNode>,
    pub floats: Vec<m::FloatNode>,
    pub zoomed: bool,
    pub scheme: m::Scheme,
    pub focus: String,
    /// Bell or agent state change while not the current tab (REQ-UI-020).
    pub urgent: bool,
    /// Typing goes to every pane (ADR-022).
    pub broadcast: bool,
    /// The floats are hidden (ADR-022).
    pub floats_hidden: bool,
}

/// Panes of a layout tree in order.
pub fn tree_panes(n: &m::LayoutNode, out: &mut Vec<String>) {
    match n {
        m::LayoutNode::PaneNode(p) => out.push(p.id.clone()),
        m::LayoutNode::SplitNode(s) => {
            for c in &s.children {
                tree_panes(c, out);
            }
        }
    }
}

impl Tab {
    pub fn tiled(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(t) = &self.tree {
            tree_panes(t, &mut out);
        }
        out
    }

    pub fn panes(&self) -> Vec<String> {
        let mut out = self.tiled();
        out.extend(self.floats.iter().map(|f| f.id.clone()));
        out
    }

    pub fn is_float(&self, pane: &str) -> bool {
        self.floats.iter().any(|f| f.id == pane)
    }
}

/// Something the model needs its owner to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Ask `pane.info` for this pane.
    NeedInfo(String),
    /// The session or our view is gone.
    Exit,
    /// The server clipboard changed.
    Clipboard(String),
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub session: String,
    pub view_id: String,
    pub tabs: Vec<Tab>,
    pub panes: HashMap<String, Pane>,
    pub marks: BTreeMap<String, String>,
    pub current: String,
}

impl Model {
    pub fn from_attach(a: &m::SessionAttachOk) -> (Model, Vec<Effect>) {
        let mut model = Model {
            session: a.snapshot.session.clone(),
            view_id: a.view_id.clone(),
            current: a.snapshot.current_tab.clone(),
            marks: a.snapshot.marks.clone(),
            ..Default::default()
        };
        let mut effects = Vec::new();
        for t in &a.snapshot.tabs {
            let tab = Tab {
                id: t.id.clone(),
                custom_title: t.custom_title.clone(),
                tree: Some(t.tree.clone()),
                floats: t.floats.clone(),
                zoomed: t.zoomed,
                scheme: t.scheme,
                focus: t.focus.clone(),
                urgent: false,
                broadcast: t.broadcast.unwrap_or(false),
                floats_hidden: t.floats_hidden.unwrap_or(false),
            };
            for p in tab.panes() {
                model.panes.insert(
                    p.clone(),
                    Pane {
                        id: p.clone(),
                        tab: t.id.clone(),
                        ..Default::default()
                    },
                );
                effects.push(Effect::NeedInfo(p));
            }
            model.tabs.push(tab);
        }
        (model, effects)
    }

    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn tab_mut(&mut self, id: &str) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn current_tab(&self) -> Option<&Tab> {
        self.tab(&self.current)
    }

    pub fn tab_index(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn focused(&self) -> Option<&Pane> {
        self.current_tab().and_then(|t| self.panes.get(&t.focus))
    }

    pub fn focused_id(&self) -> Option<String> {
        self.current_tab().map(|t| t.focus.clone())
    }

    /// REQ-UI-021: user rename > OSC 2 > fg command > cwd basename. While the shell itself
    /// is in the foreground, its OSC 2 title (often `~ - fish`) gives way to `<shell> <dir>`.
    pub fn tab_title(&self, tab: &Tab) -> String {
        if let Some(t) = &tab.custom_title {
            return t.clone();
        }
        let Some(p) = self.panes.get(&tab.focus) else {
            return String::new();
        };
        let dir = short_dir(&p.cwd);
        if p.known && !p.running() && !p.fg_cmd.is_empty() {
            return if dir.is_empty() {
                p.fg_cmd.clone()
            } else {
                format!("{} {dir}", p.fg_cmd)
            };
        }
        if !p.title.is_empty() {
            return p.title.clone();
        }
        if !p.fg_cmd.is_empty() {
            return p.fg_cmd.clone();
        }
        dir
    }

    /// Whether any pane of `tab` is a restored shell (REQ-STORE-008).
    pub fn tab_restored(&self, tab: &Tab) -> bool {
        tab.panes()
            .iter()
            .any(|p| self.panes.get(p).is_some_and(|p| p.restored))
    }

    pub fn tab_running(&self, tab: &Tab) -> bool {
        tab.panes()
            .iter()
            .any(|p| self.panes.get(p).is_some_and(Pane::running))
    }

    pub fn on_pane_info(&mut self, info: &m::PaneInfoOk) {
        let pane = self.panes.entry(info.id.clone()).or_insert_with(|| Pane {
            id: info.id.clone(),
            ..Default::default()
        });
        pane.tab = info.tab.clone();
        pane.pid = info.pid;
        pane.fg_pid = info.fg_pid;
        pane.fg_cmd = info.fg_cmd.clone();
        pane.cwd = info.cwd.clone();
        pane.title = info.title.clone();
        pane.cmd = info.cmd.clone();
        pane.meta = info.meta.clone();
        pane.restored = info.restored;
        pane.known = true;
    }

    /// Applies a frame: screens of the panes it carries; a full frame drops the rest.
    pub fn on_frame(&mut self, f: &m::Frame) {
        if f.full {
            for p in self.panes.values_mut() {
                if !f.panes.iter().any(|pf| pf.pane_id == p.id) {
                    p.screen = None;
                }
            }
        }
        for pf in &f.panes {
            let pane = self
                .panes
                .entry(pf.pane_id.clone())
                .or_insert_with(|| Pane {
                    id: pf.pane_id.clone(),
                    tab: f.tab.id.clone(),
                    ..Default::default()
                });
            pane.screen.get_or_insert_default().apply(pf);
        }
        if self.current != f.tab.id {
            self.current = f.tab.id.clone();
            if let Some(t) = self.tab_mut(&f.tab.id) {
                t.urgent = false;
            }
        }
    }

    pub fn on_event(&mut self, e: &Event) -> Vec<Effect> {
        let mut fx = Vec::new();
        match e {
            Event::TabCreated(t) => {
                if self.tab(&t.tab_id).is_none() {
                    let at = (t.index as usize).min(self.tabs.len());
                    self.tabs.insert(
                        at,
                        Tab {
                            id: t.tab_id.clone(),
                            ..Default::default()
                        },
                    );
                }
            }
            Event::TabSelected(t) if t.view_id == self.view_id => {
                self.current = t.tab_id.clone();
                if let Some(tab) = self.tab_mut(&t.tab_id) {
                    tab.urgent = false;
                }
            }
            Event::TabClosed(t) => {
                self.tabs.retain(|x| x.id != t.tab_id);
                self.panes.retain(|_, p| p.tab != t.tab_id);
                if self.current == t.tab_id
                    && let Some(first) = self.tabs.first()
                {
                    self.current = first.id.clone();
                }
            }
            Event::TabMoved(t) => {
                if let Some(i) = self.tab_index(&t.tab_id) {
                    let tab = self.tabs.remove(i);
                    let at = (t.index as usize).min(self.tabs.len());
                    self.tabs.insert(at, tab);
                }
            }
            Event::TabFlags(f) => {
                if let Some(tab) = self.tab_mut(&f.tab_id) {
                    tab.broadcast = f.broadcast;
                    tab.floats_hidden = f.floats_hidden;
                }
            }
            Event::TabRenamed(t) => {
                if let Some(tab) = self.tab_mut(&t.tab_id) {
                    tab.custom_title = Some(t.title.clone());
                }
            }
            Event::LayoutChanged(l) => {
                if let Some(tab) = self.tab_mut(&l.tab_id) {
                    tab.tree = Some(l.tree.clone());
                    tab.floats = l.floats.clone();
                    tab.zoomed = l.zoomed;
                    tab.scheme = l.scheme;
                    // A closed pane leaves the layout without a pane.exited of its own.
                    let keep = tab.panes();
                    self.panes
                        .retain(|id, p| p.tab != l.tab_id || keep.contains(id));
                    self.marks.retain(|_, p| self.panes.contains_key(p));
                }
            }
            Event::LayoutFocused(l) => {
                if let Some(tab) = self.tab_mut(&l.tab_id) {
                    tab.focus = l.pane_id.clone();
                }
            }
            Event::PaneSpawned(p) => {
                let pane = self.panes.entry(p.pane_id.clone()).or_insert_with(|| Pane {
                    id: p.pane_id.clone(),
                    ..Default::default()
                });
                pane.tab = p.tab_id.clone();
                pane.known = false;
                if let Some(tab) = self.tab_mut(&p.tab_id) {
                    if tab.tree.is_none() {
                        tab.tree = Some(m::LayoutNode::PaneNode(m::PaneNode {
                            id: p.pane_id.clone(),
                        }));
                    }
                    if tab.focus.is_empty() {
                        tab.focus = p.pane_id.clone();
                    }
                }
                fx.push(Effect::NeedInfo(p.pane_id.clone()));
            }
            Event::PaneExited(p) => {
                self.panes.remove(&p.pane_id);
                self.marks.retain(|_, v| *v != p.pane_id);
            }
            Event::PaneFgChanged(p) => {
                if let Some(pane) = self.panes.get_mut(&p.pane_id) {
                    pane.fg_pid = p.pid;
                    pane.fg_cmd = p.cmd.clone();
                    // A command ran in it: no longer just a restored shell (ADR-017 #2).
                    if pane.known && pane.pid != 0 && p.pid != pane.pid {
                        pane.restored = false;
                    }
                }
            }
            Event::PaneCwdChanged(p) => {
                if let Some(pane) = self.panes.get_mut(&p.pane_id) {
                    pane.cwd = p.cwd.clone();
                }
            }
            Event::PaneTitleChanged(p) => {
                if let Some(pane) = self.panes.get_mut(&p.pane_id) {
                    pane.title = p.title.clone();
                }
            }
            Event::PaneMetaChanged(p) => {
                let agent = p.set.contains_key("agent.state");
                if let Some(pane) = self.panes.get_mut(&p.pane_id) {
                    for k in &p.unset {
                        pane.meta.remove(k);
                    }
                    pane.meta.extend(p.set.clone());
                    let tab = pane.tab.clone();
                    if agent {
                        self.mark_urgent(&tab);
                    }
                }
            }
            Event::PaneBell(p) => {
                if let Some(tab) = self.panes.get(&p.pane_id).map(|p| p.tab.clone()) {
                    self.mark_urgent(&tab);
                }
            }
            Event::PaneMarked(p) if p.cleared == Some(true) => {
                if self.marks.get(&p.mark) == Some(&p.pane_id) {
                    self.marks.remove(&p.mark);
                }
            }
            Event::PaneMarked(p) => {
                self.marks.insert(p.mark.clone(), p.pane_id.clone());
            }
            Event::SessionRenamed(s) if s.name == self.session => {
                self.session = s.new.clone();
            }
            Event::SessionClosed(s) if s.name == self.session => fx.push(Effect::Exit),
            Event::ViewDetached(v) if v.view_id == self.view_id => fx.push(Effect::Exit),
            Event::ClipboardChanged(c) => fx.push(Effect::Clipboard(c.text.clone())),
            _ => {}
        }
        fx
    }

    fn mark_urgent(&mut self, tab: &str) {
        if tab != self.current
            && let Some(t) = self.tab_mut(tab)
        {
            t.urgent = true;
        }
    }

    /// The tab a pane belongs to.
    pub fn tab_of(&self, pane: &str) -> Option<&Tab> {
        let t = &self.panes.get(pane)?.tab;
        self.tab(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_are_inserted_at_the_daemons_index() {
        let mut model = Model::default();
        for (id, index) in [("A", 0), ("B", 1), ("N", 0), ("Z", 9)] {
            model.on_event(&Event::TabCreated(m::TabCreated {
                session: "s".into(),
                tab_id: id.into(),
                index,
                ..Default::default()
            }));
        }
        let order: Vec<&str> = model.tabs.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(order, ["N", "A", "B", "Z"]);
    }
}
