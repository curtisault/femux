//! SPEC §3.9 (REQ-LOG-*): the log every femux process writes, debug mode, `femux debug`, all
//! against the real `femux` binary with its state in a temp directory.

mod common;

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;

use common::{Env, wait_until};
use serde_json::Value;

fn log_path(env: &Env) -> PathBuf {
    env.dir.path().join("state/femux/logs/default.jsonl")
}

fn records(env: &Env) -> Vec<Value> {
    std::fs::read_to_string(log_path(env))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
        .collect()
}

fn has(env: &Env, f: impl Fn(&Value) -> bool) -> bool {
    records(env).iter().any(f)
}

#[test]
fn log_001_every_process_appends_to_one_file_per_server() {
    let env = Env::new();
    env.run(&["--debug", "daemon"]);
    env.run(&["--debug", "new", "s"]);
    env.run(&["--debug", "ls"]);
    let path = log_path(&env);
    wait_until("the daemon's proc.start", Duration::from_secs(5), || {
        has(&env, |r| {
            r["proc"] == "daemon" && r["event"] == "proc.start"
        })
    });
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
        0o700
    );
    let rs = records(&env);
    let procs: std::collections::BTreeSet<&str> =
        rs.iter().filter_map(|r| r["proc"].as_str()).collect();
    assert!(
        procs.contains("daemon") && procs.contains("cli"),
        "{procs:?}"
    );
    // Each process has its own run id; a daemon started in debug mode reports it.
    let runs: std::collections::BTreeSet<&str> =
        rs.iter().filter_map(|r| r["run"].as_str()).collect();
    assert!(runs.len() >= 4, "daemon, daemon spawner, new, ls: {runs:?}");
    let start = rs
        .iter()
        .find(|r| r["proc"] == "daemon" && r["event"] == "proc.start")
        .unwrap();
    assert_eq!(start["fields"]["level"], "debug");
    assert_eq!(
        femux_test_path(&env.run(&["debug", "path"])),
        path.display().to_string()
    );
}

fn femux_test_path(out: &str) -> String {
    out.trim().to_owned()
}

#[test]
fn log_003_without_debug_only_warnings_and_errors_are_written() {
    let env = Env::new();
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    env.run(&["ls"]);
    // A failing command is a warning.
    let bad = env.cmd(&["kill", "nope"]).output().unwrap();
    assert!(!bad.status.success());
    wait_until(
        "the failed command's warning",
        Duration::from_secs(5),
        || has(&env, |r| r["event"] == "proto.error"),
    );
    for r in records(&env) {
        assert!(
            r["level"] == "warn" || r["level"] == "error",
            "outside debug mode: {r}"
        );
    }
}

#[test]
fn log_003_debug_on_and_off_switch_a_running_daemon() {
    let env = Env::new();
    env.run(&["daemon"]);
    let st = env.json(&["debug", "status"]);
    assert_eq!(st["debug"], false);
    assert_eq!(st["running"], true);
    assert_eq!(st["level"], "warn");
    let on = env.json(&["debug", "on"]);
    assert_eq!(on["debug"], true);
    assert_eq!(on["level"], "debug");
    assert_eq!(on["path"], log_path(&env).display().to_string());
    // The daemon now writes debug records: a request it answers is logged.
    env.run(&["new", "s"]);
    wait_until("the daemon's proto records", Duration::from_secs(5), || {
        has(&env, |r| {
            r["proc"] == "daemon"
                && r["event"] == "proto.response"
                && r["fields"]["kind"] == "session.create"
        })
    });
    assert!(has(&env, |r| r["event"] == "debug.level"
        && r["fields"]["level"] == "debug"));
    assert_eq!(env.json(&["debug", "on", "--trace"])["level"], "trace");
    let off = env.json(&["debug", "off"]);
    assert_eq!(off["debug"], false);
    let n = records(&env).len();
    env.run(&["ls"]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        records(&env)[n..]
            .iter()
            .all(|r| r["level"] == "warn" || r["level"] == "error"),
        "off again"
    );
    // A bad level is refused before anything runs.
    let bad = env.cmd(&["--debug=loud", "ls"]).output().unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("debug or trace"));
}

