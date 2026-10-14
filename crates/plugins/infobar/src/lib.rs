//! `infobar` plugin (0.4): the built-in info bar modules (REQ-UI-032). The bar itself, its
//! position and the order of modules belong to the client (REQ-UI-030/031); this plugin only
//! measures and renders. `session` is drawn by the client (the session chip and mode), so it
//! has no module here.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use femux_plugin::{BarCtx, ClientPlugin, InfoModule, Plugin, Span, Spans, Style};
use femux_proto::msg::NotifyLevel;

pub fn plugin() -> Plugin {
    Plugin {
        name: "infobar",
        client: Some(Box::new(Infobar)),
        daemon: None,
        cli: None,
    }
}

/// Every module name this plugin provides.
pub const MODULES: &[&str] = &[
    "tabs",
    "clock",
    "cpu",
    "mem",
    "net",
    "battery",
    "git",
    "agents",
    "notifications",
    "hostname",
];

struct Infobar;

impl ClientPlugin for Infobar {
    fn name(&self) -> &'static str {
        "infobar"
    }
    fn modules(&self) -> Vec<Box<dyn InfoModule>> {
        vec![
            Box::new(Tabs),
            Box::new(Clock::default()),
            Box::new(Cpu::default()),
            Box::new(Mem::default()),
            Box::new(Net::default()),
            Box::new(Battery::default()),
            Box::new(Git::default()),
            Box::new(Agents),
            Box::new(Notifications),
            Box::new(Hostname::default()),
        ]
    }
}

const SECOND: Duration = Duration::from_secs(1);

fn label(name: &str, ctx: &BarCtx) -> Option<Span> {
    // The vertical bar prints the module name above its value.
    (!ctx.vertical).then(|| Span::new(format!("{name} "), Style::Muted))
}

/// 1536 -> "1.5k", 9_876_543_210 -> "9.2G".
pub fn human(bytes: f64) -> String {
    let units = ["", "k", "M", "G", "T"];
    let mut v = bytes;
    let mut i = 0;
    while v >= 1000.0 && i + 1 < units.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 || v >= 100.0 {
        format!("{v:.0}{}", units[i])
    } else {
        format!("{v:.1}{}", units[i])
    }
}

// ---------------------------------------------------------------- tabs

struct Tabs;

impl InfoModule for Tabs {
    fn name(&self) -> &str {
        "tabs"
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        // A row says `tab 2/5` (the strip already lists the tabs); a column lists them.
        if !ctx.vertical {
            let at = ctx.tabs.iter().position(|t| t.current).map_or(0, |i| i + 1);
            let mut out = vec![Span::new(
                format!("tab {at}/{}", ctx.tabs.len()),
                Style::Muted,
            )];
            if ctx.tabs.iter().any(|t| t.urgent && !t.current) {
                out.push(Span::new(" !", Style::Warn));
            }
            return out;
        }
        let mut out = Vec::new();
        for (i, t) in ctx.tabs.iter().enumerate() {
            if i > 0 {
                out.push(Span::text(if ctx.vertical { "\n" } else { " " }));
            }
            let style = if t.current {
                Style::Key
            } else if t.urgent {
                Style::Warn
            } else {
                Style::Muted
            };
            let text = if ctx.vertical {
                format!("{} {}", t.index, t.title)
            } else {
                t.index.to_string()
            };
            out.push(Span::new(text, style));
        }
        out
    }
    fn interval(&self) -> Duration {
        SECOND
    }
}

// ---------------------------------------------------------------- clock

#[derive(Default)]
struct Clock {
    text: String,
}

/// Local time as `Sat 23:41`.
pub fn local_time() -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    // SAFETY: time and localtime_r only write the provided out-parameters.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        format!(
            "{} {:02}:{:02}",
            DAYS[tm.tm_wday.clamp(0, 6) as usize],
            tm.tm_hour,
            tm.tm_min
        )
    }
}

impl InfoModule for Clock {
    fn name(&self) -> &str {
        "clock"
    }
    fn sample(&mut self, _ctx: &BarCtx) {
        self.text = local_time();
    }
    fn render(&self, _ctx: &BarCtx, _w: usize) -> Spans {
        if self.text.is_empty() {
            return vec![Span::text(local_time())];
        }
        vec![Span::text(self.text.clone())]
    }
    fn interval(&self) -> Duration {
        SECOND
    }
}

// ---------------------------------------------------------------- cpu / mem

#[derive(Default)]
struct Cpu {
    sys: Option<sysinfo::System>,
    history: VecDeque<f32>,
}

