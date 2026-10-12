//! The agents plugin's daemon side: tiered agent-state detection (REQ-AGT-001..004) written
//! to pane metadata as `agent.state`, `agent.source`, `agent.since`, `agent.name` and, for
//! `femux agent explain`, `agent.evidence`. It talks to the daemon only through the protocol
//! (REQ-PLUG-003) and runs wherever the daemon does (tier 2 reads that machine's `/proc`).

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use femux_plugin::DaemonPlugin;
use femux_proto::msg as m;
use femux_proto::{Event, Request, Response};

use crate::procfs;
use crate::tier::{Source, Tracker, valid_state};

/// Commands treated as agents for tiers 2 and 3 (their `comm`, at most 15 characters).
pub const KNOWN_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "cursor-agent",
    "aider",
    "gemini",
    "goose",
    "amp",
    "crush",
    "qwen",
];

/// After `exited`, how long until `idle` (REQ-AGT-002: "exited then idle").
const EXITED_FOR_MS: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub blocked_after_ms: u64,
    pub hold_ms: u64,
    /// Tier 2 (Linux only, REQ-AGT-002 note).
    pub process_tier: bool,
    pub tick: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::from_config(&femux_config::Config::default())
    }
}

impl Settings {
    pub fn from_config(c: &femux_config::Config) -> Settings {
        Settings {
            enabled: c.agents.enabled,
            blocked_after_ms: c.agents.blocked_after_ms,
            hold_ms: c.agents.hold_ms,
            process_tier: cfg!(target_os = "linux"),
            tick: Duration::from_millis(250),
        }
    }

    /// From `config.toml`; the defaults when it cannot be read.
    pub fn load() -> Settings {
        femux_config::Config::load(&femux_config::config_path())
            .map(|c| Settings::from_config(&c))
            .unwrap_or_default()
    }
}

