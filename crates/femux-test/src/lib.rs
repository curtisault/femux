//! Headless test harness (REQ-TEST-001): starts a daemon on a temp socket, connects as a
//! client, applies frames to a screen model, and exposes `screen(pane)` as a JSON cell grid
//! plus `screen_text(pane)` for debugging.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use femux_proto::msg::{self as m};
use femux_proto::{
    Call, ErrorBody, Event, Push, Request, Response, ServerMsg, decode_envelope,
    io_async::read_frame,
};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

pub use femux_daemon::Config;

/// A daemon on a temp socket with a predictable shell (`/bin/sh`, `PS1='$ '`) and a store in
/// the temp directory.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub socket: PathBuf,
    pub handle: Option<femux_daemon::Handle>,
    /// The config the daemon runs with; [`Harness::restart`] reuses it.
    pub cfg: Config,
}

impl Harness {
    pub fn config(dir: &Path) -> Config {
        let mut cfg = Config::new("test");
        cfg.socket = dir.join("run/test.sock");
        cfg.shell = "/bin/sh".into();
        cfg.login_shell = false;
        cfg.home = dir.to_path_buf();
        cfg.env = vec![
            ("PS1".into(), "$ ".into()),
            ("ENV".into(), String::new()),
            ("HOME".into(), dir.to_string_lossy().into_owned()),
        ];
        cfg.db = Some(dir.join("state/test.db"));
        cfg
    }

    pub fn start() -> Harness {
        Harness::start_with(|_| {})
    }

    pub fn start_with(f: impl FnOnce(&mut Config)) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Harness::config(dir.path());
        f(&mut cfg);
        let socket = cfg.socket.clone();
        let handle = femux_daemon::start(cfg.clone()).unwrap();
        Harness {
            dir,
            socket,
            handle: Some(handle),
            cfg,
        }
    }

    /// Stops the daemon (as `server.stop` would: exit snapshot, panes SIGHUPed) and starts a
    /// new one on the same socket and store, after `f` adjusts its config.
    pub fn restart_with(&mut self, f: impl FnOnce(&mut Config)) {
        self.stop();
        f(&mut self.cfg);
        self.handle = Some(femux_daemon::start(self.cfg.clone()).unwrap());
    }

    /// Like [`Harness::restart_with`] with the same config.
    pub fn restart(&mut self) {
        self.restart_with(|_| {});
    }

    pub fn daemon(&self) -> &std::sync::Arc<femux_daemon::Daemon> {
        &self.handle.as_ref().unwrap().daemon
    }

    /// A client that has completed the handshake with every feature and an 80x24 terminal.
    pub async fn client(&self) -> Client {
        Client::connect(&self.socket, Client::hello(80, 24))
            .await
            .unwrap()
    }

    pub fn stop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.stop();
        }
    }
}

/// A pane as seen through frames: cells indexed by column (a wide character's second
/// column holds a copy with `width` 0 removed from text output).
#[derive(Debug, Clone, Default)]
pub struct PaneScreen {
    pub rect: m::Rect,
    pub rows: Vec<Vec<Option<m::Cell>>>,
    pub cursor: m::Cursor,
}