/// Bars for a sparkline of percentages.
pub fn sparkline(values: impl IntoIterator<Item = f32>) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    values
        .into_iter()
        .map(|v| BARS[((v.clamp(0.0, 100.0) / 100.0) * 7.0).round() as usize])
        .collect()
}

impl InfoModule for Cpu {
    fn name(&self) -> &str {
        "cpu"
    }
    fn sample(&mut self, _ctx: &BarCtx) {
        let sys = self.sys.get_or_insert_with(sysinfo::System::new);
        sys.refresh_cpu_usage();
        self.history.push_back(sys.global_cpu_usage());
        while self.history.len() > 8 {
            self.history.pop_front();
        }
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        let Some(now) = self.history.back() else {
            return Vec::new();
        };
        let mut out: Spans = label("cpu", ctx).into_iter().collect();
        out.push(Span::text(format!("{now:.0}%")));
        if ctx.vertical {
            out.push(Span::new(
                format!(" {}", sparkline(self.history.iter().copied())),
                Style::Muted,
            ));
        }
        out
    }
    fn interval(&self) -> Duration {
        SECOND
    }
}

#[derive(Default)]
struct Mem {
    sys: Option<sysinfo::System>,
    used_total: Option<(u64, u64)>,
}

impl InfoModule for Mem {
    fn name(&self) -> &str {
        "mem"
    }
    fn sample(&mut self, _ctx: &BarCtx) {
        let sys = self.sys.get_or_insert_with(sysinfo::System::new);
        sys.refresh_memory();
        self.used_total = Some((sys.used_memory(), sys.total_memory()));
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        let Some((used, total)) = self.used_total.filter(|(_, t)| *t > 0) else {
            return Vec::new();
        };
        let mut out: Spans = label("mem", ctx).into_iter().collect();
        out.push(Span::text(if ctx.vertical {
            format!("{} / {}", human(used as f64), human(total as f64))
        } else {
            human(used as f64)
        }));
        out
    }
    fn interval(&self) -> Duration {
        SECOND
    }
}

// ---------------------------------------------------------------- net

#[derive(Default)]
struct Net {
    nets: Option<sysinfo::Networks>,
    last: Option<Instant>,
    /// Bytes per second down and up.
    rate: Option<(f64, f64)>,
}

impl InfoModule for Net {
    fn name(&self) -> &str {
        "net"
    }
    fn sample(&mut self, _ctx: &BarCtx) {
        let now = Instant::now();
        let fresh = self.nets.is_none();
        let nets = self
            .nets
            .get_or_insert_with(sysinfo::Networks::new_with_refreshed_list);
        if !fresh {
            nets.refresh(true);
        }
        let (rx, tx) = nets
            .iter()
            .filter(|(name, _)| !name.starts_with("lo"))
            .fold((0u64, 0u64), |(r, t), (_, d)| {
                (r + d.received(), t + d.transmitted())
            });
        if let Some(last) = self.last.filter(|_| !fresh) {
            let secs = now.duration_since(last).as_secs_f64().max(0.001);
            self.rate = Some((rx as f64 / secs, tx as f64 / secs));
        }
        self.last = Some(now);
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        let Some((down, up)) = self.rate else {
            return Vec::new();
        };
        let mut out: Spans = label("net", ctx).into_iter().collect();
        let sep = if ctx.vertical { "\n" } else { " " };
        out.push(Span::text(format!("↓{}{sep}↑{}", human(down), human(up))));
        out
    }
    fn interval(&self) -> Duration {
        SECOND
    }
}

// ---------------------------------------------------------------- battery

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Charge {
    pub percent: u8,
    pub charging: bool,
}

/// Linux: the first `type == Battery` under /sys/class/power_supply.
pub fn read_sys_battery(root: &std::path::Path) -> Option<Charge> {
    let mut entries: Vec<_> = std::fs::read_dir(root).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let percent = std::fs::read_to_string(p.join("capacity"))
            .ok()?
            .trim()
            .parse::<u8>()
            .ok()?;
        let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
        return Some(Charge {
            percent: percent.min(100),
            charging: matches!(status.trim(), "Charging" | "Full"),
        });
    }
    None
}

/// macOS: `pmset -g batt` output, e.g. `-InternalBattery-0 (id=1)\t87%; charging; ...`.
pub fn parse_pmset(out: &str) -> Option<Charge> {
    let line = out.lines().find(|l| l.contains("InternalBattery"))?;
    let pct = line.split('\t').nth(1).unwrap_or(line);
    let percent = pct.split('%').next()?.trim().parse::<u8>().ok()?;
    let charging =
        pct.contains("; charging") || pct.contains("charged") || pct.contains("AC attached");
    Some(Charge { percent, charging })
}

