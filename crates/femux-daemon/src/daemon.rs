//! The daemon core: configuration, pane processes and the request handlers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use femux_proto::msg::{self as m, Response, SplitDir};
use femux_proto::{Request, feature_of};
use femux_vt::{HostCaps, Terminal};
use nix::sys::signal::Signal;
use tokio::sync::{Notify, mpsc, watch};

use crate::bus::{Bus, Scope};
use crate::frames::FrameState;
use crate::layout::{Node, PaneId, Ratio, Rect, neighbor};
use crate::pty::{self, SpawnSpec};
use crate::state::*;

/// A pane terminal shared between its reader task and the request handlers.
pub type SharedTerminal = Arc<Mutex<Terminal>>;

/// Daemon configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Server name (`[daemon] name`).
    pub name: String,
    pub socket: PathBuf,
    pub shell: String,
    /// Start shells as login shells (`-sh` argv0).
    pub login_shell: bool,
    /// `TERM` for new panes (REQ-PTY-007).
    pub term: String,
    pub scrollback_lines: usize,
    pub keep_alive: bool,
    pub resize_debounce: Duration,
    pub events_buffer: usize,
    pub events_replay: usize,
    pub home: PathBuf,
    /// Extra environment for every pane (tests use it to pin PS1 etc.).
    pub env: Vec<(String, String)>,
}

impl Config {
    pub fn new(name: &str) -> Config {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/".into());
        Config {
            name: name.to_owned(),
            socket: femux_proto::socket_path(name),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            login_shell: true,
            term: "xterm-256color".into(),
            scrollback_lines: 10_000,
            keep_alive: false,
            resize_debounce: Duration::from_millis(50),
            events_buffer: 4096,
            events_replay: 1000,
            home,
            env: Vec::new(),
        }
    }
}

/// Message families this daemon serves (REQ-PROTO-020). `clipboard` and `store` arrive later.
pub const SERVED: &[&str] = &[
    "events",
    "frame",
    "layout",
    "pane",
    "scrollback",
    "server",
    "session",
    "tab",
    "view",
];

pub struct Daemon {
    pub cfg: Config,
    pub state: Mutex<State>,
    pub bus: Mutex<Bus>,
    /// Bumped whenever something visible may have changed; frame pumps watch it.
    pub epoch: watch::Sender<u64>,
    pub resize_kick: Notify,
    pub shutdown: watch::Sender<bool>,
    pub started: Instant,
}

/// Per-connection context passed to handlers.
pub struct ConnCtx {
    pub id: u64,
    pub features: Vec<String>,
}

fn host_caps(t: &m::TermCaps) -> HostCaps {
    let d = HostCaps::default();
    HostCaps {
        fg: t.fg.map_or(d.fg, |[r, g, b]| (r, g, b)),
        bg: t.bg.map_or(d.bg, |[r, g, b]| (r, g, b)),
        cursor: t.cursor.map(|[r, g, b]| (r, g, b)),
    }
}

fn keymods(mods: &[m::Mod]) -> femux_vt::Mods {
    let mut out = femux_vt::Mods::default();
    for md in mods {
        match md {
            m::Mod::Shift => out.shift = true,
            m::Mod::Ctrl => out.ctrl = true,
            m::Mod::Alt => out.alt = true,
            m::Mod::Super => out.super_ = true,
        }
    }
    out
}

/// Strips a `file://host` prefix from an OSC 7 URL.
fn osc7_path(url: &str) -> String {
    match url.strip_prefix("file://") {
        Some(rest) => rest
            .find('/')
            .map_or(rest.to_owned(), |i| rest[i..].to_owned()),
        None => url.to_owned(),
    }
}

impl Daemon {
    pub fn new(cfg: Config) -> Arc<Daemon> {
        let bus = Bus::new(cfg.events_buffer, cfg.events_replay);
        Arc::new(Daemon {
            cfg,
            state: Mutex::new(State::default()),
            bus: Mutex::new(bus),
            epoch: watch::channel(0).0,
            resize_kick: Notify::new(),
            shutdown: watch::channel(false).0,
            started: Instant::now(),
        })
    }

    pub fn dirty(&self) {
        self.epoch.send_modify(|e| *e += 1);
    }

    fn emit(&self, e: impl Into<femux_proto::Event>, scope: Scope) {
        emit(&self.bus, e, scope);
    }

    // ---------------------------------------------------------------- panes

    fn pane_env(&self, pane: &str) -> Vec<(String, String)> {
        let mut env = vec![
            ("TERM".into(), self.cfg.term.clone()),
            ("TERM_PROGRAM".into(), "femux".into()),
            (
                "TERM_PROGRAM_VERSION".into(),
                env!("CARGO_PKG_VERSION").into(),
            ),
            ("FEMUX_PANE".into(), pane.to_owned()),
            (
                "FEMUX_SOCKET".into(),
                self.cfg.socket.to_string_lossy().into_owned(),
            ),
        ];
        env.extend(self.cfg.env.iter().cloned());
        env
    }

    /// Spawns a pane process in `tab` and registers the pane (REQ-PTY-001, REQ-PTY-007).
    fn spawn_pane(
        self: &Arc<Self>,
        st: &mut State,
        tab: &str,
        cwd: &Path,
        cmd: Option<String>,
        size: (u16, u16),
    ) -> R<PaneId> {
        let id = new_id();
        let host = st
            .tabs
            .get(tab)
            .and_then(|t| st.sessions.get(&t.session))
            .and_then(|s| s.host.clone());
        let mut vt = Terminal::new(size.0, size.1, self.cfg.scrollback_lines)
            .map_err(|e| (m::ErrorCode::Internal, e.to_string()))?;
        vt.set_host(&host.unwrap_or_default())
            .map_err(|e| (m::ErrorCode::Internal, e.to_string()))?;
        let cwd = if cwd.is_dir() {
            cwd.to_path_buf()
        } else {
            self.cfg.home.clone()
        };
        let pane = Pane {
            id: id.clone(),
            tab: tab.to_owned(),
            vt: Arc::new(Mutex::new(vt)),
            input: mpsc::unbounded_channel().0,
            master: Arc::new(placeholder_fd()?),
            pid: 0,
            spawn_gen: 0,
            size,
            cmd,
            title: String::new(),
            osc_cwd: None,
            cwd: cwd.to_string_lossy().into_owned(),
            fg_pid: 0,
            fg_cmd: String::new(),
            meta: BTreeMap::new(),
            tasks: Vec::new(),
            resizes: 0,
        };
        st.panes.insert(id.clone(), pane);
        self.start_process(st, &id)?;
        Ok(id)
    }

