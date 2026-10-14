//! The femux TUI client (SPEC §5): modal input, layout actions, tab strip, copy mode, the
//! command palette, which-key, mouse. Terminal I/O lives in [`term`] and this module's
//! [`run`]; everything else is pure and tested headlessly.

pub mod app;
pub mod bindings;
pub mod config;
pub mod copy;
pub mod graphics;
pub mod host;
pub mod inbox;
pub mod keys;
pub mod layout;
pub mod model;
pub mod nav;
pub mod probe;
pub mod remote;
pub mod render;
pub mod shm;
pub mod term;
pub mod theme;

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
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
pub use remote::{Remote, Target};
use theme::Theme;

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// A protocol connection run on its own thread and runtime.
pub struct Conn {
    tx: mpsc::UnboundedSender<(u64, Request)>,
    rx: std_mpsc::Receiver<ServerMsg>,
    next_id: u64,
    /// Set when the daemon's side of the link closes (the reader ends).
    closed: Arc<AtomicBool>,
    /// What `ssh` printed on stderr, for error messages.
    stderr: Arc<Mutex<String>>,
    /// `ssh -T … attach --stdio`, killed with the connection.
    child: Option<tokio::process::Child>,
    /// `ssh -N -L …` forwarding the socket, killed with the connection.
    forward: Option<std::process::Child>,
    wake: Wake,
    _rt: tokio::runtime::Runtime,
}

/// A non-blocking pipe: the reader task writes a byte per message, the run loop polls the
/// other end next to the terminal and drains it.
struct Wake {
    read: std::os::fd::OwnedFd,
    write: Arc<std::os::fd::OwnedFd>,
}

#[derive(Clone)]
struct Ring(Arc<std::os::fd::OwnedFd>);

impl Ring {
    fn ring(&self) {
        use std::os::fd::AsRawFd;
        // SAFETY: a one-byte write to an owned non-blocking pipe; a full pipe already wakes.
        unsafe { libc::write(self.0.as_raw_fd(), [1u8].as_ptr() as *const _, 1) };
    }
}

impl Wake {
    fn new() -> Wake {
        use std::os::fd::FromRawFd;
        let mut fds = [0; 2];
        // SAFETY: pipe fills two fds we then own; both are made non-blocking.
        unsafe {
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                panic!("pipe: {}", io::Error::last_os_error());
            }
            for fd in fds {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            Wake {
                read: std::os::fd::OwnedFd::from_raw_fd(fds[0]),
                write: Arc::new(std::os::fd::OwnedFd::from_raw_fd(fds[1])),
            }
        }
    }

    fn writer(&self) -> Ring {
        Ring(self.write.clone())
    }

    fn read_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.read.as_raw_fd()
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.start_kill();
        }
        if let Some(c) = &mut self.forward {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
}

/// Collects a child's stderr (the last 4 KiB) on a thread.
fn collect_stderr(err: impl io::Read + Send + 'static) -> Arc<Mutex<String>> {
    let buf = Arc::new(Mutex::new(String::new()));
    let b = buf.clone();
    std::thread::spawn(move || {
        let mut r = io::BufReader::new(err);
        let mut line = String::new();
        while io::BufRead::read_line(&mut r, &mut line).is_ok_and(|n| n > 0) {
            let mut all = b.lock().unwrap();
            all.push_str(&line);
            let cut = all.len().saturating_sub(4096);
            if cut > 0 {
                *all = all[cut..].to_owned();
            }
            line.clear();
        }
    });
    buf
}

impl Conn {
    pub fn open(socket: &Path) -> io::Result<Conn> {
        let rt = runtime()?;
        let stream = rt.block_on(tokio::net::UnixStream::connect(socket))?;
        let (rd, wr) = stream.into_split();
        Ok(Conn::start(rt, rd, wr))
    }

