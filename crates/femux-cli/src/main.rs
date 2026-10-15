//! The `femux` binary (REQ-ARCH-004, SPEC §11): `femux daemon` runs the server, plain
//! `femux` attaches (starting a daemon if none runs), and every other subcommand is a thin
//! wrapper over the protocol with `--json` output (REQ-CLI-001). Inside a pane, commands
//! default to that pane (REQ-CLI-002). Completions come from the CLI definition
//! (REQ-CLI-003). Plugins add subcommands (REQ-PLUG-002).

mod config;
mod conn;
mod debug;
mod integration;
mod layouts;
mod plugins;

use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as Proc, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clap::{Arg, ArgMatches, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use femux_plugin::{CliCore, Plugin};
use femux_proto::msg as m;
use serde_json::{Value, json};

use config::Config;
use conn::Conn;

#[derive(Parser)]
#[command(name = "femux", version, about = "A terminal multiplexer")]
struct Cli {
    /// Server name (`[daemon] name`).
    #[arg(long, global = true)]
    server: Option<String>,
    /// Socket path; overrides --server and $FEMUX_SOCKET.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Debug mode: log in detail to the server's log (`--debug=trace` for more). Also
    /// $FEMUX_DEBUG=1 or trace.
    #[arg(
        long,
        global = true,
        value_name = "LEVEL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "debug"
    )]
    debug: Option<String>,
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
        /// Start one only if none runs, and print its socket either way.
        #[arg(long, conflicts_with = "foreground")]
        ensure: bool,
    },
    /// Attach the client to a session (the first one, or a new one, when omitted).
    Attach {
        session: Option<String>,
        /// Attach to the daemon on a remote host over ssh: a name from `femux hosts`, or an
        /// ssh destination.
        #[arg(long, conflicts_with = "stdio")]
        host: Option<String>,
        /// Speak the protocol on stdin/stdout (what `--host` runs on the remote).
        #[arg(long)]
        stdio: bool,
    },
    /// List sessions.
    Ls,
    /// Create a session.
    New {
        name: String,
        /// Root directory (defaults to $HOME).
        #[arg(long)]
        root: Option<String>,
    },
    /// Kill a session.
    Kill { name: String },
    /// Tabs: new, close, rename, select.
    #[command(subcommand)]
    Tab(TabCmd),
    /// Panes: split, close, focus, resize, info, set, cat, signal, respawn.
    #[command(subcommand)]
    Pane(PaneCmd),
    /// Layout templates: save, load, list, export.
    #[command(subcommand)]
    Layout(LayoutCmd),
    /// Key bindings: list, explain.
    #[command(subcommand)]
    Keys(KeysCmd),
    /// Agent hook integration: install, uninstall.
    #[command(subcommand)]
    Integration(IntegrationCmd),
    /// The config file: check, edit.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Apply config.toml to the running daemon and its clients (an invalid file changes nothing).
    Reload,
    /// The log and debug mode: on, off, status, path, snapshot, tail.
    #[command(subcommand)]
    Debug(debug::DebugCmd),
    /// Print shell completions for bash, zsh or fish.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// Which pane a command acts on: `--pane`, else `$FEMUX_PANE` (REQ-CLI-002).
#[derive(Args, Clone)]
struct PaneSel {
    /// Pane id (defaults to $FEMUX_PANE).
    #[arg(long, short = 'p')]
    pane: Option<String>,
}