impl PaneScreen {
    fn apply(&mut self, pf: &m::PaneFrame) {
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

    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|r| {
                let mut s = String::new();
                let mut x = 0;
                while x < r.len() {
                    match &r[x] {
                        Some(c) if !c.ch.is_empty() => {
                            s.push_str(&c.ch);
                            x += c.width.max(1) as usize;
                        }
                        _ => {
                            s.push(' ');
                            x += 1;
                        }
                    }
                }
                s.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The JSON cell grid (ADR-012).
    pub fn json(&self) -> Value {
        let blank = || json!({"ch": "", "fg": "default", "bg": "default", "attrs": 0, "width": 1});
        json!({
            "cols": self.rect.w,
            "rows": self.rect.h,
            "cursor": serde_json::to_value(&self.cursor).unwrap(),
            "cells": self.rows.iter().map(|r| r.iter().map(|c| c.as_ref().map_or_else(blank, |c| serde_json::to_value(c).unwrap())).collect::<Vec<_>>()).collect::<Vec<_>>(),
        })
    }
}

/// A protocol client that tracks events and the frames of its views.
pub struct Client {
    wr: OwnedWriteHalf,
    rx: mpsc::UnboundedReceiver<Result<ServerMsg, String>>,
    next_id: u64,
    pending: HashMap<u64, Result<Response, ErrorBody>>,
    pub events: Vec<Event>,
    pub frames: Vec<m::Frame>,
    pub panes: HashMap<String, PaneScreen>,
    pub tab: Option<m::TabInfo>,
    /// Ack frames as they arrive (default true).
    pub auto_ack: bool,
    pub hello_ok: Option<m::HelloOk>,
}

impl Client {
    pub fn hello(cols: u16, rows: u16) -> m::Hello {
        m::Hello {
            proto: femux_proto::PROTO_VERSION,
            client: "femux-test/0.1.0".into(),
            features: femux_daemon::SERVED.iter().map(|s| s.to_string()).collect(),
            term: Some(m::TermCaps {
                cols,
                rows,
                colors: m::ColorDepth::Truecolor,
                kitty_graphics: false,
                kitty_keyboard: false,
                sixel: false,
                sync_output: true,
                fg: None,
                bg: None,
                cursor: None,
                name: None,
                cell_w: None,
                cell_h: None,
            }),
        }
    }

    /// Connects without a handshake.
    pub async fn raw(socket: &Path) -> std::io::Result<Client> {
        let stream = UnixStream::connect(socket).await?;
        let (mut rd, wr) = stream.into_split();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = read_frame(&mut rd).await {
                let msg = decode_envelope(&frame)
                    .and_then(ServerMsg::decode)
                    .map_err(|e| e.to_string());
                if tx.send(msg).is_err() {
                    return;
                }
            }
        });
        Ok(Client {
            wr,
            rx,
            next_id: 1,
            pending: HashMap::new(),
            events: Vec::new(),
            frames: Vec::new(),
            panes: HashMap::new(),
            tab: None,
            auto_ack: true,
            hello_ok: None,
        })
    }

    pub async fn connect(socket: &Path, hello: m::Hello) -> Result<Client, ErrorBody> {
        let mut c = Client::raw(socket).await.map_err(|e| ErrorBody {
            code: m::ErrorCode::Internal,
            message: e.to_string(),
            data: None,
        })?;
        let ok = c.call(hello).await?;
        c.hello_ok = Some(ok);
        Ok(c)
    }

    /// Sends a request under a caller-chosen id (for driving a client state machine).
    pub async fn send_with_id(&mut self, id: u64, req: Request) {
        let bytes = req.encode(id).unwrap();
        self.wr.write_all(&bytes).await.unwrap();
    }

