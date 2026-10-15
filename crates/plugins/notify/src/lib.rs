//! `notify` plugin (0.5): `[notify]` actions for agent and pane events (SPEC §9.4).
//!
//! - Daemon side: turns `agent.state` changes to `done|blocked|error` and pane exits into
//!   `notify.post` with the `[notify]` entry name as `event`, and runs `webhook` actions
//!   (REQ-NOTIFY-001) where the daemon runs.
//! - Client side: on `notify.posted`, runs `sound` and `desktop` on the client's machine
//!   (REQ-NOTIFY-003: a remote daemon's notifications arrive as events), skipping
//!   `only_when_hidden` entries whose pane some view shows (REQ-NOTIFY-002).
//! - `femux notify test [event]` posts a notification by hand.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use femux_config::{Config, NotifyAction};
use femux_plugin::log;
use femux_plugin::{
    CliArg, CliCommand, CliCore, CliInput, CliOutput, CliPlugin, ClientPlugin, DaemonPlugin,
    HostEffect, Plugin,
};
use femux_proto::msg as m;
use femux_proto::{Event, Request, Response};
use serde_json::json;

/// The `[notify]` entries this plugin knows how to raise.
pub const EVENTS: &[&str] = &["agent_done", "agent_blocked", "agent_error", "pane_exited"];

pub type Actions = BTreeMap<String, NotifyAction>;

/// The `[notify]` table of a config text.
pub fn parse(toml: &str) -> Result<Actions, String> {
    Config::parse(toml).map(|c| c.notify)
}

fn load() -> Actions {
    Config::load(&femux_config::config_path())
        .map(|c| c.notify)
        .unwrap_or_else(|_| Config::default().notify)
}

pub fn plugin() -> Plugin {
    let actions = load();
    Plugin {
        name: "notify",
        client: Some(Box::new(Client::new(actions.clone()))),
        daemon: Some(Box::new(Daemon::new(actions))),
        cli: Some(Box::new(Cli)),
    }
}

/// Whether an entry should fire for a notification shown (or not) on some view.
pub fn fires(a: &NotifyAction, shown: bool) -> bool {
    !(a.only_when_hidden.unwrap_or(false) && shown)
}

/// `~/x` -> `$HOME/x`.
pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest),
        None => PathBuf::from(path),
    }
}

// ---------------------------------------------------------------------------- client

pub struct Client {
    actions: Actions,
    /// Reload from disk on `config.reloaded` (off in tests).
    reload: bool,
}

impl Client {
    pub fn new(actions: Actions) -> Client {
        Client {
            actions,
            reload: true,
        }
    }

    /// A client with fixed actions, for tests.
    pub fn fixed(actions: Actions) -> Client {
        Client {
            actions,
            reload: false,
        }
    }

    /// What the client does for one posted notification (REQ-NOTIFY-001/002).
    pub fn effects(&self, n: &m::NotifyPosted) -> Vec<HostEffect> {
        let Some(a) = n.event.as_ref().and_then(|e| self.actions.get(e)) else {
            return Vec::new();
        };
        let event = n.event.as_deref().unwrap_or("");
        if !fires(a, n.shown) {
            log::debug("plugin.notify", "notify.skipped")
                .msg(format!("{event}: shown, and only_when_hidden is set"))
                .field("event", event)
                .emit();
            return Vec::new();
        }
        let mut out = Vec::new();
        match a.sound.as_deref() {
            None | Some("") => {}
            Some("bell") => out.push(HostEffect::Bell),
            Some(file) => out.push(HostEffect::Sound(expand(file).to_string_lossy().into())),
        }
        if a.desktop.unwrap_or(false) {
            let body = match &n.pane_id {
                Some(p) => format!("pane {p}"),
                None => "femux".into(),
            };
            out.push(HostEffect::Desktop {
                title: n.title.clone(),
                body,
            });
        }
        let actions: Vec<&str> = out
            .iter()
            .map(|e| match e {
                HostEffect::Bell => "bell",
                HostEffect::Sound(_) => "sound",
                HostEffect::Desktop { .. } => "desktop",
                _ => "other",
            })
            .collect();
        log::info("plugin.notify", "notify.run")
            .msg(format!("{event}: {}", actions.join(", ")))
            .field("event", event)
            .field("actions", actions)
            .field("shown", n.shown)
            .emit();
        out
    }
}

