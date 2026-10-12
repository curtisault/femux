//! REQ-CLI-001 (`--json` everywhere), REQ-CLI-002 (`$FEMUX_PANE` default), REQ-CLI-003
//! (completions), REQ-UI-012 (layout templates), plugin subcommands and agent hooks, all
//! against the real `femux` binary.

mod common;

use std::io::Write;
use std::process::Stdio;
use std::time::Duration;

use common::{Env, wait_until};
use femux_proto::msg as m;

/// A running daemon with session `s`; returns the first pane's id.
fn daemon(env: &Env) -> String {
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    env.json(&["tab", "new", "--session", "s"]);
    let tabs = tabs(env, "s");
    tabs[0].focus.clone()
}

fn tabs(env: &Env, session: &str) -> Vec<m::TabSnapshot> {
    let socket = env.socket();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = femux_test::Client::connect(&socket, femux_test::Client::hello(80, 24))
            .await
            .unwrap();
        let att = c
            .call(m::SessionAttach {
                name: session.into(),
                tab: None,
                cols: None,
                rows: None,
            })
            .await
            .unwrap();
        c.call(m::SessionDetach {
            view_id: att.view_id,
        })
        .await
        .unwrap();
        att.snapshot.tabs
    })
}

const LEAVES: &[&[&str]] = &[
    &["ls"],
    &["new"],
    &["kill"],
    &["attach"],
    &["daemon"],
    &["tab", "new"],
    &["tab", "close"],
    &["tab", "rename"],
    &["tab", "select"],
    &["pane", "split"],
    &["pane", "close"],
    &["pane", "focus"],
    &["pane", "resize"],
    &["pane", "info"],
    &["pane", "set"],
    &["pane", "cat"],
    &["pane", "signal"],
    &["pane", "respawn"],
    &["layout", "save"],
    &["layout", "load"],
    &["layout", "list"],
    &["layout", "export"],
    &["keys", "list"],
    &["keys", "explain"],
    &["integration", "install"],
    &["integration", "uninstall"],
    &["completions"],
    &["config", "check"],
    &["config", "edit"],
    &["reload"],
    &["clipboard", "set"],
    &["shell-integration"],
    &["launch"],
    &["agent", "list"],
    &["agent", "wait"],
    &["agent", "prompt"],
    &["agent", "explain"],
    &["notify", "test"],
];

#[test]
fn cli_001_every_subcommand_has_json() {
    let env = Env::new();
    for leaf in LEAVES {
        let mut args: Vec<&str> = leaf.to_vec();
        args.push("--help");
        let (ok, out, err) = env.try_run(&args);
        assert!(ok, "{leaf:?}: {err}");
        assert!(out.contains("--json"), "{leaf:?} lacks --json:\n{out}");
    }
    // Output is JSON on success and on failure.
    let pane = daemon(&env);
    let ls = env.json(&["ls"]);
    assert_eq!(ls["sessions"][0]["name"], "s");
    assert_eq!(ls["sessions"][0]["tabs"], 2);
    let keys = env.json(&["keys", "list"]);
    assert!(
        keys["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["key"] == "v" && b["action"] == "split right")
    );
    let explain = env.json(&["keys", "explain", "ctrl+p"]);
    assert_eq!(explain["bindings"][0]["action"], "palette");
    let info = env.json(&["pane", "info", "--pane", &pane]);
    assert_eq!(info["id"], pane);
    assert_eq!(info["session"], "s");
    let bad = env.json(&["kill", "nope"]);
    assert!(bad["error"].as_str().unwrap().contains("not_found"));
    // Text output stays human.
    assert!(env.run(&["ls"]).starts_with("s: 2 tabs"));
    assert!(env.run(&["keys", "explain", "v"]).contains("split right"));
}

#[test]
fn cli_002_commands_default_to_the_current_pane() {
    let env = Env::new();
    let pane = daemon(&env);
    let inside = |args: &[&str]| {
        let out = env
            .cmd(&["--json"])
            .args(args)
            .env("FEMUX_PANE", &pane)
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {text}"))
    };
    // Without a pane and outside femux: a clear error.
    let out = env.json(&["pane", "info"]);
    assert!(out["error"].as_str().unwrap().contains("no pane"));
    // Inside: set, info, cat, split, focus, tab rename all find the pane.
    inside(&["pane", "set", "agent.state=working", "agent.name=t"]);
    let info = inside(&["pane", "info"]);
    assert_eq!(info["meta"]["agent.state"], "working");
    inside(&["pane", "set", "--unset", "agent.name", "x=1"]);
    assert!(
        inside(&["pane", "info"])["meta"]
            .get("agent.name")
            .is_none()
    );
    let split = inside(&["pane", "split", "--dir", "h"]);
    let right = split["pane_id"].as_str().unwrap().to_owned();
    assert_ne!(right, pane);
    let focused = inside(&["pane", "focus", "l"]);
    assert_eq!(focused["pane_id"], right);
    inside(&["tab", "rename", "mine"]);
    let t = tabs(&env, "s");
    assert_eq!(t[0].custom_title.as_deref(), Some("mine"));
    // tab new lands in the pane's session; tab select by index applies to the session.
    let created = inside(&["tab", "new", "--title", "third"]);
    assert_eq!(tabs(&env, "s").len(), 3);
    inside(&["tab", "select", "1"]);
    inside(&["tab", "close", created["tab_id"].as_str().unwrap()]);
    assert_eq!(tabs(&env, "s").len(), 2);
    // cat shows what the pane printed.
    let socket = env.socket();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = femux_test::Client::connect(&socket, femux_test::Client::hello(80, 24))
            .await
            .unwrap();
        c.attach("s", 80, 24).await;
        c.wait_text(&pane, "$").await;
        c.type_text(&pane, "echo cat-$((20+22))\n").await;
        c.wait_text(&pane, "cat-42").await;
    });
    let cat = inside(&["pane", "cat", "--lines", "5"]);
    let lines: Vec<String> = cat["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_owned())
        .collect();
    assert!(lines.iter().any(|l| l == "cat-42"), "{lines:?}");
    assert!(lines.len() <= 5);
    let text = env
        .cmd(&["pane", "cat", "--lines", "5"])
        .env("FEMUX_PANE", &pane)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&text.stdout).contains("cat-42"));
    inside(&["pane", "signal", "SIGINT"]);
    inside(&["pane", "respawn"]);
    inside(&["pane", "close", "--pane", &right]);
    wait_until("pane closed", Duration::from_secs(5), || {
        env.json(&["pane", "info", "--pane", &right])["error"].is_string()
    });
}

