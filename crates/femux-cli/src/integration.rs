//! `femux integration install|uninstall <agent>` (REQ-AGT-005): writes the agent's hook
//! configuration so its events call the tier-1 command `femux integration hook <agent>`,
//! which sets `agent.state` (REQ-AGT-001) on the pane the agent runs in. `uninstall` removes
//! exactly what `install` wrote.

use std::path::PathBuf;

use serde_json::{Value, json};

pub const AGENTS: &[&str] = &["claude-code", "codex", "opencode", "cursor"];

/// The marker every written hook carries, so uninstall can find it.
const MARK: &str = "femux integration hook";

fn home() -> PathBuf {
    crate::config::home()
}

/// Where each agent keeps its hooks.
pub fn hook_file(agent: &str) -> Result<PathBuf, String> {
    Ok(match agent {
        "claude-code" => home().join(".claude/settings.json"),
        "codex" => home().join(".codex/config.toml"),
        "opencode" => crate::config::config_dir()
            .parent()
            .unwrap_or(&home())
            .join("opencode/plugin/femux.js"),
        "cursor" => home().join(".cursor/hooks.json"),
        _ => {
            return Err(format!(
                "unknown agent {agent:?}; one of {}",
                AGENTS.join(", ")
            ));
        }
    })
}

fn read_json(p: &PathBuf) -> Result<Value, String> {
    match std::fs::read_to_string(p) {
        Ok(t) if t.trim().is_empty() => Ok(json!({})),
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn write(p: &PathBuf, text: &str) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(p, text).map_err(|e| format!("{}: {e}", p.display()))
}

/// Claude Code: `hooks.<Event>[].hooks[]` command entries (settings.json).
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "SubagentStop",
    "Notification",
    "SessionEnd",
];

fn claude_install(mut settings: Value) -> Value {
    if !settings.is_object() {
        settings = json!({});
    }
    let hooks = settings["hooks"].as_object().cloned().unwrap_or_default();
    let mut hooks = Value::Object(hooks);
    for ev in CLAUDE_EVENTS {
        let entry =
            json!({"hooks": [{"type": "command", "command": format!("{MARK} claude-code")}]});
        let list = hooks[*ev].as_array().cloned().unwrap_or_default();
        let mut list: Vec<Value> = list.into_iter().filter(|e| !has_mark(e)).collect();
        list.push(entry);
        hooks[*ev] = Value::Array(list);
    }
    settings["hooks"] = hooks;
    settings
}

fn claude_uninstall(mut settings: Value) -> Value {
    if let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) {
        for (_, list) in hooks.iter_mut() {
            if let Some(arr) = list.as_array_mut() {
                arr.retain(|e| !has_mark(e));
            }
        }
        hooks.retain(|_, v| v.as_array().is_some_and(|a| !a.is_empty()));
    }
    settings
}

fn has_mark(v: &Value) -> bool {
    v.to_string().contains(MARK)
}

/// Cursor: `hooks.json` with `hooks.<event>[].command`.
fn cursor_install(mut v: Value) -> Value {
    if !v.is_object() {
        v = json!({});
    }
    v["version"] = json!(1);
    let mut hooks = v["hooks"].as_object().cloned().unwrap_or_default();
    for (ev, state) in [
        ("beforeSubmitPrompt", "working"),
        ("stop", "done"),
        ("afterFileEdit", "working"),
    ] {
        let mut list: Vec<Value> = hooks
            .get(ev)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !has_mark(e))
            .collect();
        list.push(json!({"command": format!("{MARK} cursor {state}")}));
        hooks.insert(ev.to_owned(), Value::Array(list));
    }
    v["hooks"] = Value::Object(hooks);
    v
}

fn cursor_uninstall(v: Value) -> Value {
    claude_uninstall(v)
}

const SAVED: &str = "# femux saved: ";

fn is_notify(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with("notify ") || t.starts_with("notify=")
}

/// Codex: the single top-level `notify = [...]` line in config.toml. A user's own `notify`
/// is kept as a `# femux saved:` comment and restored by uninstall, since TOML allows one.
fn codex_install(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for l in codex_strip(text) {
        if is_notify(&l) && !l.contains(MARK) {
            out.push(format!("{SAVED}{l}"));
        } else {
            out.push(l);
        }
    }
    let line = format!("notify = [\"femux\", \"integration\", \"hook\", \"codex\"] # {MARK}");
    // Top-level keys must come before the first table header.
    let at = out
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(out.len());
    out.insert(at, line);
    out.join("\n") + "\n"
}

/// Removes femux's line and restores a saved `notify`.
fn codex_strip(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !(is_notify(l) && l.contains(MARK)))
        .map(|l| l.strip_prefix(SAVED).unwrap_or(l).to_owned())
        .collect()
}

/// OpenCode: a plugin file that forwards events.
fn opencode_plugin() -> String {
    format!(
        r#"// {MARK}: forwards OpenCode events to femux (REQ-AGT-001).
export const FemuxPlugin = async ({{ $ }}) => ({{
  event: async ({{ event }}) => {{
    const states = {{
      "session.idle": "idle",
      "message.updated": "working",
      "permission.updated": "blocked",
      "session.error": "error",
    }};
    const state = states[event.type];
    if (state) await $`femux integration hook opencode ${{state}}`.quiet().nothrow();
  }},
}});
"#
    )
}