    /// (Re)starts the process of an existing pane and its I/O tasks.
    fn start_process(self: &Arc<Self>, st: &mut State, id: &str) -> R<()> {
        let env = self.pane_env(id);
        let pane = st.panes.get_mut(id).ok_or_else(|| not_found("pane"))?;
        let spec = SpawnSpec {
            shell: &self.cfg.shell,
            cmd: pane.cmd.as_deref(),
            login: self.cfg.login_shell,
            cwd: Path::new(&pane.cwd),
            env,
            cols: pane.size.0,
            rows: pane.size.1,
        };
        let spawned = pty::spawn(&spec)
            .map_err(|e| (m::ErrorCode::Internal, format!("spawn failed: {e}")))?;
        for t in pane.tasks.drain(..) {
            t.abort();
        }
        pane.spawn_gen += 1;
        pane.pid = spawned.pid;
        pane.fg_pid = spawned.pid;
        pane.fg_cmd = pty::proc_name(spawned.pid).unwrap_or_default();
        let master = Arc::new(spawned.master);
        pane.master = master.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        pane.input = tx.clone();
        let vt = pane.vt.clone();
        let spawn_gen = pane.spawn_gen;
        let id = id.to_owned();
        let me = self.clone();
        pane.tasks.push(tokio::spawn(reader(
            me.clone(),
            id.clone(),
            master.clone(),
            vt,
            tx,
        )));
        pane.tasks.push(tokio::spawn(writer(master.clone(), rx)));
        pane.tasks
            .push(tokio::spawn(poller(me.clone(), id.clone(), master)));
        let mut child = spawned.child;
        // The waiter is not aborted with the other tasks: it reaps the process.
        tokio::spawn(async move {
            let code = match child.wait().await {
                Ok(s) => s.code().unwrap_or_else(|| {
                    use std::os::unix::process::ExitStatusExt;
                    128 + s.signal().unwrap_or(0)
                }),
                Err(_) => -1,
            };
            me.pane_exited(&id, spawn_gen, code);
        });
        Ok(())
    }

    /// Kills a pane's process group and stops its tasks.
    fn kill_pane(pane: &mut Pane) {
        for t in pane.tasks.drain(..) {
            t.abort();
        }
        if pane.pid != 0 {
            let _ = pty::signal_group(pane.pid, Signal::SIGHUP);
        }
    }

    fn pane_exited(self: &Arc<Self>, id: &str, spawn_gen: u64, code: i32) {
        let mut st = self.state.lock().unwrap();
        if st.panes.get(id).is_none_or(|p| p.spawn_gen != spawn_gen) {
            return;
        }
        let scope = st.pane_scope(id);
        self.emit(
            m::PaneExited {
                pane_id: id.to_owned(),
                code,
                ..Default::default()
            },
            scope,
        );
        self.remove_pane(&mut st, id);
        drop(st);
        self.dirty();
    }

    /// Removes a pane from its tab; closes the tab and session when they become empty.
    fn remove_pane(self: &Arc<Self>, st: &mut State, id: &str) {
        let Some(mut pane) = st.panes.remove(id) else {
            return;
        };
        Self::kill_pane(&mut pane);
        let tab_id = pane.tab.clone();
        let Some(tab) = st.tabs.get_mut(&tab_id) else {
            return;
        };
        tab.tree = tab.tree.take().and_then(|t| t.remove(id));
        tab.floats.retain(|(p, _)| p != id);
        let session = tab.session.clone();
        for s in st.sessions.values_mut() {
            s.marks.retain(|_, p| p != id);
        }
        let remaining = st.tabs[&tab_id].panes();
        if remaining.is_empty() {
            self.close_tab(st, &tab_id);
            return;
        }
        let tab = st.tabs.get_mut(&tab_id).unwrap();
        let refocus = tab.focus == id;
        if refocus {
            tab.focus = remaining[0].clone();
            tab.zoomed = false;
        }
        let ev = tab.layout_event();
        self.emit(ev, State::session_scope(&session));
        if refocus {
            let focus = st.tabs[&tab_id].focus.clone();
            self.emit(
                m::LayoutFocused {
                    tab_id: tab_id.clone(),
                    pane_id: focus,
                    ..Default::default()
                },
                State::session_scope(&session),
            );
        }
        self.apply_geometry(st, &tab_id);
    }

    fn close_tab(self: &Arc<Self>, st: &mut State, tab_id: &str) {
        let Some(tab) = st.tabs.remove(tab_id) else {
            return;
        };
        for p in tab.panes() {
            if let Some(mut pane) = st.panes.remove(&p) {
                Self::kill_pane(&mut pane);
            }
        }
        let name = tab.session.clone();
        let Some(session) = st.sessions.get_mut(&name) else {
            return;
        };
        session.tabs.retain(|t| t != tab_id);
        let next = session.tabs.first().cloned();
        self.emit(
            m::TabClosed {
                session: name.clone(),
                tab_id: tab_id.to_owned(),
                ..Default::default()
            },
            State::session_scope(&name),
        );
        match next {
            Some(next) => {
                for v in st.views.values_mut().filter(|v| v.tab == tab_id) {
                    v.tab = next.clone();
                    v.frames.need_full = true;
                }
                self.schedule_resize(st, &next);
            }
            None => self.close_session(st, &name),
        }
    }

    fn close_session(self: &Arc<Self>, st: &mut State, name: &str) {
        let Some(session) = st.sessions.remove(name) else {
            return;
        };
        for t in session.tabs {
            if let Some(tab) = st.tabs.remove(&t) {
                for p in tab.panes() {
                    if let Some(mut pane) = st.panes.remove(&p) {
                        Self::kill_pane(&mut pane);
                    }
                }
            }
        }
        let gone: Vec<String> = st
            .views
            .values()
            .filter(|v| v.session == name)
            .map(|v| v.id.clone())
            .collect();
        for v in gone {
            st.views.remove(&v);
            self.emit(
                m::ViewDetached {
                    view_id: v,
                    session: name.to_owned(),
                    ..Default::default()
                },
                State::session_scope(name),
            );
        }
        self.emit(
            m::SessionClosed {
                name: name.to_owned(),
                ..Default::default()
            },
            State::session_scope(name),
        );
        // REQ-LIFE-002: exit with the last session unless keep_alive.
        if st.sessions.is_empty() && st.had_session && !self.cfg.keep_alive {
            self.shutdown.send_replace(true);
        }
    }

