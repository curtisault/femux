//! femux clients. 0.1.0 ships the bare viewer (ADR-014): attach to one session, show its
//! focused pane, type into it, `prefix d` (ctrl+space d) to detach.

pub mod viewer;

use std::io;
use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use femux_proto::msg as m;
use femux_proto::{Push, Request, Response, ServerMsg, decode_envelope, io_async::read_frame};
use termwiz::caps::Capabilities;
use termwiz::surface::Change;
use termwiz::terminal::buffered::BufferedTerminal;
use termwiz::terminal::{Terminal, new_terminal};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use viewer::{Action, Viewer};

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// A protocol connection run on its own thread and runtime.
struct Conn {
    tx: mpsc::UnboundedSender<(u64, Request)>,
    rx: std_mpsc::Receiver<ServerMsg>,
    next_id: u64,
    _rt: tokio::runtime::Runtime,
}

impl Conn {
    fn open(socket: &Path) -> io::Result<Conn> {
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
            next_id: 1,
            _rt: rt,
        })
    }

    fn send(&mut self, req: impl Into<Request>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let _ = self.tx.send((id, req.into()));
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

/// Attaches to `session` (or the first session, creating `0` if there is none) and runs the
/// bare viewer until detach or until the session goes away.
pub fn run(socket: &Path, session: Option<&str>) -> io::Result<()> {
    let caps = Capabilities::new_from_env().map_err(other)?;
    let mut term = new_terminal(caps).map_err(other)?;
    let size = term.get_screen_size().map_err(other)?;

    let mut conn = Conn::open(socket)?;
    let mut aside = Vec::new();
    let hello = m::Hello {
        proto: femux_proto::PROTO_VERSION,
        client: format!("femux-viewer/{}", env!("CARGO_PKG_VERSION")),
        features: [
            "session", "tab", "layout", "pane", "frame", "events", "view",
        ]
        .map(String::from)
        .to_vec(),
        term: Some(m::TermCaps {
            cols: size.cols as u16,
            rows: size.rows as u16,
            colors: m::ColorDepth::Truecolor,
            kitty_graphics: false,
            kitty_keyboard: false,
            sixel: false,
            sync_output: true,
            fg: None,
            bg: None,
            cursor: None,
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
            cols: Some(size.cols as u16),
            rows: Some(size.rows as u16),
        },
        &mut aside,
    )? {
        Response::SessionAttach(a) => a,
        r => return Err(other(format!("unexpected {r:?}"))),
    };
    let mut viewer = Viewer::new(&attach);
    conn.send(m::FrameSubscribe {
        view_id: attach.view_id.clone(),
    });

    term.set_raw_mode().map_err(other)?;
    term.enter_alternate_screen().map_err(other)?;
    let mut screen = BufferedTerminal::new(term).map_err(other)?;
    let result = (|| -> io::Result<()> {
        loop {
            for msg in aside
                .drain(..)
                .chain(conn.rx.try_iter().collect::<Vec<_>>())
            {
                match msg {
                    ServerMsg::Push(Push::Frame(f)) => {
                        let ack = viewer.on_frame(&f);
                        conn.send(ack);
                    }
                    ServerMsg::Event(femux_proto::Event::SessionClosed(c)) if c.name == name => {
                        return Ok(());
                    }
                    ServerMsg::Event(femux_proto::Event::ViewDetached(d))
                        if d.view_id == viewer.view_id =>
                    {
                        return Ok(());
                    }
                    ServerMsg::Event(e) => viewer.on_event(&e),
                    _ => {}
                }
            }
            if viewer.dirty {
                viewer.render(&mut screen);
                screen.flush().map_err(other)?;
            }
            if let Some(ev) = screen
                .terminal()
                .poll_input(Some(Duration::from_millis(10)))
                .map_err(other)?
            {
                if let termwiz::input::InputEvent::Resized { cols, rows } = ev {
                    screen.resize(cols, rows);
                    viewer.dirty = true;
                }
                for action in viewer.on_input(&ev) {
                    match action {
                        Action::Send(req) => {
                            conn.send(req);
                        }
                        Action::Detach => {
                            let id = conn.send(m::SessionDetach {
                                view_id: viewer.view_id.clone(),
                            });
                            let _ = conn.wait(id, &mut Vec::new());
                            return Ok(());
                        }
                    }
                }
            }
        }
    })();
    screen.add_change(Change::ClearScreen(Default::default()));
    let _ = screen.flush();
    let term = screen.terminal();
    let _ = term.exit_alternate_screen();
    let _ = term.set_cooked_mode();
    result
}