#[test]
fn cli_003_completions_come_from_the_definition() {
    let env = Env::new();
    for shell in ["bash", "zsh", "fish"] {
        let out = env.run(&["completions", shell]);
        for word in [
            "layout",
            "pane",
            "keys",
            "clipboard",
            "shell-integration",
            "completions",
        ] {
            assert!(out.contains(word), "{shell} completions lack {word}");
        }
    }
    let j = env.json(&["completions", "fish"]);
    assert_eq!(j["shell"], "fish");
}

fn shape(n: &m::LayoutNode) -> String {
    match n {
        m::LayoutNode::PaneNode(_) => "p".into(),
        m::LayoutNode::SplitNode(s) => format!(
            "{}{}/{}({},{})",
            match s.dir {
                m::SplitDir::H => "h",
                m::SplitDir::V => "v",
            },
            s.ratio.num,
            s.ratio.den,
            shape(&s.children[0]),
            shape(&s.children[1])
        ),
    }
}

#[test]
fn ui_012_layout_templates_save_load_list_export() {
    let env = Env::new();
    let pane = daemon(&env);
    let run = |args: &[&str]| {
        let out = env
            .cmd(&["--json"])
            .args(args)
            .env("FEMUX_PANE", &pane)
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {text}"))
    };
    let b = run(&["pane", "split", "--dir", "h"])["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    run(&[
        "pane", "split", "--dir", "v", "--pane", &b, "--", "sleep", "600",
    ]);
    let saved = run(&["layout", "save", "work"]);
    let path = saved["path"].as_str().unwrap();
    assert!(path.ends_with("state/femux/layouts/work.json"), "{path}");
    assert_eq!(
        run(&["layout", "list"])["layouts"],
        serde_json::json!(["work"])
    );
    assert_eq!(env.run(&["layout", "list"]).trim(), "work");
    let exported = run(&["layout", "export", "work"]);
    assert_eq!(exported["tree"]["t"], "split");
    let panes = exported["panes"].as_object().unwrap();
    assert_eq!(panes.len(), 3);
    assert!(panes.values().any(|p| p["cmd"] == "sleep 600"), "{panes:?}");
    assert!(
        panes
            .values()
            .all(|p| p["cwd"].as_str().is_some_and(|c| !c.is_empty()))
    );
    // export without a name serializes the current tab the same way.
    let live = run(&["layout", "export"]);
    assert_eq!(live["tree"], exported["tree"]);
    // load rebuilds the shape (new pane ids) in a new tab, running the saved commands.
    let before = tabs(&env, "s");
    let loaded = run(&["layout", "load", "work"]);
    let after = tabs(&env, "s");
    assert_eq!(after.len(), before.len() + 1);
    let new_tab = after.iter().find(|t| t.id == loaded["tab_id"]).unwrap();
    assert_eq!(shape(&new_tab.tree), shape(&before[0].tree));
    let mut ids = Vec::new();
    femux_client::model::tree_panes(&new_tab.tree, &mut ids);
    let cmds: Vec<serde_json::Value> = ids
        .iter()
        .map(|p| env.json(&["pane", "info", "--pane", p])["cmd"].clone())
        .collect();
    assert!(cmds.iter().any(|c| *c == "sleep 600"), "{cmds:?}");
    assert!(env.json(&["layout", "load", "nope"])["error"].is_string());
}