    fn create_tab(
        self: &Arc<Self>,
        st: &mut State,
        session: &str,
        cwd: Option<&str>,
        cmd: Option<String>,
        title: Option<String>,
        index: Option<u32>,
    ) -> R<(String, String)> {
        let s = st.session(session)?;
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| s.root.clone());
        let size = s
            .tabs
            .first()
            .and_then(|t| st.tabs.get(t))
            .map_or((80, 24), |t| t.size);
        let tab_id = new_id();
        st.tabs.insert(
            tab_id.clone(),
            Tab {
                id: tab_id.clone(),
                session: session.to_owned(),
                title,
                tree: None,
                floats: Vec::new(),
                zoomed: false,
                focus: String::new(),
                scheme: m::Scheme::Bsp,
                size,
                rects: Vec::new(),
                layout_gen: 0,
            },
        );
        let pane = match self.spawn_pane(st, &tab_id, &cwd, cmd, size) {
            Ok(p) => p,
            Err(e) => {
                st.tabs.remove(&tab_id);
                return Err(e);
            }
        };
        let tab = st.tabs.get_mut(&tab_id).unwrap();
        tab.tree = Some(Node::Pane(pane.clone()));
        tab.focus = pane.clone();
        let s = st.sessions.get_mut(session).unwrap();
        let at = index.map_or(s.tabs.len(), |i| (i as usize).min(s.tabs.len()));
        s.tabs.insert(at, tab_id.clone());
        let scope = State::session_scope(session);
        self.emit(
            m::TabCreated {
                session: session.to_owned(),
                tab_id: tab_id.clone(),
                ..Default::default()
            },
            scope.clone(),
        );
        self.emit(
            m::PaneSpawned {
                pane_id: pane.clone(),
                tab_id: tab_id.clone(),
                ..Default::default()
            },
            st.pane_scope(&pane),
        );
        self.apply_geometry(st, &tab_id);
        Ok((tab_id, pane))
    }

    // ---------------------------------------------------------------- geometry

    /// Resizes every pane of `tab` to its rectangle; bumps the tab's gen if anything moved.
    pub fn apply_geometry(&self, st: &mut State, tab_id: &str) {
        let Some(tab) = st.tabs.get_mut(tab_id) else {
            return;
        };
        let mut sizes = tab.all_rects();
        if tab.zoomed {
            let area = tab.area();
            for (p, r) in &mut sizes {
                if *p == tab.focus {
                    *r = area;
                }
            }
        }
        let visible = tab.visible_rects();
        if visible != tab.rects {
            tab.rects = visible;
            tab.layout_gen += 1;
        }
        for (p, r) in sizes {
            let Some(pane) = st.panes.get_mut(&p) else {
                continue;
            };
            let size = (r.w.max(1), r.h.max(1));
            if pane.size == size {
                continue;
            }
            pane.size = size;
            pane.resizes += 1;
            let _ = pty::resize(pane.master.as_ref(), size.0, size.1);
            let mut vt = pane.vt.lock().unwrap();
            let _ = vt.resize(size.0, size.1);
            let _ = vt.sync();
        }
    }

    /// Debounces a viewer-driven size change of `tab` (REQ-VIEW-004).
    pub fn schedule_resize(&self, st: &mut State, tab: &str) {
        st.pending_resize
            .insert(tab.to_owned(), Instant::now() + self.cfg.resize_debounce);
        self.resize_kick.notify_one();
    }

    /// Applies due debounced resizes; returns the next deadline.
    pub fn run_resizes(&self) -> Option<Instant> {
        let mut st = self.state.lock().unwrap();
        let now = Instant::now();
        let due: Vec<String> = st
            .pending_resize
            .iter()
            .filter(|(_, d)| **d <= now)
            .map(|(t, _)| t.clone())
            .collect();
        let mut changed = false;
        for t in due {
            st.pending_resize.remove(&t);
            let Some(size) = st.viewer_size(&t) else {
                continue;
            };
            let Some(tab) = st.tabs.get_mut(&t) else {
                continue;
            };
            if tab.size != size {
                tab.size = size;
                self.apply_geometry(&mut st, &t);
                changed = true;
            }
        }
        let next = st.pending_resize.values().min().copied();
        drop(st);
        if changed {
            self.dirty();
        }
        next
    }

    /// Sets the host terminal of a session's panes (REQ-QRY-002).
    fn set_session_host(st: &mut State, session: &str, host: Option<HostCaps>) {
        let Some(s) = st.sessions.get_mut(session) else {
            return;
        };
        s.host = host.clone();
        let caps = host.unwrap_or_default();
        let tabs = s.tabs.clone();
        for t in tabs {
            let Some(tab) = st.tabs.get(&t) else { continue };
            for p in tab.panes() {
                if let Some(pane) = st.panes.get(&p) {
                    let _ = pane.vt.lock().unwrap().set_host(&caps);
                }
            }
        }
    }

    // ---------------------------------------------------------------- connections

    pub fn conn_closed(self: &Arc<Self>, conn: u64) {
        let mut st = self.state.lock().unwrap();
        st.conns.remove(&conn);
        let views: Vec<String> = st
            .views
            .values()
            .filter(|v| v.conn == conn)
            .map(|v| v.id.clone())
            .collect();
        for v in views {
            self.detach(&mut st, &v);
        }
        drop(st);
        self.dirty();
    }

    fn detach(self: &Arc<Self>, st: &mut State, view_id: &str) {
        let Some(view) = st.views.remove(view_id) else {
            return;
        };
        self.emit(
            m::ViewDetached {
                view_id: view.id.clone(),
                session: view.session.clone(),
                ..Default::default()
            },
            State::session_scope(&view.session),
        );
        if !st.views.values().any(|v| v.session == view.session) {
            Self::set_session_host(st, &view.session, None);
        }
        self.schedule_resize(st, &view.tab);
    }

    // ---------------------------------------------------------------- requests

    /// Handles one request (after the handshake). Mutations emit their event after applying.
    pub fn handle(self: &Arc<Self>, conn: &ConnCtx, req: Request) -> R<Response> {
        let kind = req.kind();
        if !SERVED.contains(&feature_of(kind))
            || !conn.features.iter().any(|f| f == feature_of(kind))
        {
            return Err((
                m::ErrorCode::FeatureMissing,
                format!("feature {:?} is not available", feature_of(kind)),
            ));
        }
        let req = match self.handle_terminal(req) {
            Ok(done) => {
                self.dirty();
                return done;
            }
            Err(req) => req,
        };
        let mut st = self.state.lock().unwrap();
        let resp = self.dispatch(&mut st, conn, req);
        drop(st);
        self.dirty();
        resp
    }

    /// A pane's terminal and input queue, taken under the state lock and used without it.
    fn terminal(&self, pane: &str) -> R<(SharedTerminal, mpsc::UnboundedSender<Vec<u8>>)> {
        let st = self.state.lock().unwrap();
        let p = st.pane(pane)?;
        Ok((p.vt.clone(), p.input.clone()))
    }

    /// Requests that only touch one pane's terminal. `Err` hands the request back.
    fn handle_terminal(&self, req: Request) -> Result<R<Response>, Request> {
        use Request as Q;
        let run = || -> R<Response> {
            Ok(match req.clone() {
                Q::PaneInput(r) => {
                    let (vt, input) = self.terminal(&r.pane_id)?;
                    let mut out = Vec::new();
                    {
                        let mut vt = vt.lock().unwrap();
                        for e in &r.events {
                            let bytes = match e {
                                m::InputEvent::KeyEvent(k) => vt.encode_key(&femux_vt::Key {
                                    name: &k.key,
                                    mods: keymods(&k.mods),
                                    text: k.text.as_deref(),
                                    action: match k.event {
                                        m::KeyEventKind::Press => femux_vt::KeyAction::Press,
                                        m::KeyEventKind::Repeat => femux_vt::KeyAction::Repeat,
                                        m::KeyEventKind::Release => femux_vt::KeyAction::Release,
                                    },
                                }),
                                m::InputEvent::MouseEvent(e) => {
                                    vt.encode_mouse(&femux_vt::MouseInput {
                                        x: e.x,
                                        y: e.y,
                                        button: e.button.map(|b| match b {
                                            m::MouseButton::Left => femux_vt::MouseButton::Left,
                                            m::MouseButton::Middle => femux_vt::MouseButton::Middle,
                                            m::MouseButton::Right => femux_vt::MouseButton::Right,
                                        }),
                                        mods: keymods(&e.mods),
                                        action: match e.event {
                                            m::MouseEventKind::Press => {
                                                femux_vt::MouseAction::Press
                                            }
                                            m::MouseEventKind::Release => {
                                                femux_vt::MouseAction::Release
                                            }
                                            m::MouseEventKind::Move => femux_vt::MouseAction::Move,
                                            m::MouseEventKind::WheelUp => {
                                                femux_vt::MouseAction::WheelUp
                                            }
                                            m::MouseEventKind::WheelDown => {
                                                femux_vt::MouseAction::WheelDown
                                            }
                                        },
                                    })
                                }
                                m::InputEvent::Paste(p) => vt.encode_paste(&p.text),
                            };
                            out.extend(bytes.map_err(|e| (m::ErrorCode::Internal, e.to_string()))?);
                        }
                    }
                    if !out.is_empty() {
                        let _ = input.send(out);
                    }
                    Response::PaneInput(m::Empty {})
                }
                Q::ScrollbackRead(r) => {
                    let (vt, _) = self.terminal(&r.pane_id)?;
                    let lines = vt
                        .lock()
                        .unwrap()
                        .read_lines(r.from_line, r.count.min(10_000))
                        .map_err(|e| (m::ErrorCode::Internal, e.to_string()))?;
                    Response::ScrollbackRead(m::ScrollbackReadOk {
                        lines: lines.iter().map(|l| crate::frames::cells(l)).collect(),
                    })
                }
                Q::ScrollbackSearch(r) => {
                    let re = regex::Regex::new(&r.regex)
                        .map_err(|e| invalid(format!("bad regex: {e}")))?;
                    let (vt, _) = self.terminal(&r.pane_id)?;
                    let mut vt = vt.lock().unwrap();
                    Response::ScrollbackSearch(m::ScrollbackSearchOk {
                        matches: search(&mut vt, &re, r.from_line, r.dir, r.limit),
                    })
                }
                Q::ScrollbackMarks(r) => {
                    let (vt, _) = self.terminal(&r.pane_id)?;
                    let prompts = vt
                        .lock()
                        .unwrap()
                        .prompt_lines()
                        .map_err(|e| (m::ErrorCode::Internal, e.to_string()))?;
                    Response::ScrollbackMarks(m::ScrollbackMarksOk { prompts })
                }
                Q::ScrollbackClear(r) => {
                    self.terminal(&r.pane_id)?
                        .0
                        .lock()
                        .unwrap()
                        .clear_scrollback();
                    Response::ScrollbackClear(m::Empty {})
                }
                _ => unreachable!(),
            })
        };
        match &req {
            Q::PaneInput(_)
            | Q::ScrollbackRead(_)
            | Q::ScrollbackSearch(_)
            | Q::ScrollbackMarks(_)
            | Q::ScrollbackClear(_) => Ok(run()),
            _ => Err(req),
        }
    }

    fn dispatch(self: &Arc<Self>, st: &mut State, conn: &ConnCtx, req: Request) -> R<Response> {
        use Request as Q;
        Ok(match req {
            Q::Hello(_) => return Err(invalid("already greeted")),
            Q::SessionList(_) => {
                let sessions = st
                    .sessions
                    .values()
                    .map(|s| m::SessionSummary {
                        name: s.name.clone(),
                        root: s.root.to_string_lossy().into_owned(),
                        tabs: s.tabs.len() as u32,
                        clients: st
                            .views
                            .values()
                            .filter(|v| v.session == s.name)
                            .map(|v| v.conn)
                            .collect::<std::collections::BTreeSet<_>>()
                            .len() as u32,
                        created: s.created,
                    })
                    .collect();
                Response::SessionList(m::SessionListOk { sessions })
            }
            Q::SessionCreate(r) => {
                if r.file.is_some() {
                    return Err(invalid("session files arrive in 0.3.0"));
                }
                if r.name.is_empty() {
                    return Err(invalid("session name is empty"));
                }
                if st.sessions.contains_key(&r.name) {
                    return Err(conflict(format!("session {:?} exists", r.name)));
                }
                let root = r
                    .root
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.cfg.home.clone());
                st.sessions.insert(
                    r.name.clone(),
                    Session {
                        name: r.name.clone(),
                        root,
                        created: created_now(),
                        tabs: Vec::new(),
                        marks: BTreeMap::new(),
                        host: None,
                    },
                );
                st.had_session = true;
                self.emit(
                    m::SessionCreated {
                        name: r.name.clone(),
                        ..Default::default()
                    },
                    State::session_scope(&r.name),
                );
                if let Err(e) = self.create_tab(st, &r.name, None, None, None, None) {
                    st.sessions.remove(&r.name);
                    return Err(e);
                }
                Response::SessionCreate(m::SessionCreateOk { name: r.name })
            }
            Q::SessionAttach(r) => {
                let s = st.session(&r.name)?;
                let tab = match r.tab {
                    Some(t) if s.tabs.contains(&t) => t,
                    Some(t) => {
                        return Err(not_found(format_args!("tab {t} in session {:?}", r.name)));
                    }
                    None => s.tabs.first().cloned().ok_or_else(|| not_found("tab"))?,
                };
                let caps = st.conns.get(&conn.id).and_then(|c| c.caps.clone());
                let cols = r.cols.or(caps.as_ref().map(|c| c.cols)).unwrap_or(80);
                let rows = r.rows.or(caps.as_ref().map(|c| c.rows)).unwrap_or(24);
                let view_id = new_id();
                st.views.insert(
                    view_id.clone(),
                    View {
                        id: view_id.clone(),
                        conn: conn.id,
                        session: r.name.clone(),
                        tab: tab.clone(),
                        cols,
                        rows,
                        frames: FrameState::new(),
                    },
                );
                if let Some(caps) = &caps {
                    Self::set_session_host(st, &r.name, Some(host_caps(caps)));
                }
                self.emit(
                    m::ViewAttached {
                        view_id: view_id.clone(),
                        session: r.name.clone(),
                        ..Default::default()
                    },
                    State::session_scope(&r.name),
                );
                self.schedule_resize(st, &tab);
                let snapshot = st.snapshot(&r.name, &tab)?;
                Response::SessionAttach(m::SessionAttachOk { view_id, snapshot })
            }
            Q::SessionDetach(r) => {
                st.view(&r.view_id)?;
                self.detach(st, &r.view_id);
                Response::SessionDetach(m::Empty {})
            }
            Q::SessionRename(r) => {
                st.session(&r.name)?;
                if r.new.is_empty() {
                    return Err(invalid("session name is empty"));
                }
                if st.sessions.contains_key(&r.new) {
                    return Err(conflict(format!("session {:?} exists", r.new)));
                }
                let mut s = st.sessions.remove(&r.name).unwrap();
                s.name = r.new.clone();
                st.sessions.insert(r.new.clone(), s);
                for t in st.tabs.values_mut().filter(|t| t.session == r.name) {
                    t.session = r.new.clone();
                }
                for v in st.views.values_mut().filter(|v| v.session == r.name) {
                    v.session = r.new.clone();
                }
                self.emit(
                    m::SessionRenamed {
                        name: r.name,
                        new: r.new.clone(),
                        ..Default::default()
                    },
                    State::session_scope(&r.new),
                );
                Response::SessionRename(m::Empty {})
            }
            Q::SessionKill(r) => {
                st.session(&r.name)?;
                self.close_session(st, &r.name);
                Response::SessionKill(m::Empty {})
            }
            Q::ViewResize(r) => {
                let v = st.view_mut(&r.view_id)?;
                v.cols = r.cols.max(1);
                v.rows = r.rows.max(1);
                let (tab, session) = (v.tab.clone(), v.session.clone());
                self.emit(
                    m::ViewResized {
                        view_id: r.view_id,
                        cols: r.cols,
                        rows: r.rows,
                        ..Default::default()
                    },
                    State::session_scope(&session),
                );
                self.schedule_resize(st, &tab);
                Response::ViewResize(m::Empty {})
            }
            Q::TabCreate(r) => {
                let (tab_id, pane_id) =
                    self.create_tab(st, &r.session, r.cwd.as_deref(), r.cmd, r.title, r.index)?;
                Response::TabCreate(m::TabCreateOk { tab_id, pane_id })
            }
            Q::TabSelect(r) => {
                let session = st.tab(&r.tab_id)?.session.clone();
                let v = st.view_mut(&r.view_id)?;
                if v.session != session {
                    return Err(invalid("tab is in another session"));
                }
                let old = std::mem::replace(&mut v.tab, r.tab_id.clone());
                v.frames.need_full = true;
                self.emit(
                    m::TabSelected {
                        view_id: r.view_id,
                        tab_id: r.tab_id.clone(),
                        ..Default::default()
                    },
                    State::session_scope(&session),
                );
                self.schedule_resize(st, &old);
                self.schedule_resize(st, &r.tab_id);
                Response::TabSelect(m::Empty {})
            }
            Q::TabClose(r) => {
                st.tab(&r.tab_id)?;
                self.close_tab(st, &r.tab_id);
                Response::TabClose(m::Empty {})
            }
            Q::TabMove(r) => {
                let session = st.tab(&r.tab_id)?.session.clone();
                let s = st.sessions.get_mut(&session).unwrap();
                s.tabs.retain(|t| *t != r.tab_id);
                let at = (r.index as usize).min(s.tabs.len());
                s.tabs.insert(at, r.tab_id.clone());
                self.emit(
                    m::TabMoved {
                        tab_id: r.tab_id,
                        index: at as u32,
                        ..Default::default()
                    },
                    State::session_scope(&session),
                );
                Response::TabMove(m::Empty {})
            }
            Q::TabRename(r) => {
                let tab = st.tab_mut(&r.tab_id)?;
                tab.title = Some(r.title.clone());
                let session = tab.session.clone();
                self.emit(
                    m::TabRenamed {
                        tab_id: r.tab_id,
                        title: r.title,
                        ..Default::default()
                    },
                    State::session_scope(&session),
                );
                Response::TabRename(m::Empty {})
            }
            Q::LayoutSplit(r) => {
                let tab_id = st.pane(&r.pane_id)?.tab.clone();
                let tab = st.tab(&tab_id)?;
                if !tab.tree.as_ref().is_some_and(|t| t.contains(&r.pane_id)) {
                    return Err(invalid("only tiled panes can be split"));
                }
                let (dir, new_first) = match r.preselect {
                    Some(d) => preselect(d),
                    None => (r.dir, false),
                };
                let keep = match r.ratio {
                    Some(q) => Ratio::new(q.num, q.den)
                        .ok_or_else(|| invalid("ratio must be in [0, 1]"))?,
                    None => half(),
                };
                let cwd = PathBuf::from(&st.pane(&r.pane_id)?.cwd);
                let size = st.pane(&r.pane_id)?.size;
                let new = self.spawn_pane(st, &tab_id, &cwd, r.cmd, size)?;
                let tab = st.tabs.get_mut(&tab_id).unwrap();
                tab.tree
                    .as_mut()
                    .unwrap()
                    .split(&r.pane_id, new.clone(), dir, keep, new_first);
                tab.focus = new.clone();
                tab.zoomed = false;
                let ev = tab.layout_event();
                let session = tab.session.clone();
                self.emit(
                    m::PaneSpawned {
                        pane_id: new.clone(),
                        tab_id: tab_id.clone(),
                        ..Default::default()
                    },
                    st.pane_scope(&new),
                );
                self.emit(ev, State::session_scope(&session));
                self.emit(
                    m::LayoutFocused {
                        tab_id: tab_id.clone(),
                        pane_id: new.clone(),
                        ..Default::default()
                    },
                    State::session_scope(&session),
                );
                self.apply_geometry(st, &tab_id);
                Response::LayoutSplit(m::LayoutSplitOk { pane_id: new })
            }
            Q::LayoutClose(r) => {
                st.pane(&r.pane_id)?;
                self.remove_pane(st, &r.pane_id);
                Response::LayoutClose(m::Empty {})
            }
            Q::LayoutFocus(r) => {
                let tab_id = st.view(&r.view_id)?.tab.clone();
                let tab = st.tab(&tab_id)?;
                let target = match (r.pane_id, r.dir) {
                    (Some(p), None) => {
                        if !tab.panes().contains(&p) {
                            return Err(not_found(format_args!("pane {p} in the view's tab")));
                        }
                        p
                    }
                    (None, Some(d)) => neighbor(&tab.all_rects(), &tab.focus, d)
                        .unwrap_or_else(|| tab.focus.clone()),
                    _ => return Err(invalid("give exactly one of pane_id or dir")),
                };
                let tab = st.tabs.get_mut(&tab_id).unwrap();
                tab.focus = target.clone();
                let session = tab.session.clone();
                self.emit(
                    m::LayoutFocused {
                        tab_id: tab_id.clone(),
                        pane_id: target.clone(),
                        ..Default::default()
                    },
                    State::session_scope(&session),
                );
                if tab.zoomed {
                    self.apply_geometry(st, &tab_id);
                }
                Response::LayoutFocus(m::LayoutFocusOk { pane_id: target })
            }
            Q::LayoutResize(r) => {
                let tab_id = st.pane(&r.pane_id)?.tab.clone();
                let tab = st.tabs.get_mut(&tab_id).unwrap();
                let area = tab.area();
                let moved = tab
                    .tree
                    .as_mut()
                    .is_some_and(|t| t.resize(&r.pane_id, r.dir, r.delta_cells, area));
                if !moved {
                    return Err(invalid("no separator to move in that direction"));
                }
                self.layout_changed(st, &tab_id);
                Response::LayoutResize(m::Empty {})
            }
            Q::LayoutSwap(r) => {
                let (ta, tb) = (st.pane(&r.a)?.tab.clone(), st.pane(&r.b)?.tab.clone());
                if ta != tb {
                    return Err(invalid("panes are in different tabs"));
                }
                let tab = st.tabs.get_mut(&ta).unwrap();
                let tiled = |t: &Tab, p: &str| t.tree.as_ref().is_some_and(|n| n.contains(p));
                if !tiled(tab, &r.a) || !tiled(tab, &r.b) {
                    return Err(invalid("only tiled panes can be swapped"));
                }
                tab.tree.as_mut().unwrap().swap(&r.a, &r.b);
                self.layout_changed(st, &ta);
                Response::LayoutSwap(m::Empty {})
            }
            Q::LayoutEqualize(r) => {
                let tab = st.tab_mut(&r.tab_id)?;
                if let Some(t) = tab.tree.as_mut() {
                    t.equalize();
                }
                self.layout_changed(st, &r.tab_id);
                Response::LayoutEqualize(m::Empty {})
            }
            Q::LayoutRotate(r) => {
                let tab_id = st.pane(&r.pane_id)?.tab.clone();
                let tab = st.tabs.get_mut(&tab_id).unwrap();
                if !tab.tree.as_mut().is_some_and(|t| t.rotate(&r.pane_id)) {
                    return Err(invalid("pane is not in a split"));
                }
                self.layout_changed(st, &tab_id);
                Response::LayoutRotate(m::Empty {})
            }
            Q::LayoutZoom(r) => {
                let tab = st.tab_mut(&r.tab_id)?;
                tab.zoomed = r.on.unwrap_or(!tab.zoomed);
                let zoomed = tab.zoomed;
                self.layout_changed(st, &r.tab_id);
                Response::LayoutZoom(m::LayoutZoomOk { zoomed })
            }
            Q::LayoutFloat(r) => {
                let tab_id = st.pane(&r.pane_id)?.tab.clone();
                let tab = st.tabs.get_mut(&tab_id).unwrap();
                let tree = tab
                    .tree
                    .take()
                    .ok_or_else(|| invalid("tab has no tiled panes"))?;
                if !tree.contains(&r.pane_id) || tree.panes().len() == 1 {
                    tab.tree = Some(tree);
                    return Err(invalid("pane is not tiled, or is the last tiled pane"));
                }
                tab.tree = tree.remove(&r.pane_id);
                let (w, h) = tab.size;
                let rect = r.rect.as_ref().map(Rect::from).unwrap_or(Rect {
                    x: w / 6,
                    y: h / 6,
                    w: (w * 2 / 3).max(1),
                    h: (h * 2 / 3).max(1),
                });
                tab.floats.push((r.pane_id.clone(), rect));
                self.layout_changed(st, &tab_id);
                Response::LayoutFloat(m::Empty {})
            }
            Q::LayoutTile(r) => {
                let tab_id = st.pane(&r.pane_id)?.tab.clone();
                let tab = st.tabs.get_mut(&tab_id).unwrap();
                let Some(i) = tab.floats.iter().position(|(p, _)| *p == r.pane_id) else {
                    return Err(invalid("pane is not floating"));
                };
                tab.floats.remove(i);
                let anchor = tab.tree.as_ref().map(|t| {
                    if t.contains(&tab.focus) {
                        tab.focus.clone()
                    } else {
                        t.panes()[0].clone()
                    }
                });
                match (tab.tree.as_mut(), anchor) {
                    (Some(t), Some(a)) => {
                        t.split(&a, r.pane_id.clone(), SplitDir::H, half(), false);
                    }
                    _ => tab.tree = Some(Node::Pane(r.pane_id.clone())),
                }
                self.layout_changed(st, &tab_id);
                Response::LayoutTile(m::Empty {})
            }
            Q::LayoutSetScheme(r) => {
                st.tab_mut(&r.tab_id)?.scheme = r.scheme;
                self.layout_changed(st, &r.tab_id);
                Response::LayoutSetScheme(m::Empty {})
            }
            Q::LayoutGet(r) => {
                let tab = st.tab(&r.tab_id)?;
                Response::LayoutGet(m::LayoutGetOk {
                    tree: tab.tree.as_ref().map(Node::to_proto).unwrap_or_default(),
                    floats: tab.floats_proto(),
                })
            }
            Q::LayoutApply(r) => {
                let tab = st.tab(&r.tab_id)?;
                let tree = Node::from_proto(&r.tree).map_err(invalid)?;
                let floats: Vec<(PaneId, Rect)> = r
                    .floats
                    .unwrap_or_default()
                    .iter()
                    .map(|f| (f.id.clone(), Rect::from(&f.rect)))
                    .collect();
                let mut want: Vec<PaneId> = tree.panes();
                want.extend(floats.iter().map(|(p, _)| p.clone()));
                let mut have = tab.panes();
                want.sort();
                have.sort();
                if want != have {
                    return Err(invalid("the layout must contain exactly the tab's panes"));
                }
                let tab = st.tabs.get_mut(&r.tab_id).unwrap();
                tab.tree = Some(tree);
                tab.floats = floats;
                self.layout_changed(st, &r.tab_id);
                Response::LayoutApply(m::Empty {})
            }
            Q::PaneResize(r) => {
                let tab_id = st.pane(&r.pane_id)?.tab.clone();
                let tab = st.tabs.get_mut(&tab_id).unwrap();
                let Some((_, rect)) = tab.floats.iter_mut().find(|(p, _)| *p == r.pane_id) else {
                    return Err(invalid(
                        "only floating panes are resized directly; tiled panes follow the layout",
                    ));
                };
                rect.w = r.cols.max(1);
                rect.h = r.rows.max(1);
                self.layout_changed(st, &tab_id);
                Response::PaneResize(m::Empty {})
            }
            Q::PaneSignal(r) => {
                let pane = st.pane(&r.pane_id)?;
                let sig = match r.sig {
                    m::Signal::Sigint => Signal::SIGINT,
                    m::Signal::Sigterm => Signal::SIGTERM,
                    m::Signal::Sigkill => Signal::SIGKILL,
                    m::Signal::Sighup => Signal::SIGHUP,
                    m::Signal::Sigwinch => Signal::SIGWINCH,
                };
                // Signals go to the foreground process group, like a key typed at the terminal.
                let group = pty::foreground(pane.master.as_ref()).unwrap_or(pane.pid);
                pty::signal_group(group, sig)
                    .map_err(|e| (m::ErrorCode::Internal, e.to_string()))?;
                Response::PaneSignal(m::Empty {})
            }
            Q::PaneInfo(r) => Response::PaneInfo(st.pane(&r.pane_id)?.info()),
            Q::PaneSetMeta(r) => {
                let scope = st.pane_scope(&r.pane_id);
                let pane = st
                    .panes
                    .get_mut(&r.pane_id)
                    .ok_or_else(|| not_found("pane"))?;
                let set = r.set.unwrap_or_default();
                let unset = r.unset.unwrap_or_default();
                for k in &unset {
                    pane.meta.remove(k);
                }
                pane.meta.extend(set.clone());
                self.emit(
                    m::PaneMetaChanged {
                        pane_id: r.pane_id,
                        set,
                        unset,
                        ..Default::default()
                    },
                    scope,
                );
                Response::PaneSetMeta(m::Empty {})
            }
            Q::PaneMark(r) => {
                let mark = r
                    .mark
                    .chars()
                    .next()
                    .filter(|c| c.is_ascii_lowercase() && r.mark.len() == 1)
                    .ok_or_else(|| invalid("mark must be a-z"))?;
                let session = st
                    .session_of_pane(&r.pane_id)
                    .ok_or_else(|| not_found("pane"))?;
                st.sessions
                    .get_mut(&session)
                    .unwrap()
                    .marks
                    .insert(mark, r.pane_id.clone());
                let scope = st.pane_scope(&r.pane_id);
                self.emit(
                    m::PaneMarked {
                        pane_id: r.pane_id,
                        mark: r.mark,
                        ..Default::default()
                    },
                    scope,
                );
                Response::PaneMark(m::Empty {})
            }
            Q::PaneRespawn(r) => {
                let pane = st
                    .panes
                    .get_mut(&r.pane_id)
                    .ok_or_else(|| not_found("pane"))?;
                Self::kill_pane(pane);
                if r.cmd.is_some() {
                    pane.cmd = r.cmd;
                }
                let tab_id = pane.tab.clone();
                self.start_process(st, &r.pane_id)?;
                self.emit(
                    m::PaneSpawned {
                        pane_id: r.pane_id.clone(),
                        tab_id,
                        ..Default::default()
                    },
                    st.pane_scope(&r.pane_id),
                );
                Response::PaneRespawn(m::Empty {})
            }
            Q::FrameSubscribe(r) => {
                let v = st.view_mut(&r.view_id)?;
                if v.conn != conn.id {
                    return Err((
                        m::ErrorCode::Denied,
                        "view belongs to another connection".into(),
                    ));
                }
                v.frames.subscribed = true;
                v.frames.need_full = true;
                v.frames.unacked = None;
                v.frames.wake.notify_one();
                Response::FrameSubscribe(m::Empty {})
            }
            Q::FrameAck(r) => {
                let v = st.view_mut(&r.view_id)?;
                if v.frames.unacked == Some(r.frame_seq) {
                    v.frames.unacked = None;
                    v.frames.wake.notify_one();
                }
                Response::FrameAck(m::Empty {})
            }
            Q::PaneInput(_)
            | Q::ScrollbackRead(_)
            | Q::ScrollbackSearch(_)
            | Q::ScrollbackMarks(_)
            | Q::ScrollbackClear(_) => {
                unreachable!("handled by handle_terminal")
            }
            Q::EventsSubscribe(_) | Q::EventsUnsubscribe(_) => {
                return Err(invalid("handled by the connection"));
            }
            Q::ServerInfo(_) => Response::ServerInfo(m::ServerInfoOk {
                version: env!("CARGO_PKG_VERSION").into(),
                uptime: self.started.elapsed().as_millis() as u64,
                sessions: st.sessions.len() as u32,
                panes: st.panes.len() as u32,
                clients: st.conns.len() as u32,
            }),
            Q::ServerStop(_) => {
                // Without restore (0.3.0) panes cannot outlive the daemon, so kill_panes is moot.
                self.shutdown.send_replace(true);
                Response::ServerStop(m::Empty {})
            }
            Q::ClipboardSet(_) | Q::StoreSnapshot(_) => {
                return Err((
                    m::ErrorCode::FeatureMissing,
                    "not available in 0.1.0".into(),
                ));
            }
        })
    }

    fn layout_changed(self: &Arc<Self>, st: &mut State, tab_id: &str) {
        let Some(tab) = st.tabs.get(tab_id) else {
            return;
        };
        let session = tab.session.clone();
        self.emit(tab.layout_event(), State::session_scope(&session));
        self.apply_geometry(st, tab_id);
    }

    /// How many times `pane`'s PTY has been resized (lets tests observe REQ-VIEW-004).
    pub fn pane_resizes(&self, pane: &str) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .panes
            .get(pane)
            .map(|p| p.resizes)
    }

    /// SIGHUPs every pane (REQ-LIFE-001 without restore).
    pub fn kill_all(&self) {
        let mut st = self.state.lock().unwrap();
        for pane in st.panes.values_mut() {
            Self::kill_pane(pane);
        }
    }
}

