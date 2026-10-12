//! The femux TUI client (SPEC §5): modal input, layout actions, tab strip, copy mode, the
//! command palette, which-key, mouse. Terminal I/O lives in [`term`] and this module's
//! [`run`]; everything else is pure and tested headlessly.

pub mod app;
pub mod bindings;
pub mod config;
pub mod copy;
pub mod graphics;
pub mod keys;
pub mod model;
pub mod probe;
pub mod render;
pub mod term;
pub mod theme;

use std::io;
use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use femux_plugin::ClientPlugin;
use femux_proto::msg as m;
use femux_proto::{Request, Response, ServerMsg, decode_envelope, io_async::read_frame};
use termwiz::caps::Capabilities;
use termwiz::input::{InputEvent, MouseButtons};
use termwiz::render::terminfo::TerminfoRenderer;
use termwiz::surface::{Change, CursorVisibility, Position, Surface};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use app::{App, Effect, MouseKind};
pub use config::ClientConfig;
use theme::Theme;

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// A protocol connection run on its own thread and runtime.
pub struct Conn {
    tx: mpsc::UnboundedSender<(u64, Request)>,
    rx: std_mpsc::Receiver<ServerMsg>,
    next_id: u64,
    _rt: tokio::runtime::Runtime,
}

impl Conn {
    pub fn open(socket: &Path) -> io::Result<Conn> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let stream = rt.block_on(tokio::net::UnixStream::connect(socket))?;
        let (mut rd, mut wr) = stream.into_split();
        let (tx, mut out) = mpsc::unbounded_channel::<(u64, Request)>();
        let (msgs, rx) = std_mpsc::channel();
        rt.spawn(async move {
            while let Some((id, req)) = out.recv().await {
                let Ok(bytes) = req.encode(id) else { continue };
                if wr.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        });
        rt.spawn(async move {
            while let Ok(Some(frame)) = read_frame(&mut rd).await {
                let Ok(msg) = decode_envelope(&frame).and_then(ServerMsg::decode) else {
                    continue;
                };
                if msgs.send(msg).is_err() {
                    return;
                }
            }
        });
        Ok(Conn {
            tx,
            rx,
            // The app numbers its own requests from 1; the handshake uses the high range.
            next_id: 1 << 40,
            _rt: rt,
        })
    }

    pub fn send_raw(&self, id: u64, req: Request) {
        let _ = self.tx.send((id, req));
    }

    fn send(&mut self, req: impl Into<Request>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(id, req.into());
        id
    }

    /// Waits for the response to `id`, setting aside anything else.
    fn wait(&mut self, id: u64, aside: &mut Vec<ServerMsg>) -> io::Result<Response> {
        loop {
            match self
                .rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|_| other("daemon did not answer"))?
            {
                ServerMsg::Response(i, r) if i == id => return Ok(r),
                ServerMsg::Error(i, e) if i == id => {
                    return Err(other(format!("{:?}: {}", e.code, e.message)));
                }
                msg => aside.push(msg),
            }
        }
    }

    fn call(
        &mut self,
        req: impl Into<Request>,
        aside: &mut Vec<ServerMsg>,
    ) -> io::Result<Response> {
        let id = self.send(req);
        self.wait(id, aside)
    }
}

/// What the `femux` binary hands the client.
pub struct Host {
    pub cfg: ClientConfig,
    pub plugins: Vec<Box<dyn ClientPlugin>>,
    /// CLI subcommand names for the palette (REQ-UI-043).
    pub cli_commands: Vec<String>,
}

/// Chrome rows for a config (tab strip + status line), used before the app exists.
fn chrome_rows(cfg: &ClientConfig) -> u16 {
    1 + u16::from(cfg.tabs != config::TabsPosition::Hidden)
}

/// Collects output for the terminfo renderer.
struct Out(Vec<u8>, (usize, usize));

