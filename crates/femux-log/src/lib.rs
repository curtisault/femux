//! The femux log (SPEC §3.9): one JSON Lines file per server that every femux process talking
//! to that server appends to (REQ-LOG-001), records of one shape (REQ-LOG-002), levels and
//! debug mode (REQ-LOG-003), and the metrics debug mode adds (REQ-LOG-006).
//!
//! A process calls [`init`] once it knows its server; until then, and in a process that never
//! calls it (the in-process test harness), every record is dropped. Records are built with
//! [`error`], [`warn`], [`info`], [`debug`] or [`trace`] and written by [`Rec::emit`]. A record
//! below the current level costs one atomic load: its builder carries nothing.
//!
//! Nothing here sends anything anywhere but the file (REQ-LOG-006). What may be put in a record
//! is the caller's duty under REQ-LOG-004; [`program`] and [`count`] help.

mod metrics;
mod time;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

pub use metrics::{Histogram, Metrics, open_fds, rss_bytes, threads};
pub use time::rfc3339_ms;

/// A record over this many bytes has its `fields` cut (REQ-LOG-001).
pub const MAX_RECORD: usize = 16 * 1024;

/// Severity, most severe first: a level lets through itself and everything before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    pub const ALL: [Level; 5] = [
        Level::Error,
        Level::Warn,
        Level::Info,
        Level::Debug,
        Level::Trace,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|l| l.as_str() == s)
    }

    /// Debug mode is `debug` or `trace` (REQ-LOG-003).
    pub fn is_debug(self) -> bool {
        self >= Level::Debug
    }

    fn from_u8(v: u8) -> Option<Level> {
        Level::ALL.into_iter().find(|l| *l as u8 == v)
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which kind of femux process wrote a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proc {
    Daemon,
    Client,
    Cli,
}

impl Proc {
    pub fn as_str(self) -> &'static str {
        match self {
            Proc::Daemon => "daemon",
            Proc::Client => "client",
            Proc::Cli => "cli",
        }
    }
}

/// `<state dir>/logs` (REQ-LOG-001).
pub fn dir() -> PathBuf {
    femux_config::state_dir().join("logs")
}

/// The log of server `server`.
pub fn path(server: &str) -> PathBuf {
    dir().join(format!("{server}.jsonl"))
}

/// Where the log goes when it rotates.
pub fn rotated(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{stem}.1.jsonl"))
}

/// What a process logs as.
#[derive(Debug, Clone)]
pub struct Options {
    pub path: PathBuf,
    pub server: String,
    pub proc: Proc,
    pub level: Level,
    /// Rotate past this size (`[debug] max_mb`).
    pub max_bytes: u64,
}