#[test]
fn plugin_subcommands_and_config_bindings() {
    let env = Env::new();
    // shell-integration needs no daemon.
    let script = env.run(&["shell-integration", "bash"]);
    assert!(script.contains("133;A") && script.contains("FEMUX_PANE"));
    assert_eq!(env.json(&["shell-integration", "fish"])["shell"], "fish");
    assert!(env.json(&["shell-integration", "tcsh"])["error"].is_string());
    // clipboard set reaches attached clients as clipboard.changed.
    let pane = daemon(&env);
    let _ = pane;
    let socket = env.socket();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = femux_test::Client::connect(&socket, femux_test::Client::hello(80, 24))
            .await
            .unwrap();
        c.call(m::EventsSubscribe::default()).await.unwrap();
        let env2 = &env;
        let r = env2.json(&["clipboard", "set", "from the cli"]);
        assert_eq!(r["bytes"], 12);
        let ev = c.wait_event("clipboard.changed").await;
        let femux_proto::Event::ClipboardChanged(cc) = ev else {
            panic!()
        };
        assert_eq!(cc.text, "from the cli");
        // stdin when no text is given
        let mut child = env2
            .cmd(&["clipboard", "set"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"piped").unwrap();
        assert!(child.wait().unwrap().success());
        let ev = c.wait_event("clipboard.changed").await;
        let femux_proto::Event::ClipboardChanged(cc) = ev else {
            panic!()
        };
        assert_eq!(cc.text, "piped");
    });
    // A disabled plugin's subcommand is gone; config overrides reach keys.
    env.write_config("[plugins]\nenabled = [\"which-key\"]\n\n[keys.prefix]\nv = \"zoom\"\n");
    assert!(!env.try_run(&["shell-integration", "bash"]).0);
    assert!(env.run(&["keys", "explain", "v"]).contains("zoom"));
    env.write_config("[input]\nprefix = 7\n");
    let (ok, _, err) = env.try_run(&["keys", "list"]);
    assert!(!ok && err.contains("line 2"), "{err}");
}

#[test]
fn integration_install_hook_and_uninstall() {
    let env = Env::new();
    let home = env.dir.path();
    env.run(&["integration", "install", "claude-code"]);
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    for ev in ["UserPromptSubmit", "Stop", "Notification", "SessionEnd"] {
        assert!(
            settings["hooks"][ev]
                .to_string()
                .contains("femux integration hook claude-code"),
            "{ev}"
        );
    }
    env.run(&["integration", "install", "codex"]);
    assert!(
        std::fs::read_to_string(home.join(".codex/config.toml"))
            .unwrap()
            .contains("notify = [\"femux\"")
    );
    env.run(&["integration", "install", "opencode"]);
    assert!(home.join("config/opencode/plugin/femux.js").exists());
    env.run(&["integration", "install", "cursor"]);
    assert!(
        std::fs::read_to_string(home.join(".cursor/hooks.json"))
            .unwrap()
            .contains("femux integration hook cursor done")
    );
    assert!(env.json(&["integration", "install", "emacs"])["error"].is_string());
    // The hook sets agent.state on the pane it runs in.
    let pane = daemon(&env);
    let mut child = env
        .cmd(&["integration", "hook", "claude-code"])
        .env("FEMUX_PANE", &pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"Notification"}"#)
        .unwrap();
    assert!(child.wait().unwrap().success());
    let info = env.json(&["pane", "info", "--pane", &pane]);
    assert_eq!(info["meta"]["agent.state"], "blocked");
    assert_eq!(info["meta"]["agent.source"], "hook");
    assert_eq!(info["meta"]["agent.name"], "claude-code");
    let out = env
        .cmd(&["integration", "hook", "cursor", "done"])
        .env("FEMUX_PANE", &pane)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        env.json(&["pane", "info", "--pane", &pane])["meta"]["agent.state"],
        "done"
    );
    // Codex passes its event as an argument and leaves stdin open: the hook must not wait on it.
    let mut child = env
        .cmd(&[
            "integration",
            "hook",
            "codex",
            r#"{"type":"agent-turn-complete"}"#,
        ])
        .env("FEMUX_PANE", &pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let _keep_open = child.stdin.take();
    let status = common::exit_within(&mut child, Duration::from_secs(5))
        .expect("hook exits with stdin open");
    assert!(status.success());
    let info = env.json(&["pane", "info", "--pane", &pane]);
    assert_eq!(info["meta"]["agent.state"], "done");
    assert_eq!(info["meta"]["agent.name"], "codex");
    // Uninstall leaves the files without femux entries.
    env.run(&["integration", "uninstall", "claude-code"]);
    let settings = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
    assert!(!settings.contains("femux"), "{settings}");
    env.run(&["integration", "uninstall", "codex"]);
    assert!(
        !std::fs::read_to_string(home.join(".codex/config.toml"))
            .unwrap()
            .contains("femux")
    );
    env.run(&["integration", "uninstall", "opencode"]);
    assert!(!home.join("config/opencode/plugin/femux.js").exists());
}

