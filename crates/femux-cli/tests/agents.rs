//! SPEC §9.3 `femux agent …` and the 0.5.0 acceptance path: a Claude Code hook reports
//! states and `femux agent wait` returns, against the real binary and daemon.

mod common;

use std::io::Write;
use std::process::Stdio;
use std::time::Duration;

use common::{Env, exit_within, wait_until};

fn pane(env: &Env) -> String {
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    let ls = env.json(&["agent", "list", "--all"]);
    ls["agents"][0]["pane_id"].as_str().unwrap().to_owned()
}

fn hook(env: &Env, pane: &str, event: &str) {
    let mut child = env
        .cmd(&["integration", "hook", "claude-code"])
        .env("FEMUX_PANE", pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!(r#"{{"hook_event_name":"{event}"}}"#).as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn agent_wait_returns_when_the_claude_code_hook_reports_done() {
    let env = Env::new();
    let pane = pane(&env);
    hook(&env, &pane, "UserPromptSubmit");
    let waiter = env
        .cmd(&[
            "--json",
            "agent",
            "wait",
            &pane,
            "--until",
            "done",
            "--timeout",
            "10",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    hook(&env, &pane, "Stop");
    let out = waiter.wait_with_output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["state"], "done");
    // Already there: returns at once.
    assert_eq!(
        env.run(&["agent", "wait", &pane, "--until", "done,blocked"])
            .trim(),
        "done"
    );
    // A state that never comes: times out with an error.
    let mut child = env
        .cmd(&[
            "agent",
            "wait",
            &pane,
            "--until",
            "error",
            "--timeout",
            "0.3",
        ])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let status = exit_within(&mut child, Duration::from_secs(5)).expect("wait times out");
    assert!(!status.success());
    assert!(env.json(&["agent", "wait", &pane, "--until", "sleepy"])["error"].is_string());
}

#[test]
fn agent_list_and_explain_show_state_source_and_evidence() {
    let env = Env::new();
    let pane = pane(&env);
    assert_eq!(
        env.json(&["agent", "list"])["agents"],
        serde_json::json!([])
    );
    hook(&env, &pane, "Notification");
    let list = env.json(&["agent", "list"]);
    assert_eq!(list["agents"][0]["pane_id"], pane);
    assert_eq!(list["agents"][0]["state"], "blocked");
    assert_eq!(list["agents"][0]["source"], "hook");
    assert_eq!(list["agents"][0]["name"], "claude-code");
    assert!(env.run(&["agent", "list"]).contains("blocked"));
    // The plugin records evidence for what it accepted.
    wait_until("evidence", Duration::from_secs(5), || {
        env.json(&["agent", "explain", &pane])["evidence"].is_string()
    });
    let text = env.run(&["agent", "explain", &pane]);
    assert!(text.contains("blocked (hook"), "{text}");
    assert!(text.contains("claude-code"), "{text}");
    let explain = env
        .cmd(&["agent", "explain"])
        .env("FEMUX_PANE", &pane)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&explain.stdout).contains("blocked"));
}

#[test]
fn agent_prompt_types_text_and_enter() {
    let env = Env::new();
    let pane = pane(&env);
    env.run(&["agent", "prompt", &pane, "echo prompted-$((6*7))"]);
    wait_until("the prompt ran", Duration::from_secs(5), || {
        env.run(&["pane", "cat", "--pane", &pane])
            .contains("prompted-42")
    });
    // --wait returns once the agent reports an end state.
    hook(&env, &pane, "UserPromptSubmit");
    let mut child = env
        .cmd(&[
            "agent",
            "prompt",
            &pane,
            "true",
            "--wait",
            "--timeout",
            "10",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    hook(&env, &pane, "Stop");
    let status = exit_within(&mut child, Duration::from_secs(10)).expect("prompt --wait returns");
    assert!(status.success());
}