impl Options {
    pub fn new(server: &str, proc: Proc, level: Level) -> Options {
        Options {
            path: path(server),
            server: server.to_owned(),
            proc,
            level,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

struct Sink {
    file: File,
    ino: u64,
    checked: Instant,
    opts: Options,
}

/// 0 means "not initialised": nothing is written.
static LEVEL: AtomicU8 = AtomicU8::new(0);
/// When the current level was set, ms since the epoch (`server.debug`'s `since`).
static SINCE: AtomicU64 = AtomicU64::new(0);
static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static RUN: OnceLock<String> = OnceLock::new();

/// This process's run id (REQ-LOG-002): a ULID chosen once.
pub fn run_id() -> &'static str {
    RUN.get_or_init(|| ulid::Ulid::new().to_string())
}

fn open(path: &Path) -> io::Result<(File, u64)> {
    if let Some(parent) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let ino = file.metadata()?.ino();
    Ok((file, ino))
}

/// Starts logging for this process. Calling it again switches to the new options.
pub fn init(opts: Options) -> io::Result<()> {
    let (file, ino) = open(&opts.path)?;
    let level = opts.level;
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Sink {
        file,
        ino,
        checked: Instant::now(),
        opts,
    });
    set_level(level);
    Ok(())
}

/// Stops logging (tests).
pub fn shutdown() {
    LEVEL.store(0, Ordering::SeqCst);
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The log this process writes, once [`init`] ran.
pub fn current_path() -> Option<PathBuf> {
    SINK.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| s.opts.path.clone())
}

/// Changes the level; returns the previous one. Before [`init`] it only takes effect then.
pub fn set_level(level: Level) -> Option<Level> {
    let prev = LEVEL.swap(level as u8, Ordering::SeqCst);
    if prev != level as u8 {
        SINCE.store(time::now_ms(), Ordering::SeqCst);
    }
    Level::from_u8(prev)
}

/// The current level, `None` before [`init`].
pub fn level() -> Option<Level> {
    Level::from_u8(LEVEL.load(Ordering::Relaxed))
}

/// When the current level was set (ms since the epoch).
pub fn level_since() -> u64 {
    SINCE.load(Ordering::Relaxed)
}

/// Whether a record at `l` would be written.
pub fn enabled(l: Level) -> bool {
    let cur = LEVEL.load(Ordering::Relaxed);
    cur != 0 && l as u8 <= cur
}

/// Whether debug mode is on (REQ-LOG-003).
pub fn debug_mode() -> bool {
    enabled(Level::Debug)
}

/// A record being built. Created disabled when its level is filtered out.
#[must_use = "a record is written by .emit()"]
pub struct Rec(Option<Box<Inner>>);

struct Inner {
    level: Level,
    target: String,
    event: String,
    msg: String,
    ids: Map<String, Value>,
    fields: Map<String, Value>,
}

pub fn rec(level: Level, target: &str, event: &str) -> Rec {
    if !enabled(level) {
        return Rec(None);
    }
    Rec(Some(Box::new(Inner {
        level,
        target: target.to_owned(),
        event: event.to_owned(),
        msg: String::new(),
        ids: Map::new(),
        fields: Map::new(),
    })))
}

/// A record written whatever the level, once the log is started: what the user asked for
/// explicitly (`femux debug snapshot`, REQ-LOG-007).
pub fn always(level: Level, target: &str, event: &str) -> Rec {
    if LEVEL.load(Ordering::Relaxed) == 0 {
        return Rec(None);
    }
    Rec(Some(Box::new(Inner {
        level,
        target: target.to_owned(),
        event: event.to_owned(),
        msg: String::new(),
        ids: Map::new(),
        fields: Map::new(),
    })))
}

pub fn error(target: &str, event: &str) -> Rec {
    rec(Level::Error, target, event)
}
pub fn warn(target: &str, event: &str) -> Rec {
    rec(Level::Warn, target, event)
}
pub fn info(target: &str, event: &str) -> Rec {
    rec(Level::Info, target, event)
}
pub fn debug(target: &str, event: &str) -> Rec {
    rec(Level::Debug, target, event)
}
pub fn trace(target: &str, event: &str) -> Rec {
    rec(Level::Trace, target, event)
}

impl Rec {
    /// Whether this record will be written: guard costly fields with it.
    pub fn on(&self) -> bool {
        self.0.is_some()
    }

    pub fn msg(mut self, m: impl Into<String>) -> Rec {
        if let Some(i) = &mut self.0 {
            i.msg = m.into();
        }
        self
    }

    fn id(mut self, k: &str, v: &str) -> Rec {
        if let Some(i) = &mut self.0 {
            i.ids.insert(k.to_owned(), Value::String(v.to_owned()));
        }
        self
    }

    pub fn session(self, v: &str) -> Rec {
        self.id("session", v)
    }
    pub fn tab(self, v: &str) -> Rec {
        self.id("tab", v)
    }
    pub fn pane(self, v: &str) -> Rec {
        self.id("pane", v)
    }
    pub fn view(self, v: &str) -> Rec {
        self.id("view", v)
    }

    pub fn field(mut self, k: &str, v: impl Into<Value>) -> Rec {
        if let Some(i) = &mut self.0 {
            i.fields.insert(k.to_owned(), v.into());
        }
        self
    }

    /// Adds every entry of an object value as a field.
    pub fn fields(mut self, v: Value) -> Rec {
        if let (Some(i), Value::Object(m)) = (&mut self.0, v) {
            i.fields.extend(m);
        }
        self
    }

