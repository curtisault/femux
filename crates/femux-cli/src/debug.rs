//! The binary's side of SPEC §3.9: which level a process starts at (`--debug`,
//! `$FEMUX_DEBUG`, `[debug] level`), starting its log, and `femux debug
//! on|off|status|path|snapshot|tail` (REQ-LOG-003, 007, 008).

use std::fs::File;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Subcommand;
use femux_log::{Level, Proc};
use femux_proto::msg as m;
use serde_json::{Value, json};

use crate::config::Config;
use crate::conn::Conn;

#[derive(Subcommand)]
pub enum DebugCmd {
    /// Put the running daemon (and its clients) in debug mode.
    On {
        /// Trace mode: debug mode plus protocol bodies.
        #[arg(long)]
        trace: bool,
    },
    /// Back to warnings and errors only.
    Off,
    /// The daemon's level, the log's path and size, and since when.
    Status,
    /// Print the log's path.
    Path,
    /// Write the server's whole state into the log as one record.
    Snapshot,
    /// Panic on purpose, to check that panics reach the log (tests).
    #[command(hide = true)]
    Crash,
    /// Print the log, filtered; --follow keeps printing new records.
    Tail {
        /// This level and above: error, warn, info, debug or trace.
        #[arg(long, default_value = "trace")]
        level: String,
        /// Only events matching this pattern (`*` matches anything, e.g. `agent.*`).
        #[arg(long)]
        event: Option<String>,
        /// Only records about this pane.
        #[arg(long)]
        pane: Option<String>,
        /// Only records newer than this: 30s, 5m, 1h, 2d.
        #[arg(long)]
        since: Option<String>,
        /// Keep printing records as they are written.
        #[arg(long, short)]
        follow: bool,
    },
}

/// What `--debug` / `$FEMUX_DEBUG` asked for, set once in `main`.
pub static REQUESTED: std::sync::OnceLock<Option<Level>> = std::sync::OnceLock::new();

pub fn requested_level() -> Option<Level> {
    REQUESTED.get().copied().flatten()
}

/// `off`, `1`, `debug`, `trace`, … as a level: `None` is "not requested".
pub fn parse_request(s: &str) -> Result<Option<Level>, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" | "false" | "no" => Ok(None),
        "1" | "on" | "true" | "yes" | "debug" => Ok(Some(Level::Debug)),
        "trace" => Ok(Some(Level::Trace)),
        other => Err(format!(
            "debug level {other:?}: use debug or trace (or 1 / off)"
        )),
    }
}

/// `--debug[=trace]`, else `$FEMUX_DEBUG` (REQ-LOG-003).
pub fn requested(flag: Option<&str>) -> Result<Option<Level>, String> {
    match flag {
        Some(f) => parse_request(f),
        None => parse_request(&std::env::var("FEMUX_DEBUG").unwrap_or_default()),
    }
}

pub fn to_proto(l: Level) -> m::LogLevel {
    match l {
        Level::Trace => m::LogLevel::Trace,
        Level::Debug | Level::Info => m::LogLevel::Debug,
        Level::Warn | Level::Error => m::LogLevel::Warn,
    }
}

pub fn from_config(l: femux_config::LogLevel) -> Level {
    match l {
        femux_config::LogLevel::Warn => Level::Warn,
        femux_config::LogLevel::Debug => Level::Debug,
        femux_config::LogLevel::Trace => Level::Trace,
    }
}

/// Starts this process's log and its panic hook. A log that cannot be opened is not an error:
/// femux runs without it.
pub fn init(server: &str, proc: Proc, level: Level, cfg: &Config) {
    let mut o = femux_log::Options::new(server, proc, level);
    o.max_bytes = cfg.debug.max_mb.max(1) * 1024 * 1024;
    if femux_log::init(o).is_ok() {
        femux_log::install_panic_hook();
    }
}

fn level_name(l: m::LogLevel) -> &'static str {
    match l {
        m::LogLevel::Warn => "warn",
        m::LogLevel::Debug => "debug",
        m::LogLevel::Trace => "trace",
    }
}