#[test]
fn log_003_femux_debug_env_and_config_level() {
    let env = Env::new();
    let out = env.cmd(&["ls"]).env("FEMUX_DEBUG", "1").output().unwrap();
    assert!(!out.status.success(), "no daemon yet");
    assert!(has(&env, |r| r["proc"] == "cli" && r["event"] == "proc.start"));
    // `[debug] level` is what a daemon starts at.
    let cfg = env.dir.path().join("config/femux/config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "[debug]\nlevel = \"trace\"\n").unwrap();
    env.run(&["daemon"]);
    assert_eq!(env.json(&["debug", "status"])["level"], "trace");
}

#[test]
fn log_007_snapshot_writes_the_state_as_one_record() {
    let env = Env::new();
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    env.json(&["tab", "new", "--session", "s"]);
    let out = env.json(&["debug", "snapshot"]);
    assert_eq!(out["path"], log_path(&env).display().to_string());
    // Outside debug mode too.
    let snap = records(&env)
        .into_iter()
        .find(|r| r["event"] == "debug.snapshot")
        .expect("a debug.snapshot record");
    assert_eq!(snap["level"], "info");
    let f = &snap["fields"];
    assert_eq!(f["server"]["sessions"], 1);
    assert_eq!(f["sessions"][0]["name"], "s");
    assert_eq!(f["sessions"][0]["tabs"].as_array().unwrap().len(), 2);
    let panes = f["sessions"][0]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 2);
    assert!(panes[0]["cwd"].is_string() && panes[0].get("title").is_none());
    assert_eq!(f["debug_level"], "warn");
}