fn placeholder_fd() -> R<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>> {
    // Replaced by start_process; a pipe end keeps the type simple until then.
    let (r, _w) = nix::unistd::pipe().map_err(|e| (m::ErrorCode::Internal, e.to_string()))?;
    nix::fcntl::fcntl(
        &r,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .map_err(|e| (m::ErrorCode::Internal, e.to_string()))?;
    tokio::io::unix::AsyncFd::new(r).map_err(|e| (m::ErrorCode::Internal, e.to_string()))
}

fn search(
    vt: &mut Terminal,
    re: &regex::Regex,
    from: i64,
    dir: m::SearchDir,
    limit: u32,
) -> Vec<m::SearchMatch> {
    let (first, last) = (vt.first_line(), vt.size().1 as i64 - 1);
    let lines: Box<dyn Iterator<Item = i64>> = match dir {
        m::SearchDir::Up => Box::new((first..=from.min(last)).rev()),
        m::SearchDir::Down => Box::new(from.max(first)..=last),
    };
    let mut out = Vec::new();
    for line in lines {
        let Ok(rows) = vt.read_lines(line, 1) else {
            break;
        };
        let Some(row) = rows.first() else { continue };
        // Map byte offsets of the line's text back to columns.
        let mut text = String::new();
        let mut col_at = Vec::new();
        for (x, c) in row.iter().enumerate() {
            if c.width == 0 {
                continue;
            }
            let s = if c.text.is_empty() {
                " "
            } else {
                c.text.as_str()
            };
            for _ in 0..s.len() {
                col_at.push(x as u16);
            }
            text.push_str(s);
        }
        col_at.push(row.len() as u16);
        for mat in re.find_iter(&text) {
            let (c0, c1) = (col_at[mat.start()], col_at[mat.end()]);
            out.push(m::SearchMatch {
                line,
                col: c0,
                len: c1 - c0,
            });
            if out.len() >= limit as usize {
                return out;
            }
        }
    }
    out
}

async fn reader(
    d: Arc<Daemon>,
    id: String,
    master: Arc<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>>,
    vt: Arc<Mutex<Terminal>>,
    input: mpsc::UnboundedSender<Vec<u8>>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let Ok(mut guard) = master.readable().await else {
            return;
        };
        let n = match guard
            .try_io(|fd| nix::unistd::read(fd.get_ref(), &mut buf).map_err(std::io::Error::from))
        {
            Ok(Ok(0)) | Ok(Err(_)) => return,
            Ok(Ok(n)) => n,
            Err(_would_block) => continue,
        };
        let (replies, changes) = {
            let mut vt = vt.lock().unwrap();
            vt.write(&buf[..n]);
            let _ = vt.sync();
            (vt.take_pty_output(), vt.take_changes())
        };
        if !replies.is_empty() {
            let _ = input.send(replies);
        }
        if changes != femux_vt::Changes::default() {
            d.pane_changes(&id, changes);
        }
        d.dirty();
        // A flooding pane must not monopolize its terminal's lock or a worker thread.
        tokio::task::yield_now().await;
    }
}

