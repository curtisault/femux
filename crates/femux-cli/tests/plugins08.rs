//! 0.8.0 plugins against the real binary: git worktree tabs, broadcast typing and asciinema
//! recording (the milestone's acceptance).

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use common::{Env, wait_until};

fn first_pane(env: &Env) -> String {
    env.json(&["agent", "list", "--all"])["agents"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn git_worktree_tabs_open_list_and_go() {
    let env = Env::new();
    let repo = env.dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "a").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    env.run(&["daemon"]);
    env.run(&["new", "s", "--root", repo.to_str().unwrap()]);
    let pane = first_pane(&env);
    let tab = |args: &[&str]| {
        let out = env
            .cmd(&[&["--json", "git"][..], args].concat())
            .current_dir(&repo)
            .env("FEMUX_PANE", &pane)
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let v = tab(&["tab", "feature/x"]);
    let wt = repo.join(".worktrees/feature/x");
    assert_eq!(v["created"], true, "{v}");
    assert!(wt.join("a.txt").exists());
    // A new tab in the worktree, titled with the branch.
    let tab_id = v["tab_id"].as_str().unwrap().to_owned();
    let tabs = env.json(&["ls"]);
    assert_eq!(tabs["sessions"][0]["tabs"], 2);
    // Again: the same worktree and tab.
    let again = tab(&["tab", "feature/x"]);
    assert_eq!(
        (again["created"].as_bool(), again["tab_id"].as_str()),
        (Some(false), Some(tab_id.as_str()))
    );
    // .worktrees stays out of git status.
    let st = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&st.stdout).trim().is_empty());
    let list = tab(&["tabs"]);
    assert_eq!(list["worktrees"][0]["branch"], "feature/x");
    assert_eq!(list["worktrees"][0]["tab_id"], tab_id.as_str());
    // Dirty: refused; --force removes it and closes the tab.
    std::fs::write(wt.join("b.txt"), "b").unwrap();
    assert!(
        tab(&["rm", "feature/x"])["error"]
            .as_str()
            .unwrap()
            .contains("uncommitted")
    );
    let gone = tab(&["rm", "feature/x", "--force"]);
    assert_eq!(gone["closed_tabs"][0], tab_id.as_str());
    assert!(!wt.exists());
    assert_eq!(env.json(&["ls"])["sessions"][0]["tabs"], 1);
}

#[test]
fn broadcast_on_types_into_every_pane() {
    let env = Env::new();
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    let a = first_pane(&env);
    let b = env.json(&["pane", "split", "--pane", &a])["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    std::thread::sleep(Duration::from_millis(300));
    let on = env
        .cmd(&["--json", "broadcast", "on"])
        .env("FEMUX_PANE", &a)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&on.stdout).contains("\"broadcast\": true"),
        "{on:?}"
    );
    env.run(&["agent", "prompt", &a, "echo bcast-$((6*7))"]);
    for p in [&a, &b] {
        wait_until("both panes", Duration::from_secs(10), || {
            env.run(&["pane", "cat", "--pane", p]).contains("bcast-42")
        });
    }
    let off = env
        .cmd(&["broadcast", "off"])
        .env("FEMUX_PANE", &a)
        .output()
        .unwrap();
    assert!(off.status.success());
}

#[test]
fn recordings_are_asciinema_casts() {
    let env = Env::new();
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    let pane = first_pane(&env);
    std::thread::sleep(Duration::from_millis(300));
    // Foreground: `femux record` until --duration.
    let file = env.dir.path().join("fg.cast");
    let child = env
        .cmd(&["record", &pane, file.to_str().unwrap(), "--duration", "2"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    env.run(&["agent", "prompt", &pane, "echo rec-$((6*7))"]);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let cast = std::fs::read_to_string(&file).unwrap();
    let header: serde_json::Value = serde_json::from_str(cast.lines().next().unwrap()).unwrap();
    assert_eq!(header["version"], 2);
    assert!(header["width"].as_u64().unwrap() > 0);
    assert!(cast.contains("rec-42"), "{cast}");
    // Toggled (what prefix R does): the daemon side records into the state dir.
    env.run(&["pane", "set", "--pane", &pane, "recorder.on=1"]);
    wait_until("recording", Duration::from_secs(10), || {
        env.json(&["pane", "info", "--pane", &pane])["meta"]["recorder.file"].is_string()
    });
    env.run(&["agent", "prompt", &pane, "echo toggled-$((6*7))"]);
    std::thread::sleep(Duration::from_millis(500));
    env.run(&["pane", "set", "--pane", &pane, "--unset", "recorder.on"]);
    wait_until("stopped", Duration::from_secs(10), || {
        env.json(&["pane", "info", "--pane", &pane])["meta"]["recorder.last"].is_string()
    });
    let info = env.json(&["pane", "info", "--pane", &pane]);
    let path = info["meta"]["recorder.last"].as_str().unwrap();
    assert!(
        path.contains("/recordings/") && path.ends_with(".cast"),
        "{path}"
    );
    let cast = std::fs::read_to_string(path).unwrap();
    assert!(cast.contains("toggled-42"), "{cast}");
    assert!(info["meta"]["recorder.file"].is_null());
}