/// Installs the hooks; returns the file written.
pub fn install(agent: &str) -> Result<PathBuf, String> {
    let p = hook_file(agent)?;
    match agent {
        "claude-code" => {
            let v = claude_install(read_json(&p)?);
            write(&p, &serde_json::to_string_pretty(&v).unwrap())?;
        }
        "cursor" => {
            let v = cursor_install(read_json(&p)?);
            write(&p, &serde_json::to_string_pretty(&v).unwrap())?;
        }
        "codex" => {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            write(&p, &codex_install(&text))?;
        }
        "opencode" => write(&p, &opencode_plugin())?,
        _ => unreachable!(),
    }
    Ok(p)
}

/// Removes what `install` wrote; returns the file touched.
pub fn uninstall(agent: &str) -> Result<PathBuf, String> {
    let p = hook_file(agent)?;
    match agent {
        "claude-code" => {
            let v = claude_uninstall(read_json(&p)?);
            write(&p, &serde_json::to_string_pretty(&v).unwrap())?;
        }
        "cursor" => {
            let v = cursor_uninstall(read_json(&p)?);
            write(&p, &serde_json::to_string_pretty(&v).unwrap())?;
        }
        "codex" => {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let mut out = codex_strip(&text).join("\n");
            if !out.is_empty() {
                out.push('\n');
            }
            write(&p, &out)?;
        }
        "opencode" => {
            if p.exists() {
                std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        _ => unreachable!(),
    }
    Ok(p)
}

/// The agent state for a hook invocation: an explicit `state` argument, a Claude Code event
/// (JSON on stdin with `hook_event_name`), or a Codex notification (JSON argument with `type`).
pub fn state_for(agent: &str, arg: Option<&str>, stdin: &str) -> Option<&'static str> {
    let fixed = |s: &str| -> Option<&'static str> {
        ["idle", "working", "blocked", "done", "error", "exited"]
            .into_iter()
            .find(|x| *x == s)
    };
    if let Some(a) = arg.filter(|a| !a.starts_with('{')) {
        return fixed(a);
    }
    let payload: Value = arg
        .and_then(|a| serde_json::from_str(a).ok())
        .or_else(|| serde_json::from_str(stdin).ok())
        .unwrap_or(Value::Null);
    match agent {
        "claude-code" => match payload["hook_event_name"].as_str()? {
            "SessionStart" => Some("idle"),
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some("working"),
            "Stop" | "SubagentStop" => Some("done"),
            "Notification" => Some("blocked"),
            "SessionEnd" => Some("exited"),
            _ => None,
        },
        "codex" => match payload["type"].as_str()? {
            "agent-turn-complete" => Some("done"),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_hooks_install_idempotently_and_uninstall_cleanly() {
        let base = json!({"model": "opus", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}});
        let once = claude_install(base.clone());
        let twice = claude_install(once.clone());
        assert_eq!(once, twice);
        assert_eq!(twice["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert!(
            twice["hooks"]["UserPromptSubmit"]
                .to_string()
                .contains(MARK)
        );
        let gone = claude_uninstall(twice);
        assert_eq!(gone, base);
    }

    #[test]
    fn codex_notify_line_goes_before_tables() {
        let t = codex_install("model = \"o3\"\n\n[profiles.x]\na = 1\n");
        assert_eq!(
            t.lines().nth(2).unwrap(),
            "notify = [\"femux\", \"integration\", \"hook\", \"codex\"] # femux integration hook"
        );
        let again = codex_install(&t);
        assert_eq!(again.matches("notify").count(), 1);
        assert!(!codex_strip(&again).join("\n").contains("femux"));
        // A user's own notify is saved, never duplicated, and restored by uninstall.
        let own = "model = \"o3\"\nnotify = [\"my-notifier\"]\n";
        let t = codex_install(own);
        let active: Vec<&str> = t.lines().filter(|l| is_notify(l)).collect();
        assert_eq!(active.len(), 1, "{t}");
        assert!(active[0].contains("femux"));
        assert!(t.contains("# femux saved: notify = [\"my-notifier\"]"));
        assert_eq!(codex_strip(&t).join("\n") + "\n", own);
    }

    #[test]
    fn states_derive_from_events() {
        assert_eq!(
            state_for("claude-code", None, r#"{"hook_event_name":"Stop"}"#),
            Some("done")
        );
        assert_eq!(
            state_for("claude-code", None, r#"{"hook_event_name":"Notification"}"#),
            Some("blocked")
        );
        assert_eq!(
            state_for("codex", Some(r#"{"type":"agent-turn-complete"}"#), ""),
            Some("done")
        );
        assert_eq!(state_for("cursor", Some("working"), ""), Some("working"));
        assert_eq!(state_for("cursor", Some("bogus"), ""), None);
        assert!(opencode_plugin().contains("permission.updated"));
        assert!(
            cursor_install(json!({}))["hooks"]["stop"]
                .to_string()
                .contains("done")
        );
    }
}
