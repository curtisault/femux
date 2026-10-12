//! `agents` plugin (0.5): agent-state detection (REQ-AGT-001..004) on the daemon side, the
//! `femux agent …` subcommands (SPEC §9.3) and the inbox binding (REQ-UI-044, `prefix i`).

pub mod detect;
pub mod procfs;
pub mod tier;

use std::time::{Duration, Instant};

use femux_plugin::{
    Binding, CliArg, CliCommand, CliCore, CliFlag, CliInput, CliOutput, CliPlugin, ClientPlugin,
    Plugin, Table,
};
use femux_proto::msg as m;
use femux_proto::{Event, Response};
use serde_json::json;

pub use detect::{Agents, Settings};

pub fn plugin() -> Plugin {
    Plugin {
        name: "agents",
        client: Some(Box::new(Client)),
        daemon: Some(Box::new(Agents::new(Settings::load()))),
        cli: Some(Box::new(Cli)),
    }
}

/// The client side alone: the `prefix i` inbox binding.
pub fn client() -> Box<dyn ClientPlugin> {
    Box::new(Client)
}

struct Client;

impl ClientPlugin for Client {
    fn name(&self) -> &'static str {
        "agents"
    }
    fn bindings(&self) -> Vec<Binding> {
        vec![Binding {
            table: Table::Prefix,
            key: "i".into(),
            action: "inbox".into(),
        }]
    }
}

struct Cli;

fn pane_arg(help: &'static str) -> CliArg {
    CliArg {
        name: "pane",
        help,
        required: false,
    }
}

fn err(e: femux_proto::ErrorBody) -> String {
    e.message
}

/// The pane a command means: the argument, else `$FEMUX_PANE`.
fn pane_of(input: &CliInput, core: &dyn CliCore) -> Result<String, String> {
    input
        .args
        .first()
        .cloned()
        .or_else(|| core.pane())
        .ok_or_else(|| "no pane: run inside femux or name one".to_owned())
}

fn info(core: &mut dyn CliCore, pane: &str) -> Result<m::PaneInfoOk, String> {
    match core.request(
        m::PaneInfo {
            pane_id: pane.to_owned(),
        }
        .into(),
    ) {
        Ok(Response::PaneInfo(i)) => Ok(i),
        Ok(other) => Err(format!("unexpected answer {other:?}")),
        Err(e) => Err(err(e)),
    }
}

