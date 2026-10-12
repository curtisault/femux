//! The `femux` binary (REQ-ARCH-004): `femux daemon` runs the server; plain `femux` attaches,
//! starting a daemon if none is running.

use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use femux_proto::msg as m;
use femux_proto::{Call, Request, ServerMsg, decode_envelope, read_frame};

#[derive(Parser)]
#[command(name = "femux", version, about = "A terminal multiplexer")]
struct Cli {
    /// Server name (`[daemon] name`).
    #[arg(long, global = true, default_value = "default")]
    server: String,
    /// Socket path; overrides --server and $FEMUX_SOCKET.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the session daemon.
    Daemon {
        /// Stay in the foreground instead of detaching.
        #[arg(long)]
        foreground: bool,
    },
    /// Attach the bare viewer to a session.
    Attach { session: Option<String> },
    /// List sessions.
    Ls,
    /// Create a session.
    New { name: String },
    /// Kill a session.
    Kill { name: String },
}

fn socket(cli: &Cli) -> PathBuf {
    if let Some(s) = &cli.socket {
        return s.clone();
    }
    if cli.server == "default"
        && let Some(s) = std::env::var_os("FEMUX_SOCKET")
    {
        return s.into();
    }
    femux_proto::socket_path(&cli.server)
}

fn running(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// Starts `femux daemon --foreground` in its own session and waits for its socket.
fn spawn_daemon(cli: &Cli, socket: &Path) -> io::Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args([
        "daemon",
        "--foreground",
        "--server",
        &cli.server,
        "--socket",
    ])
    .arg(socket)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !running(socket) {
        if Instant::now() > deadline {
            return Err(io::Error::other("the daemon did not start"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn run_daemon(cli: &Cli, socket: PathBuf) -> io::Result<()> {
    let mut cfg = femux_daemon::Config::new(&cli.server);
    cfg.socket = socket;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(femux_daemon::run_with_signals(cfg))
}

/// One request over a fresh connection.
fn call<C: Call>(socket: &Path, req: C) -> io::Result<C::Response> {
    let mut s = UnixStream::connect(socket)
        .map_err(|e| io::Error::new(e.kind(), format!("no daemon at {}: {e}", socket.display())))?;
    let hello = m::Hello {
        proto: femux_proto::PROTO_VERSION,
        client: format!("femux-cli/{}", env!("CARGO_PKG_VERSION")),
        features: vec![femux_proto::feature_of(C::KIND).to_owned()],
        term: None,
    };
    use std::io::Write;
    s.write_all(&Request::from(hello).encode(1)?)?;
    s.write_all(&req.into().encode(2)?)?;
    loop {
        let frame = read_frame(&mut s)?.ok_or_else(|| io::Error::other("connection closed"))?;
        let env = decode_envelope(&frame).map_err(io::Error::other)?;
        match (env.id, env.kind.as_str()) {
            (2, "error") | (1, "error") => {
                let e: m::ErrorBody = femux_proto::from_body(env.body).map_err(io::Error::other)?;
                return Err(io::Error::other(format!(
                    "{}: {}",
                    e.code.as_str(),
                    e.message
                )));
            }
            (2, _) => return femux_proto::from_body(env.body).map_err(io::Error::other),
            _ => {
                let _ = ServerMsg::decode(env);
            }
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let socket = socket(&cli);
    let result = match &cli.command {
        Some(Cmd::Daemon { foreground: true }) => run_daemon(&cli, socket),
        Some(Cmd::Daemon { foreground: false }) => {
            if running(&socket) {
                Err(io::Error::other(format!(
                    "a daemon is already running at {}",
                    socket.display()
                )))
            } else {
                spawn_daemon(&cli, &socket)
            }
        }
        None => (|| {
            if !running(&socket) {
                spawn_daemon(&cli, &socket)?;
            }
            femux_client::run(&socket, None)
        })(),
        Some(Cmd::Attach { session }) => femux_client::run(&socket, session.as_deref()),
        Some(Cmd::Ls) => call(&socket, m::SessionList {}).map(|l| {
            for s in l.sessions {
                println!("{}: {} tabs, {} clients", s.name, s.tabs, s.clients);
            }
        }),
        Some(Cmd::New { name }) => (|| {
            if !running(&socket) {
                spawn_daemon(&cli, &socket)?;
            }
            call(
                &socket,
                m::SessionCreate {
                    name: name.clone(),
                    root: None,
                    file: None,
                },
            )
            .map(|_| ())
        })(),
        Some(Cmd::Kill { name }) => {
            call(&socket, m::SessionKill { name: name.clone() }).map(|_| ())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("femux: {e}");
            ExitCode::FAILURE
        }
    }
}