fn ago(since_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let s = now.saturating_sub(since_ms) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h{}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d", s / 86_400),
    }
}

/// What `on`, `off` and `status` print.
pub fn report(d: &m::ServerDebugOk) -> (String, Value) {
    let size = std::fs::metadata(&d.path).map_or(0, |m| m.len());
    let on = d.level != m::LogLevel::Warn;
    let text = format!(
        "debug mode {} ({}, for {})\nlog: {} ({:.1} MB)",
        if on { "on" } else { "off" },
        level_name(d.level),
        ago(d.since),
        d.path,
        size as f64 / 1_048_576.0
    );
    let json = json!({
        "debug": on,
        "level": level_name(d.level),
        "path": d.path,
        "bytes": size,
        "since": d.since,
    });
    (text, json)
}

pub fn set(conn: &mut Conn, level: Option<Level>) -> Result<m::ServerDebugOk, String> {
    conn.call(m::ServerDebug {
        level: level.map(to_proto),
    })
    .map_err(|e| format!("{}: {}", e.code.as_str(), e.message))
}

/// `femux debug snapshot` (REQ-LOG-007): the server's state from read requests, as one
/// `debug.snapshot` record in the log whatever the level. Returns the record's fields.
pub fn snapshot(conn: &mut Conn) -> Result<Value, String> {
    let e = |e: femux_proto::ErrorBody| format!("{}: {}", e.code.as_str(), e.message);
    let info = conn.call(m::ServerInfo {}).map_err(e)?;
    let level = set(conn, None).ok();
    let sessions = conn.call(m::SessionList {}).map_err(e)?.sessions;
    let mut out_sessions = Vec::new();
    for s in &sessions {
        let tabs = conn
            .call(m::SessionTabs {
                name: s.name.clone(),
            })
            .map_err(e)?
            .tabs;
        let mut panes = Vec::new();
        for t in &tabs {
            let mut ids = Vec::new();
            collect(&t.tree, &mut ids);
            ids.extend(t.floats.iter().map(|f| f.id.clone()));
            for id in ids {
                let Ok(p) = conn.call(m::PaneInfo {
                    pane_id: id.clone(),
                }) else {
                    continue;
                };
                // REQ-LOG-004: no title (a shell may put a whole command line there).
                panes.push(json!({
                    "id": p.id,
                    "tab": p.tab,
                    "cwd": p.cwd,
                    "fg_cmd": femux_log::program(&p.fg_cmd),
                    "fg_pid": p.fg_pid,
                    "exit": p.exit,
                    "meta": p.meta,
                }));
            }
        }
        let tabs: Vec<Value> = tabs
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "index": t.index,
                    "tree": t.tree,
                    "floats": t.floats,
                    "zoomed": t.zoomed,
                    "focus": t.focus,
                    "broadcast": t.broadcast,
                })
            })
            .collect();
        out_sessions.push(json!({
            "name": s.name,
            "clients": s.clients,
            "tabs": tabs,
            "panes": panes,
        }));
    }
    let cfg = femux_config::config_path();
    let fields = json!({
        "server": {
            "version": info.version,
            "uptime_ms": info.uptime,
            "sessions": info.sessions,
            "panes": info.panes,
            "clients": info.clients,
        },
        "debug_level": level.as_ref().map(|l| level_name(l.level)),
        "config": cfg.display().to_string(),
        "config_hash": std::fs::read(&cfg).ok().map(|b| format!("{:016x}", femux_log::fnv1a(&b))),
        "plugins": Config::load(&cfg).map(|c| c.plugins.enabled).unwrap_or_default(),
        "sessions": out_sessions,
    });
    femux_log::always(Level::Info, "cli", "debug.snapshot")
        .msg(format!(
            "snapshot: {} sessions, {} panes, {} clients",
            info.sessions, info.panes, info.clients
        ))
        .fields(fields.clone())
        .emit();
    Ok(fields)
}