/// `agent.*` of a pane as JSON, `state` defaulting to `idle`.
fn agent_json(i: &m::PaneInfoOk) -> serde_json::Value {
    let g = |k: &str| i.meta.get(k).cloned();
    json!({
        "pane_id": i.id,
        "session": i.session,
        "tab": i.tab,
        "name": g("agent.name"),
        "state": g("agent.state").unwrap_or_else(|| "idle".into()),
        "source": g("agent.source"),
        "since": g("agent.since").and_then(|s| s.parse::<u64>().ok()),
        "evidence": g("agent.evidence"),
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn ago(since: Option<u64>) -> String {
    let Some(s) = since else {
        return "-".into();
    };
    let secs = now_ms().saturating_sub(s) / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

fn all_panes(core: &mut dyn CliCore) -> Result<Vec<String>, String> {
    let sessions = match core.request(m::SessionList {}.into()).map_err(err)? {
        Response::SessionList(l) => l.sessions,
        other => return Err(format!("unexpected answer {other:?}")),
    };
    let mut out = Vec::new();
    for s in sessions {
        let Response::SessionTabs(t) = core
            .request(m::SessionTabs { name: s.name }.into())
            .map_err(err)?
        else {
            continue;
        };
        for tab in t.tabs {
            collect(&tab.tree, &mut out);
            out.extend(tab.floats.into_iter().map(|f| f.id));
        }
    }
    Ok(out)
}

fn collect(n: &m::LayoutNode, out: &mut Vec<String>) {
    match n {
        m::LayoutNode::PaneNode(p) => out.push(p.id.clone()),
        m::LayoutNode::SplitNode(s) => s.children.iter().for_each(|c| collect(c, out)),
    }
}

/// Waits until the pane's `agent.state` is one of `until`; returns it, or `None` on timeout.
pub fn wait_for(
    core: &mut dyn CliCore,
    pane: &str,
    until: &[&str],
    timeout: Option<Duration>,
) -> Result<Option<String>, String> {
    core.request(
        m::EventsSubscribe {
            kinds: Some(vec!["pane.meta_changed".into(), "pane.exited".into()]),
            pane: Some(pane.to_owned()),
            ..Default::default()
        }
        .into(),
    )
    .map_err(err)?;
    // Subscribe first, then read, so a change in between is not lost.
    let i = info(core, pane)?;
    let state = i
        .meta
        .get("agent.state")
        .cloned()
        .unwrap_or_else(|| "idle".into());
    if until.contains(&state.as_str()) {
        return Ok(Some(state));
    }
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        let left = match deadline {
            Some(d) => match d.checked_duration_since(Instant::now()) {
                Some(l) => Some(l),
                None => return Ok(None),
            },
            None => None,
        };
        match core.next_event(left) {
            Some(Event::PaneMetaChanged(c)) if c.pane_id == pane => {
                if let Some(s) = c.set.get("agent.state")
                    && until.contains(&s.as_str())
                {
                    return Ok(Some(s.clone()));
                }
            }
            Some(Event::PaneExited(e)) if e.pane_id == pane => {
                if until.contains(&"exited") {
                    return Ok(Some("exited".into()));
                }
                return Err(format!("pane {pane} exited"));
            }
            Some(_) => {}
            None if deadline.is_some() => return Ok(None),
            None => return Err("the daemon went away".into()),
        }
    }
}

fn timeout_of(input: &CliInput) -> Result<Option<Duration>, String> {
    input
        .value("timeout")
        .map(|t| {
            t.trim_end_matches('s')
                .parse::<f64>()
                .ok()
                .filter(|s| *s >= 0.0)
                .map(Duration::from_secs_f64)
                .ok_or_else(|| format!("--timeout {t:?} is not a number of seconds"))
        })
        .transpose()
}

fn parse_states(s: &str) -> Result<Vec<&str>, String> {
    let v: Vec<&str> = s.split(',').map(str::trim).collect();
    match v.iter().find(|s| !tier::valid_state(s)) {
        Some(bad) => Err(format!(
            "unknown state {bad:?}; one of {}",
            tier::STATES.join(", ")
        )),
        None => Ok(v),
    }
}

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "agents"
    }

    fn commands(&self) -> Vec<CliCommand> {
        let timeout = CliFlag {
            name: "timeout",
            help: "Give up after this many seconds (exit 1)",
            value: true,
        };
        vec![
            CliCommand {
                name: "agent list",
                about: "List panes with an agent state",
                flags: vec![CliFlag {
                    name: "all",
                    help: "Include idle panes without an agent",
                    value: false,
                }],
                ..Default::default()
            },
            CliCommand {
                name: "agent wait",
                about: "Wait until a pane's agent reaches a state",
                args: vec![pane_arg("Pane id (default: this pane)")],
                flags: vec![
                    CliFlag {
                        name: "until",
                        help: "State or comma-separated states (default: done,blocked,error)",
                        value: true,
                    },
                    timeout.clone(),
                ],
                ..Default::default()
            },
            CliCommand {
                name: "agent prompt",
                about: "Type text plus Enter into a pane's agent",
                args: vec![
                    CliArg {
                        name: "pane",
                        help: "Pane id",
                        required: true,
                    },
                    CliArg {
                        name: "text",
                        help: "What to type",
                        required: true,
                    },
                ],
                flags: vec![
                    CliFlag {
                        name: "wait",
                        help: "Then wait until the agent is done, blocked or errors",
                        value: false,
                    },
                    timeout,
                ],
                ..Default::default()
            },
            CliCommand {
                name: "agent explain",
                about: "Print the evidence behind a pane's agent state",
                args: vec![pane_arg("Pane id (default: this pane)")],
                ..Default::default()
            },
        ]
    }

    fn run(
        &self,
        cmd: &str,
        input: &CliInput,
        core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        match cmd {
            "agent list" => {
                let mut rows = Vec::new();
                for p in all_panes(core)? {
                    let Ok(i) = info(core, &p) else { continue };
                    if input.flag("all") || i.meta.contains_key("agent.state") {
                        rows.push(agent_json(&i));
                    }
                }
                let s = |v: &serde_json::Value| v.as_str().unwrap_or("-").to_owned();
                let text = rows
                    .iter()
                    .map(|r| {
                        format!(
                            "{:<6} {:<10} {:<12} {:<8} {:<9} {}",
                            s(&r["pane_id"]),
                            s(&r["session"]),
                            s(&r["name"]),
                            s(&r["state"]),
                            s(&r["source"]),
                            ago(r["since"].as_u64())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(CliOutput {
                    text,
                    json: json!({ "agents": rows }),
                    attach: None,
                })
            }
            "agent wait" => {
                let pane = pane_of(input, core)?;
                let until = parse_states(input.value("until").unwrap_or("done,blocked,error"))?;
                match wait_for(core, &pane, &until, timeout_of(input)?)? {
                    Some(state) => Ok(CliOutput {
                        text: state.clone(),
                        json: json!({ "pane_id": pane, "state": state }),
                        attach: None,
                    }),
                    None => Err(format!("timed out waiting for {}", until.join("|"))),
                }
            }
            "agent prompt" => {
                let pane = input.args[0].clone();
                let text = input.args[1].clone();
                let enter = m::InputEvent::KeyEvent(m::KeyEvent {
                    key: "enter".into(),
                    mods: vec![],
                    text: None,
                    event: m::KeyEventKind::Press,
                });
                core.request(
                    m::PaneInput {
                        pane_id: pane.clone(),
                        events: vec![m::InputEvent::Paste(m::Paste { text: text.clone() }), enter],
                    }
                    .into(),
                )
                .map_err(err)?;
                if !input.flag("wait") {
                    return Ok(CliOutput {
                        text: String::new(),
                        json: json!({ "pane_id": pane, "text": text }),
                        attach: None,
                    });
                }
                // The agent first has to pick the prompt up: wait for it to leave its
                // current state before waiting for the end.
                let start = info(core, &pane)?.meta.get("agent.state").cloned();
                let timeout = timeout_of(input)?;
                let t0 = Instant::now();
                let mut until = vec!["done", "blocked", "error", "exited"];
                if matches!(start.as_deref(), Some("done" | "blocked" | "error")) {
                    until = vec!["working"];
                    if wait_for(core, &pane, &until, timeout)?.is_none() {
                        return Err("timed out waiting for the agent to start".into());
                    }
                    until = vec!["done", "blocked", "error", "exited"];
                }
                let left = timeout.map(|t| t.saturating_sub(t0.elapsed()));
                match wait_for(core, &pane, &until, left)? {
                    Some(state) => Ok(CliOutput {
                        text: state.clone(),
                        json: json!({ "pane_id": pane, "text": text, "state": state }),
                        attach: None,
                    }),
                    None => Err("timed out waiting for the agent".into()),
                }
            }
            "agent explain" => {
                let pane = pane_of(input, core)?;
                let i = info(core, &pane)?;
                let j = agent_json(&i);
                let text = match i.meta.get("agent.state") {
                    None => format!("{pane}: no agent state (idle)"),
                    Some(state) => format!(
                        "{pane}: {state} ({}, {} ago){}\n{}",
                        j["source"].as_str().unwrap_or("?"),
                        ago(j["since"].as_u64()),
                        j["name"]
                            .as_str()
                            .map_or(String::new(), |n| format!(", agent {n}")),
                        j["evidence"].as_str().unwrap_or("no evidence recorded")
                    ),
                };
                Ok(CliOutput {
                    text,
                    json: j,
                    attach: None,
                })
            }
            other => Err(format!("unknown command {other}")),
        }
    }
}