impl io::Write for Out {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl termwiz::render::RenderTty for Out {
    fn get_size_in_cells(&mut self) -> termwiz::Result<(usize, usize)> {
        Ok(self.1)
    }
}

fn mouse_kind(prev: MouseButtons, now: MouseButtons) -> MouseKind {
    if now.contains(MouseButtons::VERT_WHEEL) {
        return if now.contains(MouseButtons::WHEEL_POSITIVE) {
            MouseKind::WheelUp
        } else {
            MouseKind::WheelDown
        };
    }
    let pressed = |b: MouseButtons| now.contains(b.clone()) && !prev.contains(b);
    if pressed(MouseButtons::LEFT) {
        return MouseKind::Press(m::MouseButton::Left);
    }
    if pressed(MouseButtons::MIDDLE) {
        return MouseKind::Press(m::MouseButton::Middle);
    }
    if pressed(MouseButtons::RIGHT) {
        return MouseKind::Press(m::MouseButton::Right);
    }
    let any = MouseButtons::LEFT | MouseButtons::MIDDLE | MouseButtons::RIGHT;
    if !(now & any.clone()).is_empty() {
        MouseKind::Move
    } else if !(prev & any).is_empty() {
        MouseKind::Release
    } else {
        MouseKind::Move
    }
}

/// Attaches to `session` (or the first session, creating `0` if there is none) and runs the
/// client until detach or until the session goes away.
pub fn run(socket: &Path, session: Option<&str>, host: Host) -> io::Result<()> {
    let caps = Capabilities::new_from_env().map_err(other)?;
    let mut term = term::Term::open()?;
    term.raw()?;
    let probe = probe::run(&mut term, Duration::from_millis(250));
    let mut early = term.parse(&probe.leftover, true);
    let (cols, rows) = term.size();
    let chrome = chrome_rows(&host.cfg);

    let mut conn = Conn::open(socket)?;
    let mut aside = Vec::new();
    let hello = m::Hello {
        proto: femux_proto::PROTO_VERSION,
        client: format!("femux-tui/{}", env!("CARGO_PKG_VERSION")),
        features: [
            "session",
            "tab",
            "layout",
            "pane",
            "frame",
            "events",
            "view",
            "scrollback",
            "clipboard",
        ]
        .map(String::from)
        .to_vec(),
        term: Some(m::TermCaps {
            cols,
            rows: rows.saturating_sub(chrome).max(1),
            colors: m::ColorDepth::Truecolor,
            kitty_graphics: false,
            kitty_keyboard: probe.kitty_keyboard,
            sixel: probe.sixel,
            sync_output: probe.sync_output,
            fg: probe.fg,
            bg: probe.bg,
            cursor: probe.cursor,
            name: std::env::var("TERM_PROGRAM").ok(),
        }),
    };
    conn.call(hello, &mut aside)?;
    let name = match session {
        Some(s) => s.to_owned(),
        None => match conn.call(m::SessionList {}, &mut aside)? {
            Response::SessionList(l) if !l.sessions.is_empty() => l.sessions[0].name.clone(),
            _ => {
                conn.call(
                    m::SessionCreate {
                        name: "0".into(),
                        root: None,
                        file: None,
                    },
                    &mut aside,
                )?;
                "0".into()
            }
        },
    };
    conn.call(
        m::EventsSubscribe {
            session: Some(name.clone()),
            ..Default::default()
        },
        &mut aside,
    )?;
    let attach = match conn.call(
        m::SessionAttach {
            name: name.clone(),
            tab: None,
            cols: Some(cols),
            rows: Some(rows.saturating_sub(chrome).max(1)),
        },
        &mut aside,
    )? {
        Response::SessionAttach(a) => a,
        r => return Err(other(format!("unexpected {r:?}"))),
    };
    let theme = Theme::pick(&host.cfg.theme, probe.dark, probe.bg);
    let mut app = App::new(
        host.cfg,
        host.plugins,
        host.cli_commands,
        &attach,
        cols,
        rows,
    );
    let mouse = app.cfg.mouse;
    term.enter(mouse)?;

    let mut renderer = TerminfoRenderer::new(caps);
    let mut prev = Surface::new(cols as usize, rows as usize);
    let mut size = (cols, rows);
    let mut buttons = MouseButtons::NONE;
    let sync = probe.sync_output;
    let result = (|| -> io::Result<()> {
        loop {
            for msg in aside
                .drain(..)
                .chain(conn.rx.try_iter().collect::<Vec<_>>())
            {
                // Handshake responses are not the app's.
                if let ServerMsg::Response(id, _) | ServerMsg::Error(id, _) = &msg
                    && *id >= 1 << 40
                {
                    continue;
                }
                app.on_msg(msg);
            }
            app.tick(Instant::now());
            for (id, req) in app.take_out() {
                conn.send_raw(id, req);
            }
            for fx in app.take_effects() {
                match fx {
                    Effect::Exit => return Ok(()),
                    Effect::Clipboard(text) => term.write(term::osc52(&text).as_bytes())?,
                }
            }
            let now_size = term.size();
            if now_size != size {
                size = now_size;
                app.on_resize(size.0, size.1);
                prev = Surface::new(size.0 as usize, size.1 as usize);
                term.write(b"\x1b[2J")?;
            }
            if app.dirty {
                let mut next = Surface::new(size.0 as usize, size.1 as usize);
                render::render(&mut app, &theme, &mut next);
                let mut changes = vec![Change::CursorVisibility(CursorVisibility::Hidden)];
                changes.extend(prev.diff_screens(&next));
                let (cx, cy) = next.cursor_position();
                changes.push(Change::CursorPosition {
                    x: Position::Absolute(cx),
                    y: Position::Absolute(cy),
                });
                changes.push(Change::CursorVisibility(next.cursor_visibility()));
                let mut out = Out(Vec::new(), (size.0 as usize, size.1 as usize));
                if sync {
                    out.0.extend_from_slice(b"\x1b[?2026h");
                }
                renderer.render_to(&changes, &mut out).map_err(other)?;
                if sync {
                    out.0.extend_from_slice(b"\x1b[?2026l");
                }
                term.write(&out.0)?;
                prev = next;
            }
            let events = if early.is_empty() {
                term.read(Duration::from_millis(10))?
            } else {
                std::mem::take(&mut early)
            };
            for ev in events {
                match ev {
                    InputEvent::Key(k) => {
                        if let Some(key) = keys::map_key(&k) {
                            app.on_key(key, Instant::now());
                        }
                    }
                    InputEvent::Paste(text) => app.on_paste(text),
                    InputEvent::Mouse(me) => {
                        let kind = mouse_kind(buttons.clone(), me.mouse_buttons.clone());
                        buttons = me.mouse_buttons
                            & (MouseButtons::LEFT | MouseButtons::MIDDLE | MouseButtons::RIGHT);
                        let mods = keys::map_key(&termwiz::input::KeyEvent {
                            key: termwiz::input::KeyCode::Char('a'),
                            modifiers: me.modifiers,
                        })
                        .map(|k| k.mods)
                        .unwrap_or_default();
                        app.on_mouse(me.x.saturating_sub(1), me.y.saturating_sub(1), kind, mods);
                    }
                    InputEvent::Resized { .. } => {}
                    _ => {}
                }
            }
        }
    })();
    term.leave();
    result
}