#[derive(Default)]
struct Battery {
    charge: Option<Charge>,
    pending: Option<mpsc::Receiver<Option<Charge>>>,
}

impl InfoModule for Battery {
    fn name(&self) -> &str {
        "battery"
    }
    fn sample(&mut self, _ctx: &BarCtx) {
        if cfg!(target_os = "macos") {
            // pmset is a process: run it off the client's thread, pick the answer up next time.
            if let Some(rx) = &self.pending
                && let Ok(c) = rx.try_recv()
            {
                self.charge = c;
                self.pending = None;
            }
            if self.pending.is_none() {
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let out = std::process::Command::new("pmset")
                        .args(["-g", "batt"])
                        .output()
                        .ok()
                        .and_then(|o| parse_pmset(&String::from_utf8_lossy(&o.stdout)));
                    let _ = tx.send(out);
                });
                self.pending = Some(rx);
            }
        } else {
            self.charge = read_sys_battery(std::path::Path::new("/sys/class/power_supply"));
        }
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        let Some(c) = self.charge else {
            return Vec::new();
        };
        let mut out: Spans = label("bat", ctx).into_iter().collect();
        let style = if c.percent <= 15 && !c.charging {
            Style::Warn
        } else {
            Style::Text
        };
        out.push(Span::new(
            format!("{}%{}", c.percent, if c.charging { "+" } else { "" }),
            style,
        ));
        out
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(30)
    }
}

// ---------------------------------------------------------------- git

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitState {
    pub branch: String,
    pub dirty: usize,
    pub ahead: usize,
    pub behind: usize,
}

/// `git status --porcelain=v1 --branch` output.
pub fn parse_git_status(out: &str) -> Option<GitState> {
    let mut lines = out.lines();
    let head = lines.next()?.strip_prefix("## ")?;
    let (names, track) = match head.split_once(" [") {
        Some((n, t)) => (n, t.trim_end_matches(']')),
        None => (head, ""),
    };
    let branch = names
        .split("...")
        .next()
        .unwrap_or(names)
        .trim_start_matches("No commits yet on ")
        .to_owned();
    let count = |key: &str| -> usize {
        track
            .split(", ")
            .find_map(|p| p.strip_prefix(key))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    };
    Some(GitState {
        branch,
        dirty: lines.filter(|l| !l.is_empty()).count(),
        ahead: count("ahead "),
        behind: count("behind "),
    })
}

#[derive(Default)]
struct Git {
    /// The cwd the state is for, and the state (`None`: not a repository).
    state: Option<(String, Option<GitState>)>,
    pending: Option<(String, mpsc::Receiver<Option<GitState>>)>,
}

impl InfoModule for Git {
    fn name(&self) -> &str {
        "git"
    }
    fn sample(&mut self, ctx: &BarCtx) {
        if let Some((cwd, rx)) = &self.pending
            && let Ok(s) = rx.try_recv()
        {
            self.state = Some((cwd.clone(), s));
            self.pending = None;
        }
        let Some(cwd) = ctx.cwd.clone() else {
            self.state = None;
            return;
        };
        if self.pending.is_some() {
            return;
        }
        // git can be slow in a big repository: never on the client's thread.
        let (tx, rx) = mpsc::channel();
        let dir = cwd.clone();
        std::thread::spawn(move || {
            let out = std::process::Command::new("git")
                .args(["-C", &dir, "status", "--porcelain=v1", "--branch"])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| parse_git_status(&String::from_utf8_lossy(&o.stdout)));
            let _ = tx.send(out);
        });
        self.pending = Some((cwd, rx));
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        // Only for the directory the focused pane is in now.
        let Some((cwd, Some(g))) = &self.state else {
            return Vec::new();
        };
        if ctx.cwd.as_deref() != Some(cwd.as_str()) {
            return Vec::new();
        }
        let mut out = vec![Span::text(g.branch.clone())];
        if g.dirty > 0 {
            out.push(Span::new(format!(" +{}", g.dirty), Style::Warn));
        }
        if g.ahead > 0 {
            out.push(Span::new(format!(" ↑{}", g.ahead), Style::Muted));
        }
        if g.behind > 0 {
            out.push(Span::new(format!(" ↓{}", g.behind), Style::Muted));
        }
        out
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(2)
    }
}

// ---------------------------------------------------------------- agents

/// Agent states in display order, with their glyph and style (spec/design/README.md).
const AGENT_STATES: [(&str, &str, Style); 4] = [
    ("working", "◐", Style::Accent),
    ("blocked", "■", Style::Warn),
    ("done", "✓", Style::Ok),
    ("error", "✗", Style::Error),
];