    pub fn emit(self) {
        let Some(i) = self.0 else {
            return;
        };
        write_record(*i);
    }
}

fn line(i: &Inner, opts: &Options) -> String {
    let mut o = Map::new();
    o.insert("ts".into(), time::rfc3339_ms(time::now_ms()).into());
    o.insert("level".into(), i.level.as_str().into());
    o.insert("proc".into(), opts.proc.as_str().into());
    o.insert("pid".into(), std::process::id().into());
    o.insert("run".into(), run_id().into());
    o.insert("server".into(), opts.server.clone().into());
    o.insert("target".into(), i.target.clone().into());
    o.insert("event".into(), i.event.clone().into());
    o.insert("msg".into(), i.msg.clone().into());
    for (k, v) in &i.ids {
        o.insert(k.clone(), v.clone());
    }
    o.insert("fields".into(), Value::Object(i.fields.clone()));
    let mut s = serde_json::to_string(&Value::Object(o.clone())).unwrap_or_default();
    if s.len() + 1 > MAX_RECORD {
        // REQ-LOG-001: cut the fields, keep the record.
        o.insert("fields".into(), Value::Object(Map::new()));
        o.insert("truncated".into(), true.into());
        if i.msg.len() > 1024 {
            let mut end = 1024;
            while !i.msg.is_char_boundary(end) {
                end -= 1;
            }
            o.insert("msg".into(), i.msg[..end].into());
        }
        s = serde_json::to_string(&Value::Object(o)).unwrap_or_default();
    }
    s.push('\n');
    s
}

fn write_record(i: Inner) {
    // A panic inside the writer must not deadlock the panic hook that logs it.
    let mut guard = match SINK.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::WouldBlock) => SINK.lock().unwrap_or_else(|e| e.into_inner()),
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
    };
    let Some(sink) = guard.as_mut() else {
        return;
    };
    let text = line(&i, &sink.opts);
    sink.reopen_if_moved();
    // One write(2) on an O_APPEND descriptor: processes interleave whole lines.
    let _ = sink.file.write_all(text.as_bytes());
    sink.rotate_if_big();
}

impl Sink {
    /// Another process rotated the file: follow the path to the new one (checked once a second).
    fn reopen_if_moved(&mut self) {
        if self.checked.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.checked = Instant::now();
        let same = fs::metadata(&self.opts.path).is_ok_and(|m| m.ino() == self.ino);
        if !same && let Ok((file, ino)) = open(&self.opts.path) {
            self.file = file;
            self.ino = ino;
        }
    }

    fn rotate_if_big(&mut self) {
        let Ok(meta) = self.file.metadata() else {
            return;
        };
        if meta.len() <= self.opts.max_bytes {
            return;
        }
        // Only the file the path still names is rotated, so two processes crossing the size
        // together do not rotate twice.
        let current = fs::metadata(&self.opts.path).is_ok_and(|m| m.ino() == self.ino);
        if current {
            let _ = fs::rename(&self.opts.path, rotated(&self.opts.path));
        }
        if let Ok((file, ino)) = open(&self.opts.path) {
            self.file = file;
            self.ino = ino;
            self.checked = Instant::now();
        }
    }
}

/// A pane command as REQ-LOG-004 lets it be logged: the program's file name only.
pub fn program(cmd: &str) -> String {
    let first = cmd
        .split_whitespace()
        .find(|w| !w.contains('='))
        .unwrap_or("");
    Path::new(first)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `proc.start` with what every process reports (REQ-LOG-005), plus `extra`.
pub fn start(extra: Value) {
    let cfg = femux_config::config_path();
    let hash = fs::read(&cfg).ok().map(|b| format!("{:016x}", fnv1a(&b)));
    info("proc", "proc.start")
        .msg(format!("femux {} started", env!("CARGO_PKG_VERSION")))
        .field("version", env!("CARGO_PKG_VERSION"))
        .field("os", std::env::consts::OS)
        .field("arch", std::env::consts::ARCH)
        .field(
            "term_program",
            std::env::var("TERM_PROGRAM").unwrap_or_default(),
        )
        .field("term", std::env::var("TERM").unwrap_or_default())
        .field("config", cfg.display().to_string())
        .field("config_hash", hash.map_or(Value::Null, Value::String))
        .field("level", level().map_or("off", Level::as_str))
        .fields(extra)
        .emit();
}

/// `proc.stop` (REQ-LOG-005).
pub fn stop(reason: &str, code: i32) {
    info("proc", "proc.stop")
        .msg(format!("stopped: {reason}"))
        .field("reason", reason)
        .field("code", code)
        .emit();
}

/// A 64-bit FNV-1a hash: the config hash in `proc.start`, not for security.
pub fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Logs every panic as `proc.panic`, then runs the hook that was there before (REQ-LOG-005).
/// A client installs its terminal-restoring hook first, so the terminal is restored before
/// anything else.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("?").to_owned();
        error("proc", "proc.panic")
            .msg(format!("panicked at {loc}: {msg}"))
            .field("message", msg)
            .field("location", loc)
            .field("thread", thread)
            .field(
                "backtrace",
                std::backtrace::Backtrace::force_capture().to_string(),
            )
            .emit();
        prev(info);
    }));
}