#[test]
fn cfg_003_config_check_rejects_unknown_keys_with_lines() {
    let env = Env::new();
    // No config at all is valid: the defaults apply.
    assert_eq!(env.json(&["config", "check"])["ok"], true);
    env.write_config("[restore]\nenabled = true\nscrollbak = false\n");
    let (ok, _, err) = env.try_run(&["config", "check"]);
    assert!(!ok, "unknown key accepted");
    assert!(err.contains("line 3") && err.contains("scrollbak"), "{err}");
    // Every command refuses to run on a bad config rather than ignoring the key...
    let (ok, _, err) = env.try_run(&["ls"]);
    assert!(!ok && err.contains("scrollbak"), "{err}");
    // ...except config edit, which is how it gets fixed.
    let out = env
        .cmd(&["config", "edit"])
        .env(
            "EDITOR",
            "sh -c 'printf \"[restore]\\nenabled = true\\n\" > \"$0\"'",
        )
        .env_remove("VISUAL")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        env.try_run(&["ls"]).2.contains("no daemon"),
        "fixed config lets ls run"
    );
    env.write_config("[restore]\nenabled = true\n");
    let sessions = env.dir.path().join("config/femux/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("w.toml"),
        "name = \"w\"\n[[tab]]\ncmd = \"vim\"\n",
    )
    .unwrap();
    let (ok, _, err) = env.try_run(&["config", "check"]);
    assert!(
        !ok && err.contains("w.toml") && err.contains("line 3"),
        "{err}"
    );
    std::fs::write(
        sessions.join("w.toml"),
        "name = \"w\"\n[[tab]]\ncommand = \"vim\"\n",
    )
    .unwrap();
    let j = env.json(&["config", "check"]);
    assert_eq!(j["ok"], true);
    assert_eq!(j["files"].as_array().unwrap().len(), 2);
    // edit seeds a missing config with the defaults and validates the result.
    std::fs::remove_file(env.dir.path().join("config/femux/config.toml")).unwrap();
    let out = env
        .cmd(&["config", "edit"])
        .env("EDITOR", "true")
        .env_remove("VISUAL")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seeded = std::fs::read_to_string(env.dir.path().join("config/femux/config.toml")).unwrap();
    assert!(seeded.contains("[restore]"));
    let out = env
        .cmd(&["config", "edit"])
        .env("EDITOR", "sh -c 'echo bogus=1 >> \"$0\"'")
        .env_remove("VISUAL")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

/// ADR-013: `femux reload` applies config.toml to the running daemon; a bad file changes
/// nothing and says where it is wrong.
#[test]
fn reload_applies_the_config_to_the_running_daemon() {
    let env = Env::new();
    env.write_config("[daemon]\nkeep_alive = false\n");
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    env.write_config("[daemon]\nkeep_alive = true\n");
    assert_eq!(env.json(&["reload"])["reloaded"], true);
    // keep_alive now holds the daemon up past its last session.
    env.run(&["kill", "s"]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(env.socket().exists(), "keep_alive applied");
    env.write_config("[daemon]\nkeep_alive = true\nbogus = 1\n");
    let (ok, _, err) = env.try_run(&["reload"]);
    assert!(!ok && err.contains("line 3"), "{err}");
    env.write_config("[daemon]\nkeep_alive = true\n");
    let socket = env.socket();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut c = femux_test::Client::connect(&socket, femux_test::Client::hello(80, 24))
            .await
            .unwrap();
        c.call(m::ServerStop { kill_panes: true }).await.unwrap();
    });
}