struct Agents;

impl InfoModule for Agents {
    fn name(&self) -> &str {
        "agents"
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        let mut out = Vec::new();
        for (state, glyph, style) in AGENT_STATES {
            let n = ctx.agents.get(state).copied().unwrap_or(0);
            if n == 0 {
                continue;
            }
            if !out.is_empty() {
                out.push(Span::text(if ctx.vertical { "\n" } else { " " }));
            }
            out.push(Span::new(format!("{glyph} "), style));
            out.push(Span::text(if ctx.vertical {
                format!("{n} {state}")
            } else {
                n.to_string()
            }));
        }
        out
    }
    fn interval(&self) -> Duration {
        SECOND
    }
}

// ---------------------------------------------------------------- notifications

struct Notifications;

fn level_style(l: NotifyLevel) -> Style {
    match l {
        NotifyLevel::Info => Style::Text,
        NotifyLevel::Done => Style::Ok,
        NotifyLevel::Warn => Style::Warn,
        NotifyLevel::Error => Style::Error,
    }
}

impl InfoModule for Notifications {
    fn name(&self) -> &str {
        "notifications"
    }
    /// REQ-UI-033: the client passes only notifications whose pane is hidden, newest first.
    fn render(&self, ctx: &BarCtx, w: usize) -> Spans {
        let n = if ctx.vertical { 3 } else { 1 };
        let mut out = Vec::new();
        for (i, note) in ctx.notifications.iter().take(n).enumerate() {
            if i > 0 {
                out.push(Span::text("\n"));
            }
            let title: String = note.title.chars().take(w.max(1)).collect();
            out.push(Span::new(title, level_style(note.level)));
        }
        out
    }
    fn interval(&self) -> Duration {
        SECOND
    }
}

// ---------------------------------------------------------------- hostname

#[derive(Default)]
struct Hostname {
    name: String,
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for its length; the result is NUL-terminated on success.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return String::new();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end])
        .split('.')
        .next()
        .unwrap_or_default()
        .to_owned()
}