#[derive(Subcommand)]
enum TabCmd {
    /// Open a tab (in the session of the current pane by default).
    New {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        index: Option<u32>,
        /// Command to run instead of the shell.
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Close a tab (the current pane's tab by default).
    Close { tab: Option<String> },
    /// Rename a tab.
    Rename {
        title: String,
        #[arg(long)]
        tab: Option<String>,
    },
    /// Show a tab (by index from 1, or id) in every attached client of its session.
    Select {
        tab: String,
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
enum PaneCmd {
    /// Split the pane.
    Split {
        #[command(flatten)]
        sel: PaneSel,
        /// h: side by side (new pane right); v: stacked (new pane below).
        #[arg(long, default_value = "v")]
        dir: String,
        #[arg(long)]
        cwd: Option<String>,
        /// Put the new pane on this side instead: h, j, k or l.
        #[arg(long)]
        preselect: Option<String>,
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Close the pane.
    Close {
        #[command(flatten)]
        sel: PaneSel,
    },
    /// Focus a pane by id, or the neighbor in a direction (h, j, k, l).
    Focus {
        target: String,
        #[command(flatten)]
        sel: PaneSel,
    },
    /// Grow the pane in a direction (h, j, k, l) by `delta` cells.
    Resize {
        dir: String,
        #[arg(default_value = "2")]
        delta: i32,
        #[command(flatten)]
        sel: PaneSel,
    },
    /// Show pane info.
    Info {
        #[command(flatten)]
        sel: PaneSel,
    },
    /// Set pane metadata (`key=value`, repeatable) or unset keys.
    Set {
        #[arg(required_unless_present = "unset")]
        kv: Vec<String>,
        #[arg(long)]
        unset: Vec<String>,
        #[command(flatten)]
        sel: PaneSel,
    },
    /// Print the last lines of the pane (screen and scrollback).
    Cat {
        #[arg(long, default_value = "50")]
        lines: u32,
        #[command(flatten)]
        sel: PaneSel,
    },
    /// Send a signal: SIGINT, SIGTERM, SIGKILL, SIGHUP or SIGWINCH.
    Signal {
        sig: String,
        #[command(flatten)]
        sel: PaneSel,
    },
    /// Restart the pane's process.
    Respawn {
        #[command(flatten)]
        sel: PaneSel,
        #[arg(last = true)]
        cmd: Vec<String>,
    },
}

#[derive(Subcommand)]
enum LayoutCmd {
    /// Save the current tab's layout as a named template.
    Save {
        name: String,
        #[arg(long)]
        tab: Option<String>,
    },
    /// Open a new tab from a template.
    Load {
        name: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// List saved templates.
    List,
    /// Print a template (or the current tab's layout when no name is given).
    Export {
        name: Option<String>,
        #[arg(long)]
        tab: Option<String>,
    },
}

#[derive(Subcommand)]
enum KeysCmd {
    /// List every binding with its action and description.
    List,
    /// What a key does in each table.
    Explain { key: String },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Validate config.toml and every session file; errors name their line.
    Check,
    /// Open config.toml in $VISUAL or $EDITOR (seeded with the defaults if missing), then check it.
    Edit,
}

#[derive(Subcommand)]
enum IntegrationCmd {
    /// Write the agent's hook config: claude-code, codex, opencode or cursor.
    Install { agent: String },
    /// Remove what install wrote.
    Uninstall { agent: String },
    /// Called by the agent's hooks; sets agent.state on $FEMUX_PANE.
    #[command(hide = true)]
    Hook {
        agent: String,
        /// A state, or the agent's event payload.
        event: Option<String>,
    },
}

struct Out {
    text: String,
    json: Value,
    /// The command printed its own output (`debug tail`).
    printed: bool,
    /// A plugin asked to attach to this session afterwards.
    attach: Option<String>,
}

type R = Result<Out, String>;

fn ok(text: impl Into<String>, json: Value) -> R {
    Ok(Out {
        text: text.into(),
        json,
        printed: false,
        attach: None,
    })
}

fn socket_path(cli: &Cli, cfg: &Config) -> PathBuf {
    if let Some(s) = &cli.socket {
        return s.clone();
    }
    let server = cli
        .server
        .clone()
        .unwrap_or_else(|| cfg.daemon.name.clone());
    if cli.server.is_none()
        && let Some(s) = std::env::var_os("FEMUX_SOCKET")
    {
        return s.into();
    }
    femux_proto::socket_path(&server)
}

fn local(socket: &Path) -> femux_client::Target {
    femux_client::Target::Local(socket.to_path_buf())
}

/// `attach --stdio` (REQ-PROTO-003): copies bytes between stdin/stdout and the daemon's
/// socket until either side closes. The daemon sees an ordinary client; when the SSH link
/// drops, stdin closes, the connection with it, and the daemon detaches the view.
fn stdio_proxy(socket: &Path) -> io::Result<()> {
    use std::io::{Read, Write};
    let sock = UnixStream::connect(socket)?;
    let mut from_daemon = sock.try_clone()?;
    let mut to_daemon = sock;
    std::thread::spawn(move || {
        let mut out = io::stdout().lock();
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = from_daemon.read(&mut buf) {
            // stdout is line buffered: flush every chunk or frames sit in the buffer.
            if n == 0 || out.write_all(&buf[..n]).is_err() || out.flush().is_err() {
                break;
            }
        }
        std::process::exit(0);
    });
    let mut input = io::stdin().lock();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        to_daemon.write_all(&buf[..n])?;
    }
}

fn running(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// Starts `femux daemon --foreground` in its own session and waits for its socket.
fn spawn_daemon(server: &str, socket: &Path) -> io::Result<()> {
    let mut cmd = Proc::new(std::env::current_exe()?);
    cmd.args(["daemon", "--foreground", "--server", server, "--socket"])
        .arg(socket);
    // REQ-LOG-003: a daemon started by a process in debug mode starts in it.
    if let Some(l) = debug::requested_level() {
        cmd.arg(format!("--debug={l}"));
    }
    cmd.stdin(Stdio::null())
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

fn run_daemon(server: &str, socket: PathBuf, cfg: &Config, plugins: Vec<Plugin>) -> io::Result<()> {
    // REQ-LOG-005: the daemon's own stderr (detached, it went nowhere) goes into the log.
    let captured = femux_log::capture_stderr().unwrap_or(false);
    femux_log::start(json!({
        "command": "daemon",
        "socket": socket.display().to_string(),
        "plugins": cfg.plugins.enabled,
        "stderr_captured": captured,
    }));
    let mut dcfg = femux_daemon::Config::new(server);
    dcfg.socket = socket.clone();
    dcfg.keep_alive = cfg.daemon.keep_alive;
    dcfg.scrollback_lines = cfg.terminal.scrollback_lines;
    // REQ-STORE-001: the store lives in the state directory, one file per server.
    dcfg.db = Some(femux_config::db_path(server));
    dcfg.restore = cfg.restore.clone();
    dcfg.env_allow = cfg.env.allow.clone();
    dcfg.read_allow = cfg.clipboard.read_allow.clone();
    dcfg.passthrough = cfg.terminal.passthrough;
    dcfg.config_path = Some(femux_config::config_path());
    dcfg.gaps = cfg.layout.gaps;
    dcfg.scheme = femux_daemon::scheme(cfg.layout.scheme);
    dcfg.log_level = cfg.debug.level;
    dcfg.metrics_interval = Duration::from_millis(cfg.debug.metrics_interval_ms);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // Daemon plugins connect over the socket like any client (REQ-PLUG-003).
    for p in plugins.into_iter().filter_map(|p| p.daemon) {
        let socket = socket.clone();
        rt.spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !running(&socket) && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let name = p.name();
            if let Err(e) = femux_plugin::run_daemon_plugin(&socket, p).await {
                femux_log::error(&format!("plugin.{name}"), "plugin.error")
                    .msg(format!("plugin {name} stopped: {e}"))
                    .field("plugin", name)
                    .field("error", e.to_string())
                    .emit();
            }
        });
    }
    rt.block_on(femux_daemon::run_with_signals(dcfg))
}

fn connect(socket: &Path) -> Result<Conn, String> {
    Conn::open(socket).map_err(|e| e.to_string())
}

fn err<T>(e: femux_proto::ErrorBody) -> Result<T, String> {
    Err(format!("{}: {}", e.code.as_str(), e.message))
}

fn pane_of(conn: &Conn, sel: &Option<String>) -> Result<String, String> {
    sel.clone()
        .or_else(|| conn.env_pane())
        .ok_or_else(|| "no pane: run inside femux or pass --pane".to_owned())
}

/// The session a command means: `--session`, the current pane's, or the only one.
fn session_of(conn: &mut Conn, given: &Option<String>) -> Result<String, String> {
    if let Some(s) = given {
        return Ok(s.clone());
    }
    if let Some(p) = conn.env_pane() {
        return conn
            .call(m::PaneInfo { pane_id: p })
            .map(|i| i.session)
            .or_else(err);
    }
    let list = conn.call(m::SessionList {}).or_else(err)?.sessions;
    match list.as_slice() {
        [one] => Ok(one.name.clone()),
        [] => Err("no sessions".into()),
        _ => Err("several sessions: pass --session".into()),
    }
}

/// A session's tabs (read-only, ADR-016 #11).
fn tabs_of(conn: &mut Conn, session: &str) -> Result<Vec<m::TabSnapshot>, String> {
    conn.call(m::SessionTabs {
        name: session.to_owned(),
    })
    .map(|r| r.tabs)
    .or_else(err)
}

fn is_ulid(s: &str) -> bool {
    s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// A tab from an id, a 1-based index in `session`, or the current pane's tab.
fn tab_of(
    conn: &mut Conn,
    given: &Option<String>,
    session: &Option<String>,
) -> Result<String, String> {
    match given.as_deref() {
        Some(t) if is_ulid(t) => Ok(t.to_owned()),
        Some(t) => {
            let n: usize = t
                .parse()
                .map_err(|_| format!("tab {t:?} is neither an id nor an index"))?;
            let s = session_of(conn, session)?;
            let tabs = tabs_of(conn, &s)?;
            tabs.get(n.wrapping_sub(1))
                .map(|t| t.id.clone())
                .ok_or_else(|| format!("session {s:?} has no tab {n}"))
        }
        None => {
            let p = pane_of(conn, &None)?;
            conn.call(m::PaneInfo { pane_id: p })
                .map(|i| i.tab)
                .or_else(err)
        }
    }
}

fn direction(s: &str) -> Result<m::Direction, String> {
    Ok(match s {
        "h" | "left" => m::Direction::H,
        "j" | "down" => m::Direction::J,
        "k" | "up" => m::Direction::K,
        "l" | "right" => m::Direction::L,
        _ => return Err(format!("direction {s:?} is not h, j, k or l")),
    })
}

fn joined(cmd: &[String]) -> Option<String> {
    (!cmd.is_empty()).then(|| cmd.join(" "))
}

fn info_json(i: &m::PaneInfoOk) -> Value {
    serde_json::to_value(i).unwrap_or(Value::Null)
}

fn run_cmd(cli: &Cli, cfg: &Config, socket: &Path, all_plugins: Vec<Plugin>, cmd: &Command) -> R {
    let server = cli
        .server
        .clone()
        .unwrap_or_else(|| cfg.daemon.name.clone());
    let enabled = &cfg.plugins.enabled;
    match &cli.command {
        None => {
            if !running(socket) {
                spawn_daemon(&server, socket).map_err(|e| e.to_string())?;
            }
            femux_client::run(&local(socket), None, host(cfg, all_plugins, cmd))
                .map_err(|e| e.to_string())?;
            ok("", json!({}))
        }
        Some(Cmd::Daemon {
            foreground: true, ..
        }) => {
            run_daemon(
                &server,
                socket.to_path_buf(),
                cfg,
                plugins::enabled(enabled),
            )
            .map_err(|e| e.to_string())?;
            ok("", json!({}))
        }
        Some(Cmd::Daemon {
            foreground: false,
            ensure: true,
        }) => {
            if !running(socket) {
                spawn_daemon(&server, socket).map_err(|e| e.to_string())?;
            }
            ok(socket.display().to_string(), json!({"socket": socket}))
        }
        Some(Cmd::Daemon {
            foreground: false,
            ensure: false,
        }) => {
            if running(socket) {
                return Err(format!(
                    "a daemon is already running at {}",
                    socket.display()
                ));
            }
            spawn_daemon(&server, socket).map_err(|e| e.to_string())?;
            ok("", json!({"socket": socket}))
        }
        Some(Cmd::Attach {
            session,
            host: h,
            stdio,
        }) => {
            if *stdio {
                if !running(socket) {
                    spawn_daemon(&server, socket).map_err(|e| e.to_string())?;
                }
                stdio_proxy(socket).map_err(|e| e.to_string())?;
                // The link is gone: nothing may follow on stdout.
                std::process::exit(0);
            }
            let target = match h {
                None => local(socket),
                Some(h) => {
                    let hosts = femux_config::HostsFile::load(&femux_config::hosts_path())?;
                    let (label, e) = hosts.resolve(h);
                    femux_client::Target::Remote(femux_client::Remote {
                        label,
                        ssh: e.ssh,
                        femux: e.femux.unwrap_or_else(|| "femux".into()),
                        forward: e.forward,
                    })
                }
            };
            femux_client::run(&target, session.as_deref(), host(cfg, all_plugins, cmd))
                .map_err(|e| e.to_string())?;
            ok("", json!({}))
        }
        Some(Cmd::Ls) => {
            let list = connect(socket)?
                .call(m::SessionList {})
                .or_else(err)?
                .sessions;
            let text = list
                .iter()
                .map(|s| format!("{}: {} tabs, {} clients", s.name, s.tabs, s.clients))
                .collect::<Vec<_>>()
                .join("\n");
            ok(text, json!({"sessions": list}))
        }
        Some(Cmd::New { name, root }) => {
            if !running(socket) {
                spawn_daemon(&server, socket).map_err(|e| e.to_string())?;
            }
            connect(socket)?
                .call(m::SessionCreate {
                    name: name.clone(),
                    root: root.clone(),
                    file: None,
                })
                .or_else(err)?;
            ok("", json!({"name": name}))
        }
        Some(Cmd::Kill { name }) => {
            connect(socket)?
                .call(m::SessionKill { name: name.clone() })
                .or_else(err)?;
            ok("", json!({"name": name}))
        }
        Some(Cmd::Tab(t)) => {
            let mut c = connect(socket)?;
            match t {
                TabCmd::New {
                    session,
                    cwd,
                    title,
                    index,
                    cmd,
                } => {
                    let s = session_of(&mut c, session)?;
                    let r = c
                        .call(m::TabCreate {
                            session: s,
                            cwd: cwd.clone(),
                            cmd: joined(cmd),
                            title: title.clone(),
                            index: *index,
                            restore: None,
                        })
                        .or_else(err)?;
                    ok(
                        r.tab_id.clone(),
                        json!({"tab_id": r.tab_id, "pane_id": r.pane_id}),
                    )
                }
                TabCmd::Close { tab } => {
                    let id = tab_of(&mut c, tab, &None)?;
                    c.call(m::TabClose { tab_id: id.clone() }).or_else(err)?;
                    ok("", json!({"tab_id": id}))
                }
                TabCmd::Rename { title, tab } => {
                    let id = tab_of(&mut c, tab, &None)?;
                    c.call(m::TabRename {
                        tab_id: id.clone(),
                        title: title.clone(),
                    })
                    .or_else(err)?;
                    ok("", json!({"tab_id": id, "title": title}))
                }
                TabCmd::Select { tab, session } => {
                    let id = tab_of(&mut c, &Some(tab.clone()), session)?;
                    c.call(m::TabSelect {
                        view_id: None,
                        tab_id: id.clone(),
                    })
                    .or_else(err)?;
                    ok("", json!({"tab_id": id}))
                }
            }
        }
        Some(Cmd::Pane(p)) => {
            let mut c = connect(socket)?;
            match p {
                PaneCmd::Split {
                    sel,
                    dir,
                    cwd,
                    preselect,
                    cmd,
                } => {
                    let pane = pane_of(&c, &sel.pane)?;
                    let dir = match dir.as_str() {
                        "h" => m::SplitDir::H,
                        "v" => m::SplitDir::V,
                        d => return Err(format!("dir {d:?} is not h or v")),
                    };
                    let preselect = preselect.as_deref().map(direction).transpose()?;
                    let r = c
                        .call(m::LayoutSplit {
                            pane_id: pane,
                            dir,
                            ratio: None,
                            cwd: cwd.clone(),
                            cmd: joined(cmd),
                            preselect,
                        })
                        .or_else(err)?;
                    ok(r.pane_id.clone(), json!({"pane_id": r.pane_id}))
                }
                PaneCmd::Close { sel } => {
                    let pane = pane_of(&c, &sel.pane)?;
                    c.call(m::LayoutClose {
                        pane_id: pane.clone(),
                    })
                    .or_else(err)?;
                    ok("", json!({"pane_id": pane}))
                }
                PaneCmd::Focus { target, sel } => {
                    let (pane_id, dir) = if is_ulid(target) {
                        (target.clone(), None)
                    } else {
                        (pane_of(&c, &sel.pane)?, Some(direction(target)?))
                    };
                    let r = c
                        .call(m::LayoutFocus {
                            view_id: None,
                            pane_id: Some(pane_id),
                            dir,
                        })
                        .or_else(err)?;
                    ok(r.pane_id.clone(), json!({"pane_id": r.pane_id}))
                }
                PaneCmd::Resize { dir, delta, sel } => {
                    let pane = pane_of(&c, &sel.pane)?;
                    c.call(m::LayoutResize {
                        pane_id: pane.clone(),
                        dir: direction(dir)?,
                        delta_cells: *delta,
                    })
                    .or_else(err)?;
                    ok("", json!({"pane_id": pane}))
                }
                PaneCmd::Info { sel } => {
                    let pane = pane_of(&c, &sel.pane)?;
                    let i = c.call(m::PaneInfo { pane_id: pane }).or_else(err)?;
                    let mut text = format!(
                        "id: {}\ntab: {}\nsession: {}\npid: {}\ncwd: {}\nfg: {} ({})\ntitle: {}",
                        i.id, i.tab, i.session, i.pid, i.cwd, i.fg_cmd, i.fg_pid, i.title
                    );
                    if let Some(cmd) = &i.cmd {
                        text.push_str(&format!("\ncmd: {cmd}"));
                    }
                    for (k, v) in &i.meta {
                        text.push_str(&format!("\n{k}={v}"));
                    }
                    ok(text, info_json(&i))
                }
                PaneCmd::Set { kv, unset, sel } => {
                    let pane = pane_of(&c, &sel.pane)?;
                    let mut set = std::collections::BTreeMap::new();
                    for pair in kv {
                        let (k, v) = pair
                            .split_once('=')
                            .ok_or_else(|| format!("{pair:?} is not key=value"))?;
                        set.insert(k.to_owned(), v.to_owned());
                    }
                    c.call(m::PaneSetMeta {
                        pane_id: pane.clone(),
                        set: Some(set.clone()),
                        unset: Some(unset.clone()),
                    })
                    .or_else(err)?;
                    ok("", json!({"pane_id": pane, "set": set, "unset": unset}))
                }
                PaneCmd::Cat { lines, sel } => {
                    let pane = pane_of(&c, &sel.pane)?;
                    let n = (*lines).max(1);
                    let r = c
                        .call(m::ScrollbackRead {
                            pane_id: pane,
                            from_line: -(n as i64),
                            count: n + 500,
                        })
                        .or_else(err)?;
                    let mut out: Vec<String> = r
                        .lines
                        .iter()
                        .map(|l| {
                            l.iter()
                                .map(|c| if c.ch.is_empty() { " " } else { c.ch.as_str() })
                                .collect::<String>()
                                .trim_end()
                                .to_owned()
                        })
                        .collect();
                    while out.last().is_some_and(String::is_empty) {
                        out.pop();
                    }
                    let keep = out.len().saturating_sub(n as usize);
                    let out = out.split_off(keep);
                    ok(out.join("\n"), json!({"lines": out}))
                }
                PaneCmd::Signal { sig, sel } => {
                    let pane = pane_of(&c, &sel.pane)?;
                    let sig = match sig.to_ascii_uppercase().as_str() {
                        "SIGINT" | "INT" => m::Signal::Sigint,
                        "SIGTERM" | "TERM" => m::Signal::Sigterm,
                        "SIGKILL" | "KILL" => m::Signal::Sigkill,
                        "SIGHUP" | "HUP" => m::Signal::Sighup,
                        "SIGWINCH" | "WINCH" => m::Signal::Sigwinch,
                        s => return Err(format!("unknown signal {s:?}")),
                    };
                    c.call(m::PaneSignal {
                        pane_id: pane.clone(),
                        sig,
                    })
                    .or_else(err)?;
                    ok("", json!({"pane_id": pane}))
                }
                PaneCmd::Respawn { sel, cmd } => {
                    let pane = pane_of(&c, &sel.pane)?;
                    c.call(m::PaneRespawn {
                        pane_id: pane.clone(),
                        cmd: joined(cmd),
                    })
                    .or_else(err)?;
                    ok("", json!({"pane_id": pane}))
                }
            }
        }
        Some(Cmd::Layout(l)) => match l {
            LayoutCmd::Save { name, tab } => {
                let mut c = connect(socket)?;
                let id = tab_of(&mut c, tab, &None)?;
                let p = layouts::save(&mut c, name, &id)?;
                ok(p.display().to_string(), json!({"name": name, "path": p}))
            }
            LayoutCmd::Load { name, session } => {
                let mut c = connect(socket)?;
                let s = session_of(&mut c, session)?;
                let template = layouts::read(name)?;
                let tab = layouts::load(&mut c, &s, &template)?;
                ok(tab.clone(), json!({"name": name, "tab_id": tab}))
            }
            LayoutCmd::List => {
                let names = layouts::list();
                ok(names.join("\n"), json!({"layouts": names}))
            }
            LayoutCmd::Export { name, tab } => {
                let v = match name {
                    Some(n) => layouts::read(n)?,
                    None => {
                        let mut c = connect(socket)?;
                        let id = tab_of(&mut c, tab, &None)?;
                        layouts::snapshot(&mut c, &id)?
                    }
                };
                ok(serde_json::to_string_pretty(&v).unwrap(), v)
            }
        },
        Some(Cmd::Keys(k)) => {
            let b = femux_client::bindings::Bindings::from_config(&config::client(cfg));
            match k {
                KeysCmd::List => {
                    let list = b.list();
                    let text = list
                        .iter()
                        .map(|(t, k, a, d)| format!("{t:<7}{k:<14}{a:<24}{d}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let json = list
                        .iter()
                        .map(|(t, k, a, d)| json!({"table": t, "key": k, "action": a, "description": d}))
                        .collect::<Vec<_>>();
                    ok(text, json!({"bindings": json}))
                }
                KeysCmd::Explain { key } => {
                    let found = b.explain(key);
                    if found.is_empty() {
                        return Err(format!("{key} is not bound"));
                    }
                    let text = found
                        .iter()
                        .map(|(t, a, d)| format!("{t}: {a} ({d})"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let json = found
                        .iter()
                        .map(|(t, a, d)| json!({"table": t, "action": a, "description": d}))
                        .collect::<Vec<_>>();
                    ok(text, json!({"key": key, "bindings": json}))
                }
            }
        }
        Some(Cmd::Integration(i)) => match i {
            IntegrationCmd::Install { agent } => {
                let p = integration::install(agent)?;
                ok(
                    format!("wrote {}", p.display()),
                    json!({"agent": agent, "path": p}),
                )
            }
            IntegrationCmd::Uninstall { agent } => {
                let p = integration::uninstall(agent)?;
                ok(
                    format!("cleaned {}", p.display()),
                    json!({"agent": agent, "path": p}),
                )
            }
            IntegrationCmd::Hook { agent, event } => {
                // Only a hook without an argument (Claude Code) delivers its event on stdin;
                // Codex passes JSON as the argument with stdin left open.
                let mut stdin = String::new();
                if event.is_none() {
                    let _ = io::Read::read_to_string(&mut io::stdin(), &mut stdin);
                }
                let Some(state) = integration::state_for(agent, event.as_deref(), &stdin) else {
                    return ok("", json!({"ignored": true}));
                };
                let mut c = connect(socket)?;
                let pane = pane_of(&c, &None)?;
                let since = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let set = [
                    ("agent.state", state.to_owned()),
                    ("agent.source", "hook".to_owned()),
                    ("agent.name", agent.clone()),
                    ("agent.since", since.to_string()),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect();
                c.call(m::PaneSetMeta {
                    pane_id: pane.clone(),
                    set: Some(set),
                    unset: None,
                })
                .or_else(err)?;
                ok("", json!({"pane_id": pane, "state": state}))
            }
        },
        Some(Cmd::Reload) => {
            // Checked here first, so the error names the file even without a daemon.
            Config::load(&femux_config::config_path())?;
            connect(socket)?.call(m::ServerReload {}).or_else(err)?;
            ok("reloaded", json!({"reloaded": true}))
        }
        Some(Cmd::Config(c)) => match c {
            ConfigCmd::Check => config_check(),
            ConfigCmd::Edit => {
                let path = femux_config::config_path();
                if !path.exists() {
                    if let Some(d) = path.parent() {
                        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
                    }
                    std::fs::write(&path, EXAMPLE_CONFIG)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                }
                let editor = std::env::var("VISUAL")
                    .or_else(|_| std::env::var("EDITOR"))
                    .unwrap_or_else(|_| "vi".into());
                let status = Proc::new("sh")
                    .arg("-c")
                    .arg(format!("{editor} \"$1\""))
                    .arg("sh")
                    .arg(&path)
                    .status()
                    .map_err(|e| format!("{editor}: {e}"))?;
                if !status.success() {
                    return Err(format!("{editor} exited with {status}"));
                }
                Config::load(&path)?;
                ok(
                    format!("{} is valid", path.display()),
                    json!({"path": path, "ok": true}),
                )
            }
        },
        Some(Cmd::Debug(d)) => run_debug(d, cli.json, socket, &server),
        Some(Cmd::Completions { shell }) => {
            let mut cmd = cmd.clone();
            let mut buf = Vec::new();
            clap_complete::generate(*shell, &mut cmd, "femux", &mut buf);
            let text = String::from_utf8_lossy(&buf).into_owned();
            ok(
                text.clone(),
                json!({"shell": shell.to_string(), "script": text}),
            )
        }
    }
}

/// `femux debug …` (REQ-LOG-003, 007, 008).
fn run_debug(d: &debug::DebugCmd, json: bool, socket: &Path, server: &str) -> R {
    use debug::DebugCmd as D;
    use femux_log::Level;
    let set = |level: Level| -> R {
        let mut c = connect(socket)?;
        let r = debug::set(&mut c, Some(level))?;
        let (text, json) = debug::report(&r);
        ok(text, json)
    };
    match d {
        D::On { trace } => set(if *trace { Level::Trace } else { Level::Debug }),
        D::Crash => panic!("femux debug crash"),
        D::Off => set(Level::Warn),
        D::Status => {
            if !running(socket) {
                let path = femux_log::path(server);
                return ok(
                    format!("no daemon at {}\nlog: {}", socket.display(), path.display()),
                    json!({"running": false, "debug": false, "path": path}),
                );
            }
            let mut c = connect(socket)?;
            let (text, mut j) = debug::report(&debug::set(&mut c, None)?);
            j["running"] = true.into();
            ok(text, j)
        }
        D::Path => {
            let path = if running(socket) {
                debug::log_path(connect(socket).ok().as_mut(), server)
            } else {
                femux_log::path(server)
            };
            ok(path.display().to_string(), json!({"path": path}))
        }
        D::Snapshot => {
            let mut c = connect(socket)?;
            let fields = debug::snapshot(&mut c)?;
            let path = femux_log::current_path().unwrap_or_else(|| femux_log::path(server));
            ok(
                format!("snapshot written to {}", path.display()),
                json!({"path": path, "snapshot": fields}),
            )
        }
        D::Tail {
            level,
            event,
            pane,
            since,
            follow,
        } => {
            let f = debug::Filter::new(level, event.clone(), pane.clone(), since.as_deref())?;
            let path = if running(socket) {
                debug::log_path(connect(socket).ok().as_mut(), server)
            } else {
                femux_log::path(server)
            };
            debug::tail(&path, &f, *follow, json).map_err(|e| e.to_string())?;
            Ok(Out {
                text: String::new(),
                json: Value::Null,
                printed: true,
                attach: None,
            })
        }
    }
}

/// The commented defaults, written by `config edit` when there is no config yet.
const EXAMPLE_CONFIG: &str = include_str!("../../../spec/config.example.toml");

/// `femux config check`: config.toml, then every session file (REQ-CFG-003).
fn config_check() -> R {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let path = femux_config::config_path();
    match Config::load(&path) {
        Ok(_) => files.push(json!({"path": path, "ok": true, "exists": path.exists()})),
        Err(e) => {
            files.push(json!({"path": path, "ok": false, "error": e}));
            errors.push(e);
        }
    }
    let mut sessions: Vec<PathBuf> = std::fs::read_dir(femux_config::sessions_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    sessions.sort();
    for p in sessions {
        match femux_config::SessionFile::load(&p) {
            Ok(_) => files.push(json!({"path": p, "ok": true})),
            Err(e) => {
                files.push(json!({"path": p, "ok": false, "error": e}));
                errors.push(e);
            }
        }
    }
    if errors.is_empty() {
        let text = files
            .iter()
            .map(|f| format!("ok  {}", f["path"].as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n");
        ok(text, json!({"ok": true, "files": files}))
    } else {
        Err(errors.join("\n"))
    }
}

/// What the client gets: its config, the enabled client plugins, CLI names for the palette.
fn host(cfg: &Config, plugins: Vec<Plugin>, cmd: &Command) -> femux_client::Host {
    let mut cli_commands = Vec::new();
    for sub in cmd.get_subcommands() {
        if sub.is_hide_set() || matches!(sub.get_name(), "daemon" | "attach" | "completions") {
            continue;
        }
        let subs: Vec<_> = sub.get_subcommands().filter(|s| !s.is_hide_set()).collect();
        if subs.is_empty() {
            cli_commands.push(sub.get_name().to_owned());
        }
        for s in subs {
            cli_commands.push(format!("{} {}", sub.get_name(), s.get_name()));
        }
    }
    femux_client::Host {
        cfg: config::client(cfg),
        plugins: plugins.into_iter().filter_map(|p| p.client).collect(),
        cli_commands,
        reload: Some(Box::new(|| {
            Config::load(&femux_config::config_path()).map(|c| config::client(&c))
        })),
        debug: debug::requested_level().map(debug::to_proto),
        metrics_interval: Duration::from_millis(cfg.debug.metrics_interval_ms),
    }
}

/// Adds every enabled plugin's subcommands (one nesting level: `clipboard set`).
fn with_plugin_commands(mut cmd: Command, plugins: &[Plugin]) -> Command {
    // Group by first word so `clipboard set` and `clipboard get` share one parent.
    let mut groups: Vec<(&'static str, Option<&'static str>, Vec<Command>)> = Vec::new();
    for p in plugins.iter().filter_map(|p| p.cli.as_ref()) {
        for c in p.commands() {
            let mut parts = c.name.split(' ');
            let first: &'static str = parts.next().unwrap_or(c.name);
            let rest: Vec<&'static str> = parts.collect();
            let mut leaf = Command::new(rest.last().copied().unwrap_or(first)).about(c.about);
            for a in &c.args {
                leaf = leaf.arg(Arg::new(a.name).help(a.help).required(a.required));
            }
            for f in &c.flags {
                let arg = Arg::new(f.name).long(f.name).help(f.help);
                leaf = leaf.arg(if f.value {
                    arg.num_args(1)
                } else {
                    arg.action(clap::ArgAction::SetTrue)
                });
            }
            if let Some(t) = c.trailing {
                leaf = leaf.arg(Arg::new(t).num_args(0..).last(true));
            }
            if rest.is_empty() {
                cmd = cmd.subcommand(leaf);
                continue;
            }
            match groups.iter_mut().find(|(f, _, _)| *f == first) {
                Some((_, _, leaves)) => leaves.push(leaf),
                None => groups.push((first, Some(p.name()), vec![leaf])),
            }
        }
    }
    for (first, plugin, leaves) in groups {
        let mut parent = Command::new(first)
            .about(format!("{} plugin", plugin.unwrap_or(first)))
            .subcommand_required(true)
            .arg_required_else_help(true);
        for l in leaves {
            parent = parent.subcommand(l);
        }
        cmd = cmd.subcommand(parent);
    }
    cmd
}

/// Runs a plugin subcommand if `matches` names one.
fn run_plugin(matches: &ArgMatches, plugins: &[Plugin], socket: &Path, server: &str) -> Option<R> {
    let (name, sub) = matches.subcommand()?;
    let (full, leaf) = match sub.subcommand() {
        Some((n2, s2)) => (format!("{name} {n2}"), s2),
        None => (name.to_owned(), sub),
    };
    for p in plugins.iter().filter_map(|p| p.cli.as_ref()) {
        let Some(spec) = p.commands().into_iter().find(|c| c.name == full) else {
            continue;
        };
        let mut input = femux_plugin::CliInput {
            args: spec
                .args
                .iter()
                .filter_map(|a| leaf.get_one::<String>(a.name).cloned())
                .collect(),
            ..Default::default()
        };
        for f in &spec.flags {
            if f.value {
                if let Some(v) = leaf.get_one::<String>(f.name) {
                    input.flags.insert(f.name.to_owned(), Some(v.clone()));
                }
            } else if leaf.get_flag(f.name) {
                input.flags.insert(f.name.to_owned(), None);
            }
        }
        if let Some(t) = spec.trailing {
            input.trailing = leaf
                .get_many::<String>(t)
                .map(|v| v.cloned().collect())
                .unwrap_or_default();
        }
        let mut core = LazyCore {
            socket: socket.to_path_buf(),
            server: server.to_owned(),
            conn: None,
        };
        return Some(p.run(&full, &input, &mut core).map(|o| Out {
            printed: false,
            text: o.text,
            json: o.json,
            attach: o.attach,
        }));
    }
    None
}

/// Connects on the first request, so commands like `shell-integration` need no daemon.
struct LazyCore {
    socket: PathBuf,
    server: String,
    conn: Option<Conn>,
}

impl LazyCore {
    fn conn(&mut self) -> Result<&mut Conn, femux_proto::ErrorBody> {
        if self.conn.is_none() {
            self.conn = Some(
                Conn::open(&self.socket).map_err(|e| femux_proto::ErrorBody {
                    code: m::ErrorCode::Internal,
                    message: e.to_string(),
                    data: None,
                })?,
            );
        }
        Ok(self.conn.as_mut().unwrap())
    }
}

impl CliCore for LazyCore {
    fn request(
        &mut self,
        req: femux_proto::Request,
    ) -> Result<femux_proto::Response, femux_proto::ErrorBody> {
        self.conn()?.request(req)
    }
    fn pane(&self) -> Option<String> {
        std::env::var("FEMUX_PANE").ok().filter(|p| !p.is_empty())
    }
    fn socket(&self) -> &Path {
        &self.socket
    }
    fn stdin(&mut self) -> String {
        let mut s = String::new();
        let _ = io::Read::read_to_string(&mut io::stdin(), &mut s);
        s
    }
    fn start_daemon(&mut self) -> Result<(), String> {
        if running(&self.socket) {
            return Ok(());
        }
        spawn_daemon(&self.server, &self.socket).map_err(|e| e.to_string())
    }
    fn next_event(&mut self, timeout: Option<Duration>) -> Option<femux_proto::Event> {
        self.conn().ok()?.next_event(timeout)
    }
}

/// The first positional argument (the subcommand), skipping global options and their values.
fn subcommand_name() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--server" | "--socket" => {
                args.next();
            }
            a if a.starts_with('-') => {}
            a => return Some(a.to_owned()),
        }
    }
    None
}

fn main() -> ExitCode {
    let cfg = match Config::load(&femux_config::config_path()) {
        Ok(c) => c,
        // `config check` and `config edit` are how a bad config gets fixed: they run on the
        // defaults and report the error themselves.
        Err(_) if subcommand_name().as_deref() == Some("config") => Config::default(),
        Err(e) => {
            eprintln!("femux: config: {e}");
            return ExitCode::FAILURE;
        }
    };
    let plugins = plugins::enabled(&cfg.plugins.enabled);
    let cmd = with_plugin_commands(Cli::command(), &plugins);
    let matches = cmd.clone().get_matches();
    let json = matches.get_flag("json");
    let requested = match debug::requested(matches.get_one::<String>("debug").map(String::as_str)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("femux: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _ = debug::REQUESTED.set(requested);
    let sub = matches.subcommand_name().map(str::to_owned);
    let result = (|| {
        // Only the globals are needed to find the socket; the subcommand may be a plugin's.
        let globals = Cli {
            server: matches.get_one::<String>("server").cloned(),
            socket: matches.get_one::<PathBuf>("socket").cloned(),
            json,
            debug: None,
            command: None,
        };
        let socket = socket_path(&globals, &cfg);
        let server = globals
            .server
            .clone()
            .unwrap_or_else(|| cfg.daemon.name.clone());
        // REQ-LOG-001/003: every process logs to its server's file; the daemon at least at
        // `[debug] level`, everything else at what was asked for, else warnings only.
        let proc = match matches.subcommand() {
            None => femux_log::Proc::Client,
            Some(("attach", a)) if !a.get_flag("stdio") => femux_log::Proc::Client,
            Some(("daemon", a)) if a.get_flag("foreground") => femux_log::Proc::Daemon,
            _ => femux_log::Proc::Cli,
        };
        let asked = requested.unwrap_or(femux_log::Level::Warn);
        let level = match proc {
            femux_log::Proc::Daemon => asked.max(debug::from_config(cfg.debug.level)),
            _ => asked,
        };
        debug::init(&server, proc, level, &cfg);
        if proc != femux_log::Proc::Daemon {
            femux_log::start(json!({"command": sub.clone().unwrap_or_default()}));
        }
        if let Some(r) = run_plugin(&matches, &plugins, &socket, &server) {
            let out = r?;
            // Attach afterwards only from a terminal, outside femux, and not for --json.
            if let Some(session) = &out.attach
                && !json
                && std::env::var_os("FEMUX_PANE").is_none()
                && unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
            {
                femux_client::run(&local(&socket), Some(session), host(&cfg, plugins, &cmd))
                    .map_err(|e| e.to_string())?;
                return ok("", out.json);
            }
            return Ok(out);
        }
        let cli = Cli::from_arg_matches(&matches).map_err(|e| e.to_string())?;
        run_cmd(&cli, &cfg, &socket, plugins, &cmd)
    })();
    let code = i32::from(result.is_err());
    if let Err(e) = &result {
        femux_log::warn("cli", "cli.error")
            .msg(e.clone())
            .field("command", sub.clone().unwrap_or_default())
            .emit();
    }
    femux_log::stop(sub.as_deref().unwrap_or("attach"), code);
    match result {
        Ok(out) if out.printed => ExitCode::SUCCESS,
        Ok(out) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&out.json).unwrap());
            } else if !out.text.is_empty() {
                println!("{}", out.text);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                println!("{}", json!({"error": e}));
            } else {
                eprintln!("femux: {e}");
            }
            ExitCode::FAILURE
        }
    }
}