    /// The next raw message within `timeout`, bypassing this client's own bookkeeping.
    pub async fn raw_recv(&mut self, timeout: Duration) -> Option<ServerMsg> {
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(Some(Ok(m))) => Some(m),
            _ => None,
        }
    }

    /// Sends a request without waiting; returns its id.
    pub async fn send(&mut self, req: impl Into<Request>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let bytes = req.into().encode(id).unwrap();
        self.wr.write_all(&bytes).await.unwrap();
        id
    }

    /// Sends a request and waits for its response.
    pub async fn call<C: Call>(&mut self, req: C) -> Result<C::Response, ErrorBody>
    where
        C::Response: serde::Serialize,
    {
        let id = self.send(req).await;
        let resp = self.response(id).await?;
        // Re-decode through the wire format into the concrete response type.
        let bytes = resp.encode(id).unwrap();
        let env = decode_envelope(&bytes[4..]).unwrap();
        Ok(femux_proto::from_body(env.body).unwrap())
    }

    pub async fn response(&mut self, id: u64) -> Result<Response, ErrorBody> {
        loop {
            if let Some(r) = self.pending.remove(&id) {
                return r;
            }
            if !self.recv(Duration::from_secs(10)).await {
                panic!("no response to request {id}");
            }
        }
    }

    /// Handles one incoming message; false on timeout or disconnect.
    pub async fn recv(&mut self, timeout: Duration) -> bool {
        let msg = match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(Some(m)) => m,
            _ => return false,
        };
        match msg.expect("undecodable message from daemon") {
            ServerMsg::Response(id, r) => {
                self.pending.insert(id, Ok(r));
            }
            ServerMsg::Error(id, e) => {
                self.pending.insert(id, Err(e));
            }
            ServerMsg::Event(e) => self.events.push(e),
            ServerMsg::Push(Push::Frame(f)) => {
                if f.full {
                    self.panes
                        .retain(|p, _| f.panes.iter().any(|pf| pf.pane_id == *p));
                }
                for pf in &f.panes {
                    self.panes.entry(pf.pane_id.clone()).or_default().apply(pf);
                }
                self.tab = Some(f.tab.clone());
                let ack = m::FrameAck {
                    view_id: f.view_id.clone(),
                    frame_seq: f.frame_seq,
                };
                self.frames.push(f);
                if self.auto_ack {
                    self.send(ack).await;
                }
            }
        }
        true
    }

    /// True if the daemon closed the connection (within `timeout`).
    pub async fn closed(&mut self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(None) => return true,
                Ok(Some(_)) => continue,
                Err(_) => return false,
            }
        }
    }

    /// Processes messages until `cond` holds; panics with `what` after `timeout`.
    pub async fn wait_for(
        &mut self,
        what: &str,
        timeout: Duration,
        mut cond: impl FnMut(&Client) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + timeout;
        while !cond(self) {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() || !self.recv(left).await {
                if cond(self) {
                    return;
                }
                panic!(
                    "timed out waiting for {what}; screens: {:#?}",
                    self.panes
                        .iter()
                        .map(|(k, v)| (k, v.text()))
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    /// Waits until `pane`'s screen contains `needle`.
    pub async fn wait_text(&mut self, pane: &str, needle: &str) {
        let p = pane.to_owned();
        self.wait_for(
            &format!("{needle:?} in pane {pane}"),
            Duration::from_secs(10),
            |c| c.panes.get(&p).is_some_and(|s| s.text().contains(needle)),
        )
        .await;
    }

    /// Waits for an event of `kind`; returns it.
    pub async fn wait_event(&mut self, kind: &str) -> Event {
        let start = self.events.len();
        self.wait_event_from(kind, start).await
    }

    /// Waits for an event of `kind` at or after `self.events[start]`, so events that arrived
    /// while a request was in flight still count; returns the first.
    pub async fn wait_event_from(&mut self, kind: &str, start: usize) -> Event {
        let k = kind.to_owned();
        self.wait_for(&format!("event {kind}"), Duration::from_secs(10), |c| {
            c.events[start..].iter().any(|e| e.kind() == k)
        })
        .await;
        self.events[start..]
            .iter()
            .find(|e| e.kind() == kind)
            .unwrap()
            .clone()
    }

    /// The pane's screen as a JSON cell grid.
    pub fn screen(&self, pane: &str) -> Value {
        self.panes.get(pane).map_or(Value::Null, PaneScreen::json)
    }

    /// The pane's screen as text, trailing blanks trimmed.
    pub fn screen_text(&self, pane: &str) -> String {
        self.panes
            .get(pane)
            .map(PaneScreen::text)
            .unwrap_or_default()
    }

    /// Attaches to `session` with a view of `cols`×`rows` and subscribes to its frames.
    pub async fn attach(&mut self, session: &str, cols: u16, rows: u16) -> m::SessionAttachOk {
        let ok = self
            .call(m::SessionAttach {
                name: session.into(),
                tab: None,
                cols: Some(cols),
                rows: Some(rows),
            })
            .await
            .unwrap();
        self.call(m::FrameSubscribe {
            view_id: ok.view_id.clone(),
        })
        .await
        .unwrap();
        ok
    }

    /// Types `text` into `pane` as a paste-free sequence of key events.
    pub async fn type_text(&mut self, pane: &str, text: &str) {
        let events = text
            .chars()
            .map(|ch| {
                let key = match ch {
                    '\n' | '\r' => "enter".to_owned(),
                    c => c.to_string(),
                };
                m::InputEvent::KeyEvent(m::KeyEvent {
                    key,
                    mods: vec![],
                    text: (!matches!(ch, '\n' | '\r')).then(|| ch.to_string()),
                    event: m::KeyEventKind::Press,
                })
            })
            .collect();
        self.call(m::PaneInput {
            pane_id: pane.into(),
            events,
        })
        .await
        .unwrap();
    }

    /// Creates a session and returns its first tab and pane.
    pub async fn new_session(&mut self, name: &str) -> (String, String) {
        self.call(m::SessionCreate {
            name: name.into(),
            root: None,
            file: None,
        })
        .await
        .unwrap();
        let list = self
            .call(m::SessionAttach {
                name: name.into(),
                tab: None,
                cols: None,
                rows: None,
            })
            .await
            .unwrap();
        self.call(m::SessionDetach {
            view_id: list.view_id,
        })
        .await
        .unwrap();
        let tab = &list.snapshot.tabs[0];
        (tab.id.clone(), tab.focus.clone())
    }
}