#[derive(Debug, Default)]
struct Watch {
    shell_pid: u32,
    fg_pid: u32,
    /// The agent in the pane, from its foreground command or a hook.
    name: Option<String>,
    tracker: Tracker,
    /// Tier 1 or 2 has said something: tier 3 stays quiet (REQ-AGT-003).
    tier12: bool,
    last_output: u64,
    /// Output seen since tier 3 last judged a silence.
    burst: bool,
    exited_at: Option<u64>,
    /// What this plugin last wrote, to recognise its own `pane.meta_changed`.
    written: Option<(String, &'static str)>,
    evidence: String,
    asking: bool,
}

pub fn is_agent(cmd: &str) -> bool {
    KNOWN_AGENTS.contains(&cmd)
}

/// Whether a screen line reads like the agent waits for an answer (REQ-AGT-003).
pub fn prompt_like(line: &str) -> bool {
    let l = line.trim_end();
    let low = l.to_lowercase();
    l.ends_with('?')
        || l.ends_with(':')
        || l == ">"
        || ["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "press enter"]
            .iter()
            .any(|p| low.contains(p))
}

pub struct Agents {
    pub settings: Settings,
    panes: HashMap<String, Watch>,
    now: u64,
}

impl Agents {
    pub fn new(settings: Settings) -> Agents {
        Agents {
            settings,
            panes: HashMap::new(),
            now: 0,
        }
    }

    fn now(&self) -> u64 {
        if self.now > 0 {
            self.now
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64)
        }
    }

    fn info(pane: &str) -> Request {
        m::PaneInfo {
            pane_id: pane.to_owned(),
        }
        .into()
    }

    /// Offers a state; returns the metadata write when it changes what the pane shows.
    fn propose(
        &mut self,
        pane: &str,
        state: &str,
        source: Source,
        evidence: String,
    ) -> Vec<Request> {
        let (now, hold) = (self.now(), self.settings.hold_ms);
        let Some(w) = self.panes.get_mut(pane) else {
            return Vec::new();
        };
        if source.tier() < 3 {
            w.tier12 = true;
        }
        if !w.tracker.propose(state, source, now, hold) {
            return Vec::new();
        }
        w.evidence = evidence;
        let j = w.tracker.current.clone().unwrap();
        w.written = Some((j.state.clone(), source.as_str()));
        let mut set: BTreeMap<String, String> = [
            ("agent.state", j.state.clone()),
            ("agent.source", source.as_str().to_owned()),
            ("agent.since", j.since.to_string()),
            ("agent.evidence", w.evidence.clone()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        if let Some(n) = &w.name {
            set.insert("agent.name".into(), n.clone());
        }
        vec![
            m::PaneSetMeta {
                pane_id: pane.to_owned(),
                set: Some(set),
                unset: None,
            }
            .into(),
        ]
    }

    /// Tier 2 (REQ-AGT-002): the foreground process's state and how long the pane was quiet.
    fn process_tier(&mut self, pane: &str) -> Vec<Request> {
        let now = self.now();
        let after = self.settings.blocked_after_ms;
        let Some(w) = self.panes.get(pane) else {
            return Vec::new();
        };
        let fg = w.fg_pid;
        let Some(p) = procfs::read(fg) else {
            return Vec::new();
        };
        let quiet = now.saturating_sub(w.last_output);
        let name = w.name.clone().unwrap_or_default();
        let (state, why) = if procfs::waiting_for_input(&p) && quiet >= after {
            ("blocked", "waits for input")
        } else if p.state == 'R' || quiet < after {
            ("working", "is running")
        } else {
            ("working", "is busy")
        };
        let evidence = format!(
            "process: {name} (pid {fg}) {why}: state {} in {}, no output for {:.1}s",
            p.state,
            if p.wchan.is_empty() { "?" } else { &p.wchan },
            quiet as f64 / 1000.0
        );
        self.propose(pane, state, Source::Process, evidence)
    }

    /// Tier 3 (REQ-AGT-003): output moving means working; a silence after a burst reads the
    /// screen's last line.
    fn heuristic_tier(&mut self, pane: &str) -> Vec<Request> {
        let now = self.now();
        let after = self.settings.blocked_after_ms;
        let Some(w) = self.panes.get_mut(pane) else {
            return Vec::new();
        };
        if w.tier12 {
            return Vec::new();
        }
        let quiet = now.saturating_sub(w.last_output);
        if quiet < after {
            w.burst = true;
            let e = format!("heuristic: output {:.1}s ago", quiet as f64 / 1000.0);
            return self.propose(pane, "working", Source::Heuristic, e);
        }
        if !w.burst {
            return Vec::new();
        }
        w.burst = false;
        vec![
            m::ScrollbackRead {
                pane_id: pane.to_owned(),
                from_line: 0,
                count: 500,
            }
            .into(),
        ]
    }

    fn on_screen(&mut self, pane: &str, lines: &[Vec<m::Cell>]) -> Vec<Request> {
        let quiet = self
            .panes
            .get(pane)
            .map_or(0, |w| self.now().saturating_sub(w.last_output));
        let last = lines
            .iter()
            .map(|l| {
                l.iter()
                    .map(|c| if c.ch.is_empty() { " " } else { c.ch.as_str() })
                    .collect::<String>()
            })
            .map(|l| l.trim_end().to_owned())
            .rfind(|l| !l.is_empty())
            .unwrap_or_default();
        let state = if prompt_like(&last) {
            "blocked"
        } else {
            "done"
        };
        let evidence = format!(
            "heuristic: silent for {:.1}s after output; last line {last:?}",
            quiet as f64 / 1000.0
        );
        self.propose(pane, state, Source::Heuristic, evidence)
    }

    fn on_info(&mut self, i: &m::PaneInfoOk) -> Vec<Request> {
        let w = self.panes.entry(i.id.clone()).or_default();
        w.asking = false;
        w.shell_pid = i.pid;
        w.fg_pid = i.fg_pid;
        w.last_output = i.last_output;
        if let Some(n) = i.meta.get("agent.name") {
            w.name = Some(n.clone());
        } else if is_agent(&i.fg_cmd) {
            w.name = Some(i.fg_cmd.clone());
        }
        if w.name.is_none() || w.fg_pid == 0 || w.fg_pid == w.shell_pid {
            return Vec::new();
        }
        if self.settings.process_tier {
            self.process_tier(&i.id)
        } else {
            self.heuristic_tier(&i.id)
        }
    }

    /// The pane's process changed (REQ-AGT-002): an agent started, or the shell is back.
    fn on_fg(&mut self, f: &m::PaneFgChanged) -> Vec<Request> {
        let now = self.now();
        let process = self.settings.process_tier;
        let w = self.panes.entry(f.pane_id.clone()).or_default();
        w.fg_pid = f.pid;
        if w.shell_pid != 0 && f.pid == w.shell_pid {
            if w.name.is_some() && w.exited_at.is_none() && process {
                w.exited_at = Some(now);
                let e = format!(
                    "process: the shell (pid {}) is in the foreground again",
                    f.pid
                );
                return self.propose(&f.pane_id, "exited", Source::Process, e);
            }
            return Vec::new();
        }
        if is_agent(&f.cmd) {
            w.name = Some(f.cmd.clone());
            w.exited_at = None;
            if process {
                let e = format!("process: {} (pid {}) started", f.cmd, f.pid);
                return self.propose(&f.pane_id, "working", Source::Process, e);
            }
        }
        vec![Agents::info(&f.pane_id)]
    }

    /// Tier 1 from hooks (`femux pane set agent.state=…`, `femux integration hook`).
    fn on_meta(&mut self, c: &m::PaneMetaChanged) -> Vec<Request> {
        let Some(state) = c.set.get("agent.state") else {
            return Vec::new();
        };
        let source = c
            .set
            .get("agent.source")
            .and_then(|s| Source::parse(s))
            .unwrap_or(Source::Hook);
        let w = self.panes.entry(c.pane_id.clone()).or_default();
        if w.written.as_ref() == Some(&(state.clone(), source.as_str())) {
            return Vec::new(); // our own write coming back
        }
        if let Some(n) = c.set.get("agent.name") {
            w.name = Some(n.clone());
        }
        if !valid_state(state) {
            return Vec::new();
        }
        let who = c
            .set
            .get("agent.name")
            .map_or(String::new(), |n| format!(" by {n}"));
        self.propose(
            &c.pane_id,
            state,
            source,
            format!("{}: set{who}", source.as_str()),
        )
    }
}

impl DaemonPlugin for Agents {
    fn name(&self) -> &'static str {
        "agents"
    }

    fn start(&mut self) -> Vec<Request> {
        if !self.settings.enabled {
            return Vec::new();
        }
        vec![m::SessionList {}.into()]
    }

    fn on_event(&mut self, ev: &Event) -> Vec<Request> {
        if let Event::ConfigReloaded(_) = ev {
            let process = self.settings.process_tier;
            self.settings = Settings {
                process_tier: process,
                tick: self.settings.tick,
                ..Settings::load()
            };
        }
        if !self.settings.enabled {
            return Vec::new();
        }
        match ev {
            Event::PaneSpawned(p) => {
                self.panes.insert(p.pane_id.clone(), Watch::default());
                vec![Agents::info(&p.pane_id)]
            }
            Event::PaneExited(p) => {
                self.panes.remove(&p.pane_id);
                Vec::new()
            }
            Event::PaneFgChanged(f) => self.on_fg(f),
            Event::PaneMetaChanged(c) => self.on_meta(c),
            // REQ-AGT-001: OSC 9;4 0 → idle, 1–3 → working.
            Event::PaneProgress(p) => {
                self.panes.entry(p.pane_id.clone()).or_default();
                let state = match p.state {
                    0 => "idle",
                    1..=3 => "working",
                    _ => return Vec::new(),
                };
                let e = format!("osc: OSC 9;4 state {}", p.state);
                self.propose(&p.pane_id, state, Source::Osc, e)
            }
            // REQ-AGT-001: OSC 7777;state=<s>.
            Event::PaneReport(r) => {
                self.panes.entry(r.pane_id.clone()).or_default();
                match r.data.strip_prefix("state=") {
                    Some(s) if valid_state(s) => {
                        let e = format!("osc: OSC 7777 {}", r.data);
                        self.propose(&r.pane_id, s, Source::Osc, e)
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn tick_every(&self) -> Option<Duration> {
        Some(self.settings.tick)
    }

    fn tick(&mut self, now_ms: u64) -> Vec<Request> {
        if !self.settings.enabled {
            return Vec::new();
        }
        self.now = now_ms;
        let mut out = Vec::new();
        let ids: Vec<String> = self.panes.keys().cloned().collect();
        for id in ids {
            let w = self.panes.get_mut(&id).unwrap();
            if let Some(at) = w.exited_at
                && now_ms.saturating_sub(at) >= EXITED_FOR_MS
            {
                w.exited_at = None;
                w.name = None;
                let e = "process: the agent exited".to_owned();
                out.extend(self.propose(&id, "idle", Source::Process, e));
                continue;
            }
            if w.name.is_some() && !w.asking {
                w.asking = true;
                out.push(Agents::info(&id));
            }
        }
        self.now = 0;
        out
    }

    fn on_response(&mut self, req: &Request, resp: &Response) -> Vec<Request> {
        match (req, resp) {
            (_, Response::SessionList(l)) => l
                .sessions
                .iter()
                .map(|s| {
                    m::SessionTabs {
                        name: s.name.clone(),
                    }
                    .into()
                })
                .collect(),
            (_, Response::SessionTabs(t)) => {
                let mut out = Vec::new();
                for tab in &t.tabs {
                    let mut ids = Vec::new();
                    collect(&tab.tree, &mut ids);
                    ids.extend(tab.floats.iter().map(|f| f.id.clone()));
                    for id in ids {
                        self.panes.entry(id.clone()).or_default();
                        out.push(Agents::info(&id));
                    }
                }
                out
            }
            (_, Response::PaneInfo(i)) => self.on_info(i),
            (Request::ScrollbackRead(r), Response::ScrollbackRead(s)) => {
                self.on_screen(&r.pane_id, &s.lines)
            }
            _ => Vec::new(),
        }
    }
}

fn collect(n: &m::LayoutNode, out: &mut Vec<String>) {
    match n {
        m::LayoutNode::PaneNode(p) => out.push(p.id.clone()),
        m::LayoutNode::SplitNode(s) => s.children.iter().for_each(|c| collect(c, out)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_look_like_questions() {
        for l in [
            "Proceed? ",
            "Run docker compose up -d db? (y/n)",
            "Allow? [y/N]",
            "Enter a region:",
            ">",
        ] {
            assert!(prompt_like(l), "{l}");
        }
        for l in ["Updated docs/api/pagination.md", "131 passed, 0 failed", ""] {
            assert!(!prompt_like(l), "{l}");
        }
        assert!(is_agent("claude") && !is_agent("vim"));
    }
}