fn collect(n: &m::LayoutNode, out: &mut Vec<String>) {
    match n {
        m::LayoutNode::PaneNode(p) => out.push(p.id.clone()),
        m::LayoutNode::SplitNode(s) => s.children.iter().for_each(|c| collect(c, out)),
    }
}

/// Which records `tail` prints (REQ-LOG-008).
pub struct Filter {
    pub level: Level,
    pub event: Option<String>,
    pub pane: Option<String>,
    /// RFC 3339 timestamps compare as strings.
    pub after: Option<String>,
}

impl Filter {
    pub fn new(
        level: &str,
        event: Option<String>,
        pane: Option<String>,
        since: Option<&str>,
    ) -> Result<Filter, String> {
        let level = Level::parse(level)
            .ok_or_else(|| format!("level {level:?}: use error, warn, info, debug or trace"))?;
        let after = since
            .map(|s| {
                let d = parse_duration(s)?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                Ok::<_, String>(femux_log::rfc3339_ms(
                    now.saturating_sub(d.as_millis() as u64),
                ))
            })
            .transpose()?;
        Ok(Filter {
            level,
            event,
            pane,
            after,
        })
    }

    pub fn matches(&self, r: &Value) -> bool {
        let lvl = r["level"].as_str().and_then(Level::parse);
        if lvl.is_none_or(|l| l > self.level) {
            return false;
        }
        if let Some(g) = &self.event
            && !glob(g, r["event"].as_str().unwrap_or(""))
        {
            return false;
        }
        if let Some(p) = &self.pane
            && r["pane"].as_str() != Some(p.as_str())
        {
            return false;
        }
        if let Some(a) = &self.after
            && r["ts"].as_str().is_none_or(|t| t < a.as_str())
        {
            return false;
        }
        true
    }
}

/// `30s`, `5m`, `1h`, `2d`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("duration {s:?}: use a number and s, m, h or d (30s, 5m)");
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?);
    let n: u64 = n.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return Err(bad()),
    };
    Ok(Duration::from_secs(secs))
}

/// `*` matches any run of characters; everything else matches itself.
pub fn glob(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(p) else {
                return false;
            };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            let Some(at) = rest.find(p) else {
                return false;
            };
            rest = &rest[at + p.len()..];
        }
    }
    true
}

/// One record as a line for people: time, level, process, target, event, pane, message.
pub fn human(r: &Value, color: bool) -> String {
    let ts = r["ts"].as_str().unwrap_or("");
    let time = ts.get(11..23).unwrap_or(ts);
    let level = r["level"].as_str().unwrap_or("?");
    let lv = format!("{level:<5}");
    let lv = if color {
        let c = match level {
            "error" => "31",
            "warn" => "33",
            "info" => "32",
            "debug" => "36",
            _ => "2",
        };
        format!("\x1b[{c}m{lv}\x1b[0m")
    } else {
        lv
    };
    let mut s = format!(
        "{time} {lv} {:<6} {} {}",
        r["proc"].as_str().unwrap_or(""),
        r["target"].as_str().unwrap_or(""),
        r["event"].as_str().unwrap_or(""),
    );
    if let Some(p) = r["pane"].as_str() {
        s.push_str(&format!(" pane {}", p.get(..8).unwrap_or(p)));
    }
    let msg = r["msg"].as_str().unwrap_or("");
    if !msg.is_empty() {
        s.push_str(": ");
        s.push_str(msg);
    }
    s
}

fn print_line(
    out: &mut impl Write,
    line: &str,
    f: &Filter,
    json: bool,
    color: bool,
) -> io::Result<()> {
    let Ok(r) = serde_json::from_str::<Value>(line) else {
        return Ok(());
    };
    if !f.matches(&r) {
        return Ok(());
    }
    if json {
        writeln!(out, "{}", line.trim_end())
    } else {
        writeln!(out, "{}", human(&r, color))
    }
}