/// Sends this process's own stderr into the log as `proc.stderr` lines (REQ-LOG-005), unless
/// stderr is a terminal someone is reading.
pub fn capture_stderr() -> io::Result<bool> {
    // SAFETY: isatty, pipe and dup2 on descriptors this process owns.
    unsafe {
        if libc::isatty(2) == 1 {
            return Ok(false);
        }
        let mut fds = [0; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::dup2(fds[1], 2) < 0 {
            return Err(io::Error::last_os_error());
        }
        libc::close(fds[1]);
        libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
        let read = fds[0];
        std::thread::Builder::new()
            .name("femux-stderr".into())
            .spawn(move || {
                use std::io::BufRead;
                use std::os::fd::FromRawFd;
                let f = File::from_raw_fd(read);
                for l in io::BufReader::new(f).lines() {
                    let Ok(l) = l else { break };
                    warn("proc", "proc.stderr").msg(l).emit();
                }
            })?;
    }
    Ok(true)
}

/// Reads records from the rotated file then the current one, skipping lines that are not JSON.
pub fn read_all(path: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    for p in [rotated(path), path.to_path_buf()] {
        if let Ok(text) = fs::read_to_string(&p) {
            out.extend(
                text.lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok()),
            );
        }
    }
    out
}

static METRICS: Mutex<Option<Metrics>> = Mutex::new(None);

/// Adds `n` to counter `name` for the next `metrics` record; nothing outside debug mode.
pub fn count(name: &str, n: u64) {
    if !debug_mode() {
        return;
    }
    let mut g = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(Metrics::default).add(name, n);
}

/// Records one duration under `name` for the next `metrics` record (REQ-LOG-006).
pub fn time(name: &str, d: Duration) {
    if !debug_mode() {
        return;
    }
    let mut g = METRICS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(Metrics::default).time(name, d);
}

/// Everything counted since the last call, as `{counters, timings}`.
pub fn take_metrics() -> Value {
    METRICS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Metrics::default)
        .take()
}

/// One `metrics` record now (REQ-LOG-006): this process's readings, what was counted since
/// the last one, and `extra`.
pub fn write_metrics(extra: Value) {
    let collected = take_metrics();
    let r = info("proc", "metrics");
    if !r.on() || !debug_mode() {
        return;
    }
    r.msg("metrics")
        .field("rss_bytes", rss_bytes())
        .field("open_fds", open_fds())
        .field("threads", threads())
        .fields(collected)
        .fields(extra)
        .emit();
}

/// Writes a `metrics` record every `every` while debug mode is on, from a thread of its own.
/// `extra` adds the process's own readings (panes, views, …).
pub fn start_metrics(every: Duration, extra: impl Fn() -> Value + Send + 'static) {
    let every = every.max(Duration::from_millis(100));
    let _ = std::thread::Builder::new()
        .name("femux-metrics".into())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                if debug_mode() {
                    write_metrics(extra());
                } else {
                    // Nothing is counted outside debug mode; drop what an earlier one left.
                    let _ = take_metrics();
                }
            }
        });
}