    /// Connects to `target`; `batch` forbids ssh prompts (reconnects).
    pub fn connect(target: &Target, batch: bool) -> io::Result<Conn> {
        let r = match target {
            Target::Local(p) => return Conn::open(p),
            Target::Remote(r) => r,
        };
        if r.forward {
            return Conn::forwarded(r, batch);
        }
        let rt = runtime()?;
        let mut cmd = tokio::process::Command::from(r.stdio_command(batch));
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = {
            let _in_rt = rt.enter();
            cmd.spawn()
                .map_err(|e| other(format!("{}: {e}", remote::ssh_program())))?
        };
        let rd = child.stdout.take().unwrap();
        let wr = child.stdin.take().unwrap();
        let err = child.stderr.take().unwrap().into_owned_fd()?;
        let stderr = collect_stderr(std::fs::File::from(err));
        let mut conn = Conn::start(rt, rd, wr);
        conn.stderr = stderr;
        conn.child = Some(child);
        Ok(conn)
    }

    /// `forward = true`: asks the remote for its socket, forwards it, connects locally.
    fn forwarded(r: &remote::Remote, batch: bool) -> io::Result<Conn> {
        let out = r
            .ensure_command(batch)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| other(format!("{}: {e}", remote::ssh_program())))?;
        if !out.status.success() {
            return Err(other(format!(
                "{}: {}",
                r.label,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let remote_sock = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let local = std::env::temp_dir().join(format!(
            "femux-fwd-{}-{}.sock",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&local);
        let mut child = r
            .forward_command(&local, &remote_sock, batch)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| other(format!("{}: {e}", remote::ssh_program())))?;
        let stderr = collect_stderr(child.stderr.take().unwrap());
        let deadline = Instant::now() + Duration::from_secs(if batch { 15 } else { 120 });
        loop {
            if let Ok(mut c) = Conn::open(&local) {
                c.forward = Some(child);
                c.stderr = stderr;
                return Ok(c);
            }
            if child.try_wait()?.is_some() || Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(other(format!(
                    "{}: forwarding failed: {}",
                    r.label,
                    stderr.lock().unwrap().trim()
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn start(
        rt: tokio::runtime::Runtime,
        mut rd: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        mut wr: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    ) -> Conn {
        let (tx, mut out) = mpsc::unbounded_channel::<(u64, Request)>();
        let (msgs, rx) = std_mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        // A byte down this pipe wakes the run loop as soon as a message arrives, instead of
        // after its input timeout: video frames need prompt image reads.
        let wake = Wake::new();
        let ring = wake.writer();
        rt.spawn(async move {
            while let Some((id, req)) = out.recv().await {
                let Ok(bytes) = req.encode(id) else { continue };
                if wr.write_all(&bytes).await.is_err() || wr.flush().await.is_err() {
                    return;
                }
            }
        });
        let c = closed.clone();
        rt.spawn(async move {
            while let Ok(Some(frame)) = read_frame(&mut rd).await {
                let Ok(msg) = decode_envelope(&frame).and_then(ServerMsg::decode) else {
                    continue;
                };
                if msgs.send(msg).is_err() {
                    break;
                }
                ring.ring();
            }
            c.store(true, Ordering::SeqCst);
            ring.ring();
        });
        Conn {
            tx,
            rx,
            // The app numbers its own requests from 1; the handshake uses the high range.
            next_id: 1 << 40,
            closed,
            stderr: Arc::new(Mutex::new(String::new())),
            child: None,
            forward: None,
            wake,
            _rt: rt,
        }
    }

    /// Readable when messages are waiting (see [`Wake`]).
    pub fn wake_fd(&self) -> std::os::fd::RawFd {
        self.wake.read_fd()
    }

    /// The link closed: nothing more will arrive (after what is queued).
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// What ssh reported, if anything.
    pub fn stderr_text(&self) -> String {
        self.stderr.lock().unwrap().trim().to_owned()
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

    /// Waits up to `timeout` for the response to `id`, setting aside anything else; gives up
    /// early when the link closes.
    fn wait_for(
        &mut self,
        id: u64,
        timeout: Duration,
        aside: &mut Vec<ServerMsg>,
    ) -> io::Result<Response> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ServerMsg::Response(i, r)) if i == id => return Ok(r),
                Ok(ServerMsg::Error(i, e)) if i == id => {
                    return Err(other(format!("{:?}: {}", e.code, e.message)));
                }
                Ok(msg) => aside.push(msg),
                Err(_) if self.closed() || Instant::now() > deadline => {
                    let why = self.stderr_text();
                    return Err(other(if why.is_empty() {
                        "daemon did not answer".to_owned()
                    } else {
                        why
                    }));
                }
                Err(_) => {}
            }
        }
    }

    fn call(
        &mut self,
        req: impl Into<Request>,
        aside: &mut Vec<ServerMsg>,
    ) -> io::Result<Response> {
        let id = self.send(req);
        self.wait_for(id, Duration::from_secs(10), aside)
    }
}

/// The message families the client uses (REQ-PROTO-020): the daemon refuses requests of
/// families a connection did not ask for.
pub const FEATURES: [&str; 12] = [
    "session",
    "tab",
    "layout",
    "pane",
    "frame",
    "events",
    "view",
    "scrollback",
    "clipboard",
    "notify",
    "plugin",
    "image",
];

/// What the `femux` binary hands the client.
pub struct Host {
    pub cfg: ClientConfig,
    pub plugins: Vec<Box<dyn ClientPlugin>>,
    /// CLI subcommand names for the palette (REQ-UI-043).
    pub cli_commands: Vec<String>,
    /// Re-reads the client's config after `config.reloaded` (ADR-013).
    pub reload: Option<Box<dyn Fn() -> Result<ClientConfig, String> + Send>>,
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

/// DECSCUSR for `shape`; `Default` asks for the terminal's own (0).
pub fn decscusr(shape: termwiz::surface::CursorShape) -> String {
    use termwiz::surface::CursorShape as S;
    let n = match shape {
        S::Default => 0,
        S::BlinkingBlock => 1,
        S::SteadyBlock => 2,
        S::BlinkingUnderline => 3,
        S::SteadyUnderline => 4,
        S::BlinkingBar => 5,
        S::SteadyBar => 6,
    };
    format!("\x1b[{n} q")
}

/// The bytes that turn the terminal showing `prev` into `next`: only the changed cells
/// (REQ-UI-050), wrapped in synchronized output (mode 2026) when the host supports it.
pub fn paint(
    renderer: &mut TerminfoRenderer,
    prev: &Surface,
    next: &Surface,
    sync: bool,
) -> io::Result<Vec<u8>> {
    let mut changes = vec![Change::CursorVisibility(CursorVisibility::Hidden)];
    changes.extend(prev.diff_screens(next));
    let (cx, cy) = next.cursor_position();
    changes.push(Change::CursorPosition {
        x: Position::Absolute(cx),
        y: Position::Absolute(cy),
    });
    changes.push(Change::CursorVisibility(next.cursor_visibility()));
    let mut out = Out(Vec::new(), next.dimensions());
    if sync {
        out.0.extend_from_slice(b"\x1b[?2026h");
    }
    renderer.render_to(&changes, &mut out).map_err(other)?;
    // DECSCUSR when the shape changes (REQ-VT-002), written directly: many terminfo entries
    // (xterm-256color among them) lack `Ss`, so termwiz would write nothing.
    let shape = next.cursor_shape().unwrap_or_default();
    if shape != prev.cursor_shape().unwrap_or_default() {
        out.0.extend_from_slice(decscusr(shape).as_bytes());
    }
    if sync {
        out.0.extend_from_slice(b"\x1b[?2026l");
    }
    Ok(out.0)
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

/// Says hello on a fresh connection.
fn hello(conn: &mut Conn, hello: &m::Hello, timeout: Duration) -> io::Result<Vec<ServerMsg>> {
    let mut aside = Vec::new();
    let id = conn.send(hello.clone());
    conn.wait_for(id, timeout, &mut aside)?;
    Ok(aside)
}

/// Picks the session (`session`, else the first, else a new `0`), subscribes to its events
/// and attaches a view of `cols`×`rows`.
fn attach_session(
    conn: &mut Conn,
    session: Option<&str>,
    cols: u16,
    rows: u16,
    aside: &mut Vec<ServerMsg>,
) -> io::Result<m::SessionAttachOk> {
    let listed = match conn.call(m::SessionList {}, aside)? {
        Response::SessionList(l) => l.sessions,
        _ => Vec::new(),
    };
    let name = match session {
        Some(s) if session_known(&listed, s) || listed.is_empty() => s.to_owned(),
        // On reconnect the session may be gone: take another rather than fail.
        Some(_) | None if !listed.is_empty() => listed[0].name.clone(),
        _ => "main".to_owned(),
    };
    if !session_known(&listed, &name) {
        conn.call(
            m::SessionCreate {
                name: name.clone(),
                root: None,
                file: None,
            },
            aside,
        )?;
    }
    conn.call(
        m::EventsSubscribe {
            session: Some(name.clone()),
            ..Default::default()
        },
        aside,
    )?;
    match conn.call(
        m::SessionAttach {
            name,
            tab: None,
            cols: Some(cols),
            rows: Some(rows),
        },
        aside,
    )? {
        Response::SessionAttach(a) => Ok(a),
        r => Err(other(format!("unexpected {r:?}"))),
    }
}

fn session_known(list: &[m::SessionSummary], name: &str) -> bool {
    list.iter().any(|s| s.name == name)
}

/// What the reconnect thread reports.
enum Relink {
    Failed {
        attempt: u32,
        error: String,
        next: Duration,
    },
    Up(Box<Conn>, Vec<ServerMsg>),
}

/// Reconnects in the background with backoff (1 s doubling to 30 s) until it succeeds or the
/// client stops listening. No prompts: the client owns the terminal.
fn relink(target: Target, hello_msg: m::Hello) -> std_mpsc::Receiver<Relink> {
    let (tx, rx) = std_mpsc::channel();
    std::thread::spawn(move || {
        let mut wait = Duration::from_secs(1);
        for attempt in 1.. {
            std::thread::sleep(wait);
            let up = Conn::connect(&target, true).and_then(|mut c| {
                let aside = hello(&mut c, &hello_msg, Duration::from_secs(20))?;
                Ok((c, aside))
            });
            let msg = match up {
                Ok((c, aside)) => Relink::Up(Box::new(c), aside),
                Err(e) => {
                    wait = (wait * 2).min(Duration::from_secs(30));
                    Relink::Failed {
                        attempt,
                        error: e.to_string(),
                        next: wait,
                    }
                }
            };
            let done = matches!(msg, Relink::Up(..));
            if tx.send(msg).is_err() || done {
                return;
            }
        }
    });
    rx
}

/// Attaches to `session` (or the first session, creating `0` if there is none) on `target`
/// and runs the client until detach or until the session goes away. A remote link that drops
/// is reconnected; the remote daemon keeps the session meanwhile.
pub fn run(target: &Target, session: Option<&str>, host: Host) -> io::Result<()> {
    let caps = Capabilities::new_from_env().map_err(other)?;
    // Before raw mode: ssh may need to ask about host keys or passwords.
    if let Some(r) = target.remote() {
        r.open_master()?;
    }
    let mut term = term::Term::open()?;
    term.raw()?;
    let probe = probe::run(&mut term, Duration::from_millis(250));
    let mut early = term.parse(&probe.leftover, true);
    let (cols, rows) = term.size();
    // The pane area before the app exists (no side nav yet).
    let area = layout::Layout::compute(cols, rows, &host.cfg, false).panes;

    let hello_msg = m::Hello {
        proto: femux_proto::PROTO_VERSION,
        client: format!("femux-tui/{}", env!("CARGO_PKG_VERSION")),
        features: FEATURES.map(String::from).to_vec(),
        term: Some(m::TermCaps {
            cols: area.w,
            rows: area.h,
            colors: m::ColorDepth::Truecolor,
            kitty_graphics: probe.kitty_graphics,
            kitty_keyboard: probe.kitty_keyboard,
            sixel: probe.sixel,
            sync_output: probe.sync_output,
            fg: probe.fg,
            bg: probe.bg,
            cursor: probe.cursor,
            name: std::env::var("TERM_PROGRAM").ok(),
            cell_w: probe.cell.map(|c| c.0),
            cell_h: probe.cell.map(|c| c.1),
        }),
    };
    // The first connection may prompt (passwords, host keys): give ssh time.
    let first_wait = if target.remote().is_some() {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(10)
    };
    let mut conn = Conn::connect(target, false)?;
    let mut aside = hello(&mut conn, &hello_msg, first_wait)?;
    let attach = attach_session(&mut conn, session, area.w, area.h, &mut aside)?;
    let host_colors = theme::HostColors {
        dark_mode: probe.dark,
        fg: probe.fg,
        bg: probe.bg,
    };
    let (mut theme, theme_error) = Theme::resolve(&host.cfg.theme, &host.plugins, host_colors);
    let reload = host.reload;
    let mut app = App::new(
        host.cfg,
        host.plugins,
        host.cli_commands,
        &attach,
        cols,
        rows,
    );
    if let Some(e) = theme_error {
        app.message = Some(e);
    }
    app.remote = target.remote().map(|r| r.label.clone());
    app.graphics.enabled = probe.kitty_graphics && app.cfg.kitty_graphics;
    app.graphics.sync = probe.sync_output;
    // ADR-021 #4: shared memory when the daemon is on this machine and the host reads it,
    // compression over SSH.
    app.graphics.transfer = match target {
        Target::Local(_) if probe.kitty_shm => graphics::Transfer::Shm,
        Target::Remote(_) => graphics::Transfer::Zlib,
        Target::Local(_) => graphics::Transfer::Plain,
    };
    let mut relinking: Option<std_mpsc::Receiver<Relink>> = None;
    let mouse = app.cfg.mouse;
    term.enter(mouse)?;

    let mut renderer = TerminfoRenderer::new(caps);
    let mut prev = Surface::new(cols as usize, rows as usize);
    let mut size = (cols, rows);
    let mut buttons = MouseButtons::NONE;
    let sync = probe.sync_output;
    let result = (|| -> io::Result<()> {
        loop {
            if relinking.is_none() && conn.closed() && conn.rx.try_recv().is_err() {
                let Some(r) = target.remote() else {
                    return Err(other("the daemon went away"));
                };
                app.link_lost(&r.label, None);
                relinking = Some(relink(target.clone(), hello_msg.clone()));
            }
            if let Some(rx) = &relinking {
                match rx.try_recv() {
                    Ok(Relink::Failed {
                        attempt,
                        error,
                        next,
                    }) => {
                        let label = &target.remote().unwrap().label;
                        app.link_lost(label, Some((attempt, error, next)));
                    }
                    Ok(Relink::Up(mut c, mut a)) => {
                        let (pw, ph) = app.view_size();
                        let session = app.model.session.clone();
                        match attach_session(&mut c, Some(&session), pw, ph, &mut a) {
                            Ok(at) => {
                                conn = *c;
                                aside = a;
                                app.reattached(&at);
                                relinking = None;
                                prev = Surface::new(size.0 as usize, size.1 as usize);
                                term.write(b"\x1b[2J")?;
                                app.graphics.invalidate();
                            }
                            Err(e) => {
                                let label = target.remote().unwrap().label.clone();
                                app.link_lost(
                                    &label,
                                    Some((0, e.to_string(), Duration::from_secs(1))),
                                );
                                relinking = Some(relink(target.clone(), hello_msg.clone()));
                            }
                        }
                    }
                    Err(std_mpsc::TryRecvError::Empty) => {}
                    Err(std_mpsc::TryRecvError::Disconnected) => relinking = None,
                }
            }
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
            // Image reads leave before the repaint (REQ-GFX-001): video frames wait on them.
            app.sync_graphics();
            for (id, req) in app.take_out() {
                // No link: detaching means leaving; anything else waits for the reattach.
                if relinking.is_some() {
                    if matches!(req, Request::SessionDetach(_)) {
                        return Ok(());
                    }
                    continue;
                }
                conn.send_raw(id, req);
            }
            for fx in app.take_effects() {
                match fx {
                    Effect::Exit => return Ok(()),
                    Effect::Clipboard(text) => term.write(term::osc52(&text).as_bytes())?,
                    Effect::Passthrough(bytes) => term.write(&bytes)?,
                    Effect::Bell => term.write(host::BELL.as_bytes())?,
                    Effect::Sound(file) => {
                        let path = std::env::var("PATH").unwrap_or_default();
                        let file = std::path::PathBuf::from(file);
                        if let Some(cmd) = host::sound_command(&file, &path) {
                            host::spawn_quietly(cmd);
                        } else {
                            // No player installed: the bell is better than silence.
                            term.write(host::BELL.as_bytes())?;
                        }
                    }
                    Effect::Desktop { title, body } => {
                        let how = host::desktop_for(
                            std::env::var("TERM_PROGRAM").ok().as_deref(),
                            std::env::var("TERM").ok().as_deref(),
                        );
                        match host::desktop_sequence(how, &title, &body) {
                            Some(seq) => term.write(seq.as_bytes())?,
                            None => {
                                let path = std::env::var("PATH").unwrap_or_default();
                                if let Some(cmd) = host::native_command(&title, &body, &path) {
                                    host::spawn_quietly(cmd);
                                }
                            }
                        }
                    }
                    Effect::Reload => match reload.as_ref().map(|r| r()) {
                        Some(Ok(cfg)) => {
                            let (t, err) = Theme::resolve(&cfg.theme, &app.plugins, host_colors);
                            theme = t;
                            app.apply_config(cfg);
                            if let Some(e) = err {
                                app.message = Some(e);
                            }
                            prev = Surface::new(size.0 as usize, size.1 as usize);
                            term.write(b"\x1b[2J")?;
                            app.graphics.invalidate();
                        }
                        Some(Err(e)) => app.message = Some(e),
                        None => {}
                    },
                }
            }
            let now_size = term.size();
            if now_size != size {
                size = now_size;
                app.on_resize(size.0, size.1);
                prev = Surface::new(size.0 as usize, size.1 as usize);
                term.write(b"\x1b[2J")?;
                app.graphics.invalidate();
            }
            if app.dirty {
                let mut next = Surface::new(size.0 as usize, size.1 as usize);
                render::render(&mut app, &theme, &mut next);
                let bytes = paint(&mut renderer, &prev, &next, sync)?;
                term.write(&bytes)?;
                prev = next;
            }
            // Images go over the painted text (REQ-GFX-001); the reads they need leave now,
            // not after the next wait.
            app.sync_graphics();
            let gfx = app.graphics.take_output();
            if !gfx.is_empty() {
                term.write(&gfx)?;
            }
            if relinking.is_none() {
                for (id, req) in app.take_out() {
                    conn.send_raw(id, req);
                }
            }
            let events = if early.is_empty() {
                term.read_or_wake(Duration::from_millis(10), conn.wake_fd())?
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
    if app.graphics.enabled {
        let _ = term.write(graphics::KITTY_CLEAR.as_bytes());
    }
    term.leave();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use termwiz::caps::ProbeHints;

    /// REQ-VT-002: the host cursor's shape is written when it changes, and only then.
    #[test]
    fn paint_sets_the_cursor_shape_when_it_changes() {
        use termwiz::surface::CursorShape;
        let caps =
            Capabilities::new_with_hints(ProbeHints::default().term(Some("xterm-256color".into())))
                .unwrap();
        let mut r = TerminfoRenderer::new(caps);
        let a = Surface::new(20, 3);
        let mut b = Surface::new(20, 3);
        b.add_change(Change::CursorShape(CursorShape::BlinkingBar));
        let s = String::from_utf8_lossy(&paint(&mut r, &a, &b, false).unwrap()).into_owned();
        assert!(s.contains("\x1b[5 q"), "{s:?}");
        let s = String::from_utf8_lossy(&paint(&mut r, &b, &b, false).unwrap()).into_owned();
        assert!(!s.contains(" q"), "unchanged: {s:?}");
        let s = String::from_utf8_lossy(&paint(&mut r, &b, &a, false).unwrap()).into_owned();
        assert!(s.contains("\x1b[0 q"), "back to the default: {s:?}");
    }

    #[test]
    fn paint_writes_only_the_diff_inside_synchronized_output() {
        let caps =
            Capabilities::new_with_hints(ProbeHints::default().term(Some("xterm-256color".into())))
                .unwrap();
        let mut r = TerminfoRenderer::new(caps);
        let mut a = Surface::new(20, 3);
        a.add_change("hello world");
        let mut b = Surface::new(20, 3);
        b.add_change("hello there");
        let bytes = paint(&mut r, &a, &b, true).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            s.starts_with("\x1b[?2026h") && s.ends_with("\x1b[?2026l"),
            "{s:?}"
        );
        assert!(s.contains("there") && !s.contains("hello"), "{s:?}");
        let plain = paint(&mut r, &a, &b, false).unwrap();
        assert!(!String::from_utf8_lossy(&plain).contains("2026"));
    }
}