#[test]
fn log_008_tail_filters_and_passes_json_through() {
    let env = Env::new();
    env.run(&["--debug", "daemon"]);
    env.run(&["--debug", "new", "s"]);
    wait_until("records", Duration::from_secs(5), || {
        has(&env, |r| {
            r["event"] == "proc.start" && r["proc"] == "daemon"
        })
    });
    let out = env.run(&["--json", "debug", "tail", "--event", "proc.*"]);
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(!lines.is_empty());
    assert!(
        lines
            .iter()
            .all(|r| r["event"].as_str().unwrap().starts_with("proc."))
    );
    // The lines are the file's own.
    let file = std::fs::read_to_string(log_path(&env)).unwrap();
    assert!(out.lines().all(|l| file.contains(l)));
    let warn = env.run(&["--json", "debug", "tail", "--level", "warn"]);
    assert!(warn.lines().all(|l| {
        let r: Value = serde_json::from_str(l).unwrap();
        r["level"] == "warn" || r["level"] == "error"
    }));
    let human = env.run(&["debug", "tail", "--event", "proc.start", "--since", "1h"]);
    assert!(
        human
            .lines()
            .all(|l| l.contains(" info  ") && l.contains("proc.start")),
        "{human}"
    );
    // --follow prints what is written after it started.
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Kill(
        env.cmd(&["--json", "debug", "tail", "--follow", "--event", "bus.*"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(400));
    env.run(&["--debug", "kill", "s"]);
    let mut stdout = child.0.stdout.take().unwrap();
    let got = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let g2 = got.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            g2.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    wait_until("a followed record", Duration::from_secs(5), || {
        got.lock()
            .unwrap()
            .lines()
            .any(|l| l.contains("\"kind\":\"session.closed\""))
    });
}

/// The first pane of session `s`, from `tab new`'s answer.
fn new_tab(env: &Env, args: &[&str]) -> String {
    let mut all = vec!["tab", "new", "--session", "s"];
    all.extend_from_slice(args);
    env.json(&all)["pane_id"].as_str().unwrap().to_owned()
}

#[test]
fn log_004_no_record_holds_typed_text_output_clipboard_or_arguments() {
    let env = Env::new();
    env.run(&["--debug=trace", "daemon"]);
    env.run(&["--debug=trace", "new", "s"]);
    let pane = new_tab(&env, &["--", "env", "TOKEN=SECRET_ENV_VALUE", "sh"]);
    env.run(&[
        "--debug=trace",
        "agent",
        "prompt",
        &pane,
        "echo SECRET_TYPED_TEXT",
    ]);
    env.run(&["--debug=trace", "clipboard", "set", "SECRET_CLIPBOARD"]);
    // The text reached the pane, and its output came back.
    wait_until("the typed text echoed", Duration::from_secs(10), || {
        env.run(&["pane", "cat", "--pane", &pane])
            .contains("SECRET_TYPED_TEXT")
    });
    env.run(&["--debug=trace", "debug", "snapshot"]);
    wait_until("the input's trace record", Duration::from_secs(5), || {
        has(&env, |r| {
            r["event"] == "proto.response" && r["fields"]["kind"] == "pane.input"
        })
    });
    let text = std::fs::read_to_string(log_path(&env)).unwrap();
    for secret in ["SECRET_ENV_VALUE", "SECRET_TYPED_TEXT", "SECRET_CLIPBOARD"] {
        assert!(!text.contains(secret), "{secret} is in the log");
    }
    // What is there instead: sizes and program names.
    let input = records(&env)
        .into_iter()
        .find(|r| r["fields"]["kind"] == "pane.input" && r["fields"]["body"].is_object())
        .expect("a pane.input body at trace");
    assert!(
        input["fields"]["body"]["events"]["redacted"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(has(&env, |r| {
        r["event"] == "pane.spawn" && r["pane"] == pane.as_str() && r["fields"]["program"] == "env"
    }));
}

#[test]
fn log_005_the_catalog_reaches_the_log() {
    let env = Env::new();
    env.run(&["--debug", "daemon"]);
    env.run(&["--debug", "new", "s"]);
    let pane = new_tab(&env, &[]);
    // A hook's agent state goes through the agents plugin (REQ-AGT-001).
    env.run(&[
        "pane",
        "set",
        "--pane",
        &pane,
        "agent.state=done",
        "agent.name=claude",
    ]);
    env.run(&["--debug", "pane", "close", "--pane", &pane]);
    let want = [
        ("daemon", "proc.start"),
        ("daemon", "client.connect"),
        ("daemon", "proto.response"),
        ("daemon", "pane.spawn"),
        ("daemon", "pane.exit"),
        ("daemon", "bus.event"),
        ("daemon", "plugin.start"),
        ("daemon", "agent.propose"),
        ("daemon", "agent.state"),
        ("cli", "proc.start"),
        ("cli", "proc.stop"),
        ("cli", "proto.response"),
    ];
    let missing = || -> Vec<(&str, &str)> {
        let rs = records(&env);
        want.iter()
            .copied()
            .filter(|(p, e)| !rs.iter().any(|r| r["proc"] == *p && r["event"] == *e))
            .collect()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !missing().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(missing().is_empty(), "missing: {:?}", missing());
    let state = records(&env)
        .into_iter()
        .find(|r| r["event"] == "agent.state")
        .unwrap();
    assert_eq!(state["pane"], pane.as_str());
    assert_eq!(state["fields"]["to"], "done");
    assert_eq!(state["fields"]["source"], "hook");
    assert!(
        state["fields"]["evidence"]
            .as_str()
            .unwrap()
            .contains("hook")
    );
    // A panic is recorded with where it happened, in every mode.
    let crash = env.cmd(&["debug", "crash"]).output().unwrap();
    assert!(!crash.status.success());
    let panic = records(&env)
        .into_iter()
        .find(|r| r["event"] == "proc.panic")
        .expect("a proc.panic record");
    assert_eq!(panic["level"], "error");
    assert_eq!(panic["fields"]["message"], "femux debug crash");
    assert!(panic["fields"]["backtrace"].is_string());
}

#[test]
fn log_006_debug_mode_writes_metrics() {
    let env = Env::new();
    let cfg = env.dir.path().join("config/femux/config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "[debug]\nmetrics_interval_ms = 200\n").unwrap();
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        !has(&env, |r| r["event"] == "metrics"),
        "no telemetry outside debug mode"
    );
    env.run(&["debug", "on"]);
    env.run(&["ls"]);
    wait_until(
        "a metrics record with requests",
        Duration::from_secs(5),
        || {
            has(&env, |r| {
                r["event"] == "metrics"
                    && r["proc"] == "daemon"
                    && r["fields"]["panes"] == 1
                    && r["fields"]["counters"]["requests.session.list"].as_u64() >= Some(1)
            })
        },
    );
    let m = records(&env)
        .into_iter()
        .rfind(|r| r["event"] == "metrics" && r["proc"] == "daemon")
        .unwrap();
    for k in [
        "rss_bytes",
        "open_fds",
        "threads",
        "sessions",
        "views",
        "busiest_panes",
        "timings",
    ] {
        assert!(m["fields"].get(k).is_some(), "{k} missing: {m}");
    }
}