async fn writer(
    master: Arc<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>>,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    while let Some(bytes) = rx.recv().await {
        let mut off = 0;
        while off < bytes.len() {
            let Ok(mut guard) = master.writable().await else {
                return;
            };
            match guard.try_io(|fd| {
                nix::unistd::write(fd.get_ref(), &bytes[off..]).map_err(std::io::Error::from)
            }) {
                Ok(Ok(n)) => off += n,
                Ok(Err(_)) => return,
                Err(_would_block) => continue,
            }
        }
    }
}

/// Polls the foreground process and cwd (REQ-PTY-002).
async fn poller(
    d: Arc<Daemon>,
    id: String,
    master: Arc<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tick.tick().await;
        let Some(fg) = pty::foreground(master.as_ref()) else {
            continue;
        };
        let cmd = pty::proc_name(fg).unwrap_or_default();
        let cwd = pty::proc_cwd(fg);
        let mut st = d.state.lock().unwrap();
        let scope = st.pane_scope(&id);
        let Some(pane) = st.panes.get_mut(&id) else {
            return;
        };
        let mut changed = false;
        if pane.fg_pid != fg || pane.fg_cmd != cmd {
            pane.fg_pid = fg;
            pane.fg_cmd = cmd.clone();
            d.emit(
                m::PaneFgChanged {
                    pane_id: id.clone(),
                    pid: fg,
                    cmd,
                    ..Default::default()
                },
                scope.clone(),
            );
            changed = true;
        }
        if let Some(cwd) = cwd.filter(|_| pane.osc_cwd.is_none())
            && pane.cwd != cwd
        {
            pane.cwd = cwd.clone();
            d.emit(
                m::PaneCwdChanged {
                    pane_id: id.clone(),
                    cwd,
                    ..Default::default()
                },
                scope,
            );
            changed = true;
        }
        drop(st);
        if changed {
            d.dirty();
        }
    }
}