impl InfoModule for Hostname {
    fn name(&self) -> &str {
        "hostname"
    }
    fn sample(&mut self, _ctx: &BarCtx) {
        self.name = hostname();
    }
    fn render(&self, _ctx: &BarCtx, _w: usize) -> Spans {
        if self.name.is_empty() {
            return Vec::new();
        }
        vec![Span::text(self.name.clone())]
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use femux_plugin::{NoteItem, TabItem};

    fn text(s: &Spans) -> String {
        s.iter().map(|s| s.text.as_str()).collect()
    }

    fn module(name: &str) -> Box<dyn InfoModule> {
        Infobar
            .modules()
            .into_iter()
            .find(|m| m.name() == name)
            .unwrap()
    }

    #[test]
    fn ui_032_every_built_in_module_exists() {
        let names: Vec<String> = Infobar
            .modules()
            .iter()
            .map(|m| m.name().to_owned())
            .collect();
        assert_eq!(names, MODULES);
    }

    #[test]
    fn system_modules_measure_this_machine() {
        let ctx = BarCtx::default();
        for name in ["cpu", "mem", "net"] {
            let mut m = module(name);
            m.sample(&ctx);
            std::thread::sleep(Duration::from_millis(250));
            m.sample(&ctx);
            let out = text(&m.render(&ctx, 40));
            assert!(out.starts_with(name), "{name}: {out:?}");
        }
        let mut h = module("hostname");
        h.sample(&ctx);
        assert_eq!(text(&h.render(&ctx, 40)), hostname());
        assert!(!hostname().is_empty());
        let mut c = module("clock");
        c.sample(&ctx);
        let t = text(&c.render(&ctx, 40));
        assert!(t.len() == 9 && t.as_bytes()[6] == b':', "{t:?}");
        // Vertical: no label (the bar prints the name), several lines where useful.
        let v = BarCtx {
            vertical: true,
            ..Default::default()
        };
        let mut n = module("net");
        n.sample(&v);
        std::thread::sleep(Duration::from_millis(100));
        n.sample(&v);
        assert!(text(&n.render(&v, 20)).contains("\n↑"));
    }

    #[test]
    fn values_format_compactly() {
        assert_eq!(human(512.0), "512");
        assert_eq!(human(1536.0), "1.5k");
        assert_eq!(human(9.2 * 1024.0 * 1024.0 * 1024.0), "9.2G");
        assert_eq!(human(150.0 * 1024.0 * 1024.0), "150M");
        assert_eq!(sparkline([0.0, 50.0, 100.0]), "▁▅█");
    }

    #[test]
    fn battery_from_sysfs_and_pmset() {
        let dir = tempfile::tempdir().unwrap();
        let ac = dir.path().join("AC");
        std::fs::create_dir_all(&ac).unwrap();
        std::fs::write(ac.join("type"), "Mains\n").unwrap();
        assert_eq!(read_sys_battery(dir.path()), None);
        let bat = dir.path().join("BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("capacity"), "87\n").unwrap();
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();
        assert_eq!(
            read_sys_battery(dir.path()),
            Some(Charge {
                percent: 87,
                charging: false
            })
        );
        let out = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t64%; charging; 1:12 remaining present: true\n";
        assert_eq!(
            parse_pmset(out),
            Some(Charge {
                percent: 64,
                charging: true
            })
        );
        assert_eq!(parse_pmset("Now drawing from 'AC Power'\n"), None);
        let b = Battery {
            charge: Some(Charge {
                percent: 9,
                charging: false,
            }),
            pending: None,
        };
        let out = b.render(&BarCtx::default(), 20);
        assert_eq!(text(&out), "bat 9%");
        assert_eq!(out[1].style, Style::Warn);
    }

    #[test]
    fn git_status_and_a_real_repository() {
        assert_eq!(
            parse_git_status("## main...origin/main [ahead 2, behind 1]\n M a\n?? b\n"),
            Some(GitState {
                branch: "main".into(),
                dirty: 2,
                ahead: 2,
                behind: 1
            })
        );
        assert_eq!(
            parse_git_status("## No commits yet on trunk\n")
                .unwrap()
                .branch,
            "trunk"
        );
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
        };
        if !git(&["init", "-q", "-b", "work"]).status.success() {
            return; // no git here
        }
        std::fs::write(dir.path().join("f"), "x").unwrap();
        let ctx = BarCtx {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut m = module("git");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            m.sample(&ctx);
            if text(&m.render(&ctx, 40)) == "work +1" {
                break;
            }
            assert!(Instant::now() < deadline, "{:?}", text(&m.render(&ctx, 40)));
            std::thread::sleep(Duration::from_millis(50));
        }
        // Another directory shows nothing until sampled there.
        let elsewhere = BarCtx {
            cwd: Some("/".into()),
            ..Default::default()
        };
        assert!(m.render(&elsewhere, 40).is_empty());
    }

    #[test]
    fn tabs_agents_and_notifications_render_from_the_context() {
        let mut ctx = BarCtx {
            tabs: vec![
                TabItem {
                    index: 1,
                    title: "shell".into(),
                    current: false,
                    urgent: false,
                },
                TabItem {
                    index: 2,
                    title: "api".into(),
                    current: true,
                    urgent: false,
                },
                TabItem {
                    index: 3,
                    title: "infra".into(),
                    current: false,
                    urgent: true,
                },
            ],
            agents: [("working", 2), ("blocked", 1), ("idle", 4), ("done", 1)]
                .map(|(k, v)| (k.to_owned(), v))
                .into(),
            notifications: vec![
                NoteItem {
                    title: "codex needs y/n".into(),
                    level: NotifyLevel::Warn,
                },
                NoteItem {
                    title: "opencode done".into(),
                    level: NotifyLevel::Done,
                },
            ],
            ..Default::default()
        };
        // A row counts (the strip lists them) and flags an urgent tab elsewhere.
        let tabs = module("tabs").render(&ctx, 40);
        assert_eq!(text(&tabs), "tab 2/3 !");
        assert_eq!(tabs[1].style, Style::Warn);
        assert_eq!(text(&module("agents").render(&ctx, 40)), "◐ 2 ■ 1 ✓ 1");
        let notes = module("notifications").render(&ctx, 40);
        assert_eq!(text(&notes), "codex needs y/n");
        assert_eq!(notes[0].style, Style::Warn);
        ctx.vertical = true;
        assert_eq!(
            text(&module("tabs").render(&ctx, 40)),
            "1 shell\n2 api\n3 infra"
        );
        assert_eq!(
            text(&module("agents").render(&ctx, 40)),
            "◐ 2 working\n■ 1 blocked\n✓ 1 done"
        );
        assert_eq!(
            text(&module("notifications").render(&ctx, 40)),
            "codex needs y/n\nopencode done"
        );
        ctx.notifications.clear();
        ctx.agents.clear();
        assert!(module("notifications").render(&ctx, 40).is_empty());
        assert!(module("agents").render(&ctx, 40).is_empty());
    }
}