impl ClientPlugin for Client {
    fn name(&self) -> &'static str {
        "notify"
    }
    fn on_event(&mut self, ev: &Event) -> Vec<HostEffect> {
        match ev {
            Event::ConfigReloaded(_) if self.reload => {
                self.actions = load();
                Vec::new()
            }
            Event::NotifyPosted(n) => self.effects(n),
            _ => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------- daemon

/// Posts a webhook: `POST url` with the notification as JSON. Runs on its own thread so the
/// plugin's event loop never waits on the network.
pub type Poster = Box<dyn Fn(String, serde_json::Value) + Send>;

/// A webhook URL's host: what REQ-LOG-004 lets the log say about it.
pub fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Credentials in the URL stay out too.
    host.rsplit_once('@').map_or(host, |(_, h)| h)
}

fn http_post(url: String, body: serde_json::Value) {
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let event = body["event"].as_str().unwrap_or("").to_owned();
        let r = agent
            .post(&url)
            .header("Content-Type", "application/json")
            .send(body.to_string());
        // REQ-LOG-005: each action's result, a failure at warn.
        match r {
            Ok(resp) => log::debug("plugin.notify", "notify.webhook")
                .msg(format!(
                    "webhook to {} answered {}",
                    url_host(&url),
                    resp.status()
                ))
                .field("event", event)
                .field("host", url_host(&url))
                .field("status", resp.status().as_u16())
                .emit(),
            Err(e) => log::warn("plugin.notify", "notify.webhook")
                .msg(format!("webhook to {} failed: {e}", url_host(&url)))
                .field("event", event)
                .field("host", url_host(&url))
                .field("error", e.to_string())
                .emit(),
        }
    });
}

pub struct Daemon {
    actions: Actions,
    reload: bool,
    post: Poster,
    /// Pane -> last `agent.state` seen, so a repeat is not a new notification.
    states: HashMap<String, String>,
    names: HashMap<String, String>,
}

impl Daemon {
    pub fn new(actions: Actions) -> Daemon {
        Daemon {
            actions,
            reload: true,
            post: Box::new(http_post),
            states: HashMap::new(),
            names: HashMap::new(),
        }
    }

    /// Fixed actions and a custom poster, for tests.
    pub fn with(actions: Actions, post: Poster) -> Daemon {
        Daemon {
            reload: false,
            post,
            ..Daemon::new(actions)
        }
    }

    /// `pane` is `None` for a pane that no longer exists (the daemon would refuse its id).
    fn raise(
        &self,
        event: &str,
        pane: Option<&str>,
        title: String,
        level: m::NotifyLevel,
    ) -> Vec<Request> {
        if !self.actions.contains_key(event) {
            return Vec::new();
        }
        let mut r = log::info("plugin.notify", "notify.post")
            .msg(format!("{event}: {title}"))
            .field("event", event)
            .field("title", title.clone());
        if let Some(p) = pane {
            r = r.pane(p);
        }
        r.emit();
        vec![
            m::NotifyPost {
                pane_id: pane.map(str::to_owned),
                title,
                level: Some(level),
                event: Some(event.to_owned()),
            }
            .into(),
        ]
    }
}