impl Daemon {
    /// Title, cwd (OSC 7) and bell reports from a pane's terminal (REQ-VT-006).
    fn pane_changes(&self, id: &str, c: femux_vt::Changes) {
        let mut st = self.state.lock().unwrap();
        let scope = st.pane_scope(id);
        let Some(pane) = st.panes.get_mut(id) else {
            return;
        };
        if let Some(title) = c.title {
            pane.title = title.clone();
            self.emit(
                m::PaneTitleChanged {
                    pane_id: id.to_owned(),
                    title,
                    ..Default::default()
                },
                scope.clone(),
            );
        }
        if let Some(url) = c.pwd {
            let cwd = osc7_path(&url);
            pane.osc_cwd = Some(cwd.clone());
            if pane.cwd != cwd {
                pane.cwd = cwd.clone();
                self.emit(
                    m::PaneCwdChanged {
                        pane_id: id.to_owned(),
                        cwd,
                        ..Default::default()
                    },
                    scope.clone(),
                );
            }
        }
        for _ in 0..c.bells {
            self.emit(
                m::PaneBell {
                    pane_id: id.to_owned(),
                    ..Default::default()
                },
                scope.clone(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_urls_become_paths() {
        assert_eq!(osc7_path("file://host/tmp/x"), "/tmp/x");
        assert_eq!(osc7_path("file:///tmp"), "/tmp");
        assert_eq!(osc7_path("/plain"), "/plain");
    }
}