/// `femux debug tail`: the rotated file, then the current one; with `follow`, new records as
/// they arrive, across rotations, until interrupted.
pub fn tail(path: &Path, f: &Filter, follow: bool, json: bool) -> io::Result<()> {
    let color = !json && unsafe { libc::isatty(1) == 1 };
    let mut out = io::stdout().lock();
    if let Ok(file) = File::open(femux_log::rotated(path)) {
        for l in io::BufReader::new(file).lines() {
            print_line(&mut out, &l?, f, json, color)?;
        }
    }
    let mut cur: Option<(File, u64)> = None;
    let mut pending = String::new();
    loop {
        if cur.is_none()
            && let Ok(file) = File::open(path)
        {
            let ino = file.metadata()?.ino();
            cur = Some((file, ino));
        }
        if let Some((file, _)) = &mut cur {
            let mut buf = String::new();
            file.read_to_string(&mut buf)?;
            pending.push_str(&buf);
            while let Some(nl) = pending.find('\n') {
                let line: String = pending.drain(..=nl).collect();
                print_line(&mut out, &line, f, json, color)?;
            }
            out.flush()?;
        }
        if !follow {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
        // Rotated: finish nothing more from the old file, start the new one at its start.
        let moved = match (&cur, std::fs::metadata(path)) {
            (Some((_, ino)), Ok(m)) => m.ino() != *ino,
            (Some(_), Err(_)) => true,
            (None, _) => false,
        };
        if moved {
            if let Some((file, _)) = &mut cur {
                let pos = file.stream_position()?;
                file.seek(SeekFrom::Start(pos))?;
                let mut rest = String::new();
                file.read_to_string(&mut rest)?;
                pending.push_str(&rest);
                while let Some(nl) = pending.find('\n') {
                    let line: String = pending.drain(..=nl).collect();
                    print_line(&mut out, &line, f, json, color)?;
                }
            }
            cur = None;
            pending.clear();
        }
    }
}

/// The log a command reads: the daemon's when one answers, else this server's path.
pub fn log_path(conn: Option<&mut Conn>, server: &str) -> PathBuf {
    conn.and_then(|c| set(c, None).ok())
        .map(|d| PathBuf::from(d.path))
        .unwrap_or_else(|| femux_log::path(server))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_003_debug_requests_parse() {
        assert_eq!(parse_request("1").unwrap(), Some(Level::Debug));
        assert_eq!(parse_request("trace").unwrap(), Some(Level::Trace));
        assert_eq!(parse_request("").unwrap(), None);
        assert_eq!(parse_request("off").unwrap(), None);
        assert!(parse_request("loud").is_err());
    }

    #[test]
    fn log_008_filters_globs_and_durations() {
        assert!(glob("agent.*", "agent.state"));
        assert!(glob("*.state", "agent.state"));
        assert!(glob("*", "x"));
        assert!(!glob("agent.*", "pane.spawn"));
        assert!(glob("pane.spawn", "pane.spawn"));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert!(parse_duration("5").is_err() && parse_duration("m").is_err());
        let f = Filter::new("warn", Some("agent.*".into()), Some("P".into()), None).unwrap();
        let r = |level: &str, event: &str, pane: &str| json!({"ts": "2026-10-14T20:00:00.000Z", "level": level, "event": event, "pane": pane});
        assert!(f.matches(&r("warn", "agent.state", "P")));
        assert!(f.matches(&r("error", "agent.state", "P")));
        assert!(!f.matches(&r("info", "agent.state", "P")));
        assert!(!f.matches(&r("warn", "pane.spawn", "P")));
        assert!(!f.matches(&r("warn", "agent.state", "Q")));
        let line = human(
            &json!({"ts": "2026-10-14T20:16:58.123Z", "level": "warn", "proc": "daemon",
                    "target": "plugin.agents", "event": "agent.state",
                    "pane": "01M4Y0T73TM87Z5C7F8KZT2Q8S", "msg": "blocked → done"}),
            false,
        );
        assert_eq!(
            line,
            "20:16:58.123 warn  daemon plugin.agents agent.state pane 01M4Y0T7: blocked → done"
        );
    }
}