impl DaemonPlugin for Daemon {
    fn name(&self) -> &'static str {
        "notify"
    }

    fn subscription(&self) -> m::EventsSubscribe {
        m::EventsSubscribe {
            kinds: Some(vec![
                "pane.meta_changed".into(),
                "pane.exited".into(),
                "notify.posted".into(),
                "config.reloaded".into(),
            ]),
            ..Default::default()
        }
    }

    fn on_event(&mut self, ev: &Event) -> Vec<Request> {
        match ev {
            Event::ConfigReloaded(_) if self.reload => {
                self.actions = load();
                Vec::new()
            }
            Event::PaneMetaChanged(c) => {
                if let Some(n) = c.set.get("agent.name") {
                    self.names.insert(c.pane_id.clone(), n.clone());
                }
                let Some(state) = c.set.get("agent.state") else {
                    return Vec::new();
                };
                let before = self.states.insert(c.pane_id.clone(), state.clone());
                if before.as_ref() == Some(state) {
                    return Vec::new();
                }
                let level = match state.as_str() {
                    "done" => m::NotifyLevel::Done,
                    "blocked" => m::NotifyLevel::Warn,
                    "error" => m::NotifyLevel::Error,
                    _ => return Vec::new(),
                };
                let name = self
                    .names
                    .get(&c.pane_id)
                    .cloned()
                    .unwrap_or_else(|| "agent".into());
                self.raise(
                    &format!("agent_{state}"),
                    Some(&c.pane_id),
                    format!("{name} {state}"),
                    level,
                )
            }
            Event::PaneExited(e) => {
                self.states.remove(&e.pane_id);
                self.names.remove(&e.pane_id);
                let title = format!("pane {} exited ({})", e.pane_id, e.code);
                let level = if e.code == 0 {
                    m::NotifyLevel::Info
                } else {
                    m::NotifyLevel::Error
                };
                self.raise("pane_exited", None, title, level)
            }
            Event::NotifyPosted(n) => {
                let Some(event) = &n.event else {
                    return Vec::new();
                };
                let Some(a) = self.actions.get(event) else {
                    return Vec::new();
                };
                if let Some(url) = a.webhook.as_ref().filter(|u| !u.is_empty())
                    && fires(a, n.shown)
                {
                    (self.post)(
                        url.clone(),
                        json!({
                            "event": event,
                            "title": n.title,
                            "level": n.level,
                            "pane_id": n.pane_id,
                            "ts": n.ts,
                            "shown": n.shown,
                        }),
                    );
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn on_response(&mut self, _req: &Request, _resp: &Response) -> Vec<Request> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------- cli

struct Cli;

impl CliPlugin for Cli {
    fn name(&self) -> &'static str {
        "notify"
    }

    fn commands(&self) -> Vec<CliCommand> {
        vec![CliCommand {
            name: "notify test",
            about: "Post a test notification through the [notify] actions",
            args: vec![CliArg {
                name: "event",
                help: "agent_done (default), agent_blocked, agent_error or pane_exited",
                required: false,
            }],
            ..Default::default()
        }]
    }

    fn run(
        &self,
        _cmd: &str,
        input: &CliInput,
        core: &mut dyn CliCore,
    ) -> Result<CliOutput, String> {
        let event = input
            .args
            .first()
            .cloned()
            .unwrap_or_else(|| "agent_done".into());
        if !EVENTS.contains(&event.as_str()) {
            return Err(format!(
                "unknown event {event:?}; one of {}",
                EVENTS.join(", ")
            ));
        }
        let title = format!("femux test: {event}");
        core.request(
            m::NotifyPost {
                pane_id: core.pane(),
                title: title.clone(),
                level: Some(m::NotifyLevel::Info),
                event: Some(event.clone()),
            }
            .into(),
        )
        .map_err(|e| e.message)?;
        Ok(CliOutput {
            text: format!("posted {event}"),
            json: json!({ "event": event, "title": title }),
            attach: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REQ-LOG-004: a webhook is logged by its host, without path, query or credentials.
    #[test]
    fn log_004_webhooks_are_logged_by_host() {
        assert_eq!(url_host("https://ntfy.sh/mytopic?auth=x"), "ntfy.sh");
        assert_eq!(
            url_host("http://user:pw@hooks.local:8080/a#b"),
            "hooks.local:8080"
        );
        assert_eq!(url_host("ntfy.sh/topic"), "ntfy.sh");
    }

    fn posted(event: &str, shown: bool) -> m::NotifyPosted {
        m::NotifyPosted {
            seq: 1,
            ts: 2,
            pane_id: Some("%1".into()),
            title: "claude done".into(),
            level: m::NotifyLevel::Done,
            event: Some(event.into()),
            shown,
        }
    }

    fn actions() -> Actions {
        Config::parse(
            r#"
[notify]
agent_done    = { sound = "~/done.wav", desktop = true, only_when_hidden = true }
agent_blocked = { sound = "bell" }
agent_error   = { desktop = true, webhook = "http://127.0.0.1:1/hook" }
"#,
        )
        .unwrap()
        .notify
    }

    #[test]
    fn notify_001_sound_bell_and_desktop_become_host_effects() {
        let c = Client::fixed(actions());
        let fx = c.effects(&posted("agent_done", false));
        let HostEffect::Sound(f) = &fx[0] else {
            panic!("{fx:?}")
        };
        assert!(f.ends_with("/done.wav") && !f.starts_with('~'), "{f}");
        assert_eq!(
            fx[1],
            HostEffect::Desktop {
                title: "claude done".into(),
                body: "pane %1".into()
            }
        );
        assert_eq!(
            c.effects(&posted("agent_blocked", true)),
            [HostEffect::Bell]
        );
        // No entry, no event: nothing.
        assert!(c.effects(&posted("pane_exited", false)).is_empty());
        let mut bare = posted("agent_done", false);
        bare.event = None;
        assert!(c.effects(&bare).is_empty());
    }

    #[test]
    fn notify_002_only_when_hidden_skips_shown_panes() {
        let c = Client::fixed(actions());
        assert!(c.effects(&posted("agent_done", true)).is_empty());
        assert_eq!(c.effects(&posted("agent_done", false)).len(), 2);
        // Entries without the flag fire either way.
        assert!(!c.effects(&posted("agent_error", true)).is_empty());
    }
}