fn pane_infos(env: &Env, session: &str) -> Vec<serde_json::Value> {
    tabs(env, session)
        .iter()
        .map(|t| env.json(&["pane", "info", "--pane", &t.focus]))
        .collect()
}

/// REQ-CFG-001/002: `femux launch` from a session file and from flags, `--save`, and a second
/// launch attaching to the existing session instead of creating another.
#[test]
fn cfg_001_002_launch_from_a_file_and_from_flags() {
    let env = Env::new();
    let home = env.dir.path().to_path_buf();
    std::fs::create_dir_all(home.join("writing/notes")).unwrap();
    let sessions = home.join("config/femux/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("writing.toml"),
        "name = \"writing\"\nroot = \"~/writing\"\ntabs = 4\n\n[[tab]]\ntitle = \"editor\"\ncommand = \"sleep 300\"\nrestore = true\n\n[[tab]]\ncommand = \"sleep 301\"\ncwd = \"~/writing/notes\"\n",
    )
    .unwrap();
    // No daemon yet: launch starts one.
    let j = env.json(&["launch", "writing", "--detach"]);
    assert_eq!(
        (j["created"].clone(), j["tabs"].clone()),
        (true.into(), 4.into()),
        "{j}"
    );
    let t = tabs(&env, "writing");
    assert_eq!(t.len(), 4);
    assert_eq!(t[0].custom_title.as_deref(), Some("editor"));
    let infos = pane_infos(&env, "writing");
    let root = home.join("writing");
    // Compare directories as the OS resolves them (macOS reports /var as /private/var).
    let same_dir = |reported: &serde_json::Value, want: &std::path::Path| {
        std::path::Path::new(reported.as_str().unwrap_or_default())
            .canonicalize()
            .ok()
            == want.canonicalize().ok()
    };
    assert_eq!(infos[0]["cmd"], "sleep 300");
    assert_eq!(infos[0]["restore"], true);
    assert_eq!(infos[1]["cmd"], "sleep 301");
    assert_eq!(infos[1]["restore"], false);
    assert!(
        same_dir(&infos[1]["cwd"], &root.join("notes")),
        "{}",
        infos[1]
    );
    // REQ-CFG-001: tabs beyond the listed ones are plain shells in the root.
    for i in [2, 3] {
        assert!(infos[i]["cmd"].is_null(), "{}", infos[i]);
        assert!(same_dir(&infos[i]["cwd"], &root), "{}", infos[i]);
    }
    let list = env.json(&["ls"]);
    assert_eq!(list["sessions"][0]["root"], root.to_string_lossy().as_ref());
    // Launching an existing session creates nothing.
    let again = env.json(&["launch", "writing", "--detach"]);
    assert_eq!(again["created"], false);
    assert_eq!(tabs(&env, "writing").len(), 4);

    // The flag form, saved: the same shape, and the saved file launches it again.
    let dir = root.to_string_lossy().into_owned();
    let j = env.json(&[
        "launch",
        "w2",
        "--tabs",
        "3",
        "--dir",
        &dir,
        "--save",
        "--detach",
        "--",
        "sleep 302",
        "sleep 303",
    ]);
    assert_eq!(j["tabs"], 3, "{j}");
    let saved = std::fs::read_to_string(sessions.join("w2.toml")).unwrap();
    let parsed = femux_config::SessionFile::parse(&saved).unwrap();
    assert_eq!(parsed.tabs, Some(3));
    assert_eq!(parsed.tab.len(), 2);
    assert_eq!(parsed.tab[1].command.as_deref(), Some("sleep 303"));
    let cmds = |env: &Env| -> Vec<serde_json::Value> {
        pane_infos(env, "w2")
            .iter()
            .map(|i| i["cmd"].clone())
            .collect()
    };
    let first = cmds(&env);
    env.run(&["kill", "w2"]);
    env.json(&["launch", "w2", "--detach"]);
    assert_eq!(cmds(&env), first);
    assert_eq!(first[0], "sleep 302");
    // Errors name the file and line.
    std::fs::write(sessions.join("bad.toml"), "name = \"bad\"\nwindows = 2\n").unwrap();
    let e = env.json(&["launch", "bad", "--detach"]);
    let e = e["error"].as_str().unwrap();
    assert!(e.contains("bad.toml") && e.contains("line 2"), "{e}");
}