/// Serialises tests that touch the process-wide logger.
#[doc(hidden)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(dir: &Path, level: Level) -> PathBuf {
        let p = dir.join("logs/s.jsonl");
        init(Options {
            path: p.clone(),
            server: "s".into(),
            proc: Proc::Cli,
            level,
            max_bytes: 64 * 1024 * 1024,
        })
        .unwrap();
        p
    }

    #[test]
    fn log_001_one_file_mode_0600_in_a_0700_dir_with_whole_lines() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let p = setup(d.path(), Level::Warn);
        warn("t", "x.y").msg("hello").emit();
        let mode = |p: &Path| fs::metadata(p).unwrap().mode() & 0o777;
        assert_eq!(mode(&p), 0o600);
        assert_eq!(mode(p.parent().unwrap()), 0o700);
        // Threads writing at once still produce whole lines.
        let hs: Vec<_> = (0..8)
            .map(|n| {
                std::thread::spawn(move || {
                    for k in 0..200 {
                        warn("t", "x.y")
                            .field("n", n)
                            .field("k", k)
                            .field("pad", "x".repeat(300))
                            .emit();
                    }
                })
            })
            .collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
        let text = fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().count(), 1601);
        assert!(
            text.lines()
                .all(|l| serde_json::from_str::<Value>(l).is_ok())
        );
        shutdown();
    }

    #[test]
    fn log_001_big_records_are_cut_and_the_file_rotates() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("logs/s.jsonl");
        init(Options {
            path: p.clone(),
            server: "s".into(),
            proc: Proc::Daemon,
            level: Level::Info,
            max_bytes: 4096,
        })
        .unwrap();
        warn("t", "big").field("blob", "x".repeat(40_000)).emit();
        let first = read_all(&p);
        assert_eq!(first[0]["truncated"], true);
        assert_eq!(first[0]["fields"], serde_json::json!({}));
        for k in 0..100 {
            info("t", "fill").field("k", k).emit();
        }
        assert!(rotated(&p).exists());
        assert!(fs::metadata(&p).unwrap().len() <= 4096 + MAX_RECORD as u64);
        // Readers see the rotated file, then the current one, in order.
        let ks: Vec<i64> = read_all(&p)
            .iter()
            .filter(|r| r["event"] == "fill")
            .map(|r| r["fields"]["k"].as_i64().unwrap())
            .collect();
        assert!(ks.windows(2).all(|w| w[0] < w[1]), "{ks:?}");
        assert_eq!(*ks.last().unwrap(), 99);
        shutdown();
    }

    #[test]
    fn log_002_record_shape() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let p = setup(d.path(), Level::Info);
        info("daemon.pty", "pane.spawn")
            .msg("pane spawned")
            .pane("P1")
            .session("s1")
            .field("program", "fish")
            .emit();
        let r = &read_all(&p)[0];
        for k in [
            "ts", "level", "proc", "pid", "run", "target", "event", "msg", "fields",
        ] {
            assert!(r.get(k).is_some(), "{k} missing: {r}");
        }
        assert_eq!(r["level"], "info");
        assert_eq!(r["proc"], "cli");
        assert_eq!(r["pane"], "P1");
        assert_eq!(r["session"], "s1");
        assert_eq!(r["event"], "pane.spawn");
        assert_eq!(r["fields"]["program"], "fish");
        assert_eq!(r["run"].as_str().unwrap().len(), 26);
        let ts = r["ts"].as_str().unwrap();
        assert!(ts.ends_with('Z') && ts.len() == 24, "{ts}");
        shutdown();
    }

    #[test]
    fn log_003_levels_filter_and_switch() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!enabled(Level::Error), "nothing before init");
        let d = tempfile::tempdir().unwrap();
        let p = setup(d.path(), Level::Warn);
        assert!(!debug("t", "d").on());
        debug("t", "dropped").emit();
        info("t", "dropped").emit();
        warn("t", "kept").emit();
        assert_eq!(set_level(Level::Trace), Some(Level::Warn));
        assert!(debug_mode());
        trace("t", "kept").emit();
        let events: Vec<_> = read_all(&p).iter().map(|r| r["event"].clone()).collect();
        assert_eq!(events, vec!["kept", "kept"]);
        assert!(Level::parse("debug").unwrap().is_debug() && !Level::Warn.is_debug());
        shutdown();
    }

    #[test]
    fn log_004_programs_lose_their_arguments_and_assignments() {
        assert_eq!(program("/usr/bin/fish"), "fish");
        assert_eq!(
            program("TOKEN=secret ./deploy --password hunter2"),
            "deploy"
        );
        assert_eq!(program(""), "");
    }

    #[test]
    fn log_006_metrics_records_carry_counters_timings_and_readings() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let p = setup(d.path(), Level::Warn);
        count("frames", 3);
        assert_eq!(
            take_metrics()["counters"],
            serde_json::json!({}),
            "off outside debug mode"
        );
        set_level(Level::Debug);
        count("frames", 3);
        count("frames", 2);
        time("draw", Duration::from_millis(5));
        write_metrics(serde_json::json!({"panes": 4}));
        let r = read_all(&p)
            .into_iter()
            .find(|r| r["event"] == "metrics")
            .unwrap();
        assert_eq!(r["fields"]["counters"]["frames"], 5);
        assert_eq!(r["fields"]["timings"]["draw"]["n"], 1);
        assert_eq!(r["fields"]["panes"], 4);
        if cfg!(target_os = "linux") {
            assert!(r["fields"]["rss_bytes"].as_u64().unwrap() > 0);
        }
        shutdown();
    }

    #[test]
    fn log_005_panics_are_logged() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let p = setup(d.path(), Level::Warn);
        install_panic_hook();
        let _ = std::thread::spawn(|| panic!("boom 42")).join();
        let _ = std::panic::take_hook();
        let r = read_all(&p)
            .into_iter()
            .find(|r| r["event"] == "proc.panic")
            .expect("a proc.panic record");
        assert_eq!(r["level"], "error");
        assert_eq!(r["fields"]["message"], "boom 42");
        assert!(r["fields"]["location"].as_str().unwrap().contains("lib.rs"));
        shutdown();
    }
}
