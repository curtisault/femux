//! REQ-TEST-006: agent detection is fed scripted process states and output through a live
//! daemon, and the tests assert on `agent.state` and `agent.source`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use femux_plugin::run_daemon_plugin;
use femux_plugin_agents::{Agents, Settings};
use femux_proto::msg as m;
use femux_test::{Client, Harness};

struct Rig {
    h: Harness,
    c: Client,
    pane: String,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    bin: tempfile::TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
        self.h.stop();
    }
}

fn settings(process_tier: bool) -> Settings {
    Settings {
        enabled: true,
        blocked_after_ms: 400,
        hold_ms: 1500,
        process_tier,
        tick: Duration::from_millis(100),
    }
}

async fn rig(s: Settings) -> Rig {
    let h = Harness::start();
    let socket = h.socket.clone();
    let task =
        tokio::spawn(async move { run_daemon_plugin(&socket, Box::new(Agents::new(s))).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    // A shell under the name of a known agent: its `comm` is `claude`. macOS's /bin/sh only
    // launches bash (the process would be named `bash`), so take a real shell binary.
    let bin = tempfile::tempdir().unwrap();
    let shell = ["/bin/bash", "/bin/sh"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .unwrap();
    std::fs::copy(shell, bin.path().join("claude")).unwrap();
    Rig {
        h,
        c,
        pane,
        task,
        bin,
    }
}

impl Rig {
    fn claude(&self) -> PathBuf {
        self.bin.path().join("claude")
    }

    async fn meta(&mut self) -> BTreeMap<String, String> {
        self.c
            .call(m::PaneInfo {
                pane_id: self.pane.clone(),
            })
            .await
            .unwrap()
            .meta
    }

    /// Polls until `agent.state` and `agent.source` are `state`/`source`; returns the meta.
    async fn expect(&mut self, state: &str, source: &str) -> BTreeMap<String, String> {
        self.expect_where(state, source, |_| true).await
    }

    /// [`Rig::expect`], also waiting for `also` to hold on the meta.
    async fn expect_where(
        &mut self,
        state: &str,
        source: &str,
        also: impl Fn(&BTreeMap<String, String>) -> bool,
    ) -> BTreeMap<String, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let meta = self.meta().await;
            let got = (
                meta.get("agent.state").map(String::as_str),
                meta.get("agent.source").map(String::as_str),
            );
            if got == (Some(state), Some(source)) && also(&meta) {
                return meta;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "wanted {state}/{source}, have {got:?}: {meta:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn set(&mut self, kv: &[(&str, &str)]) {
        self.c
            .call(m::PaneSetMeta {
                pane_id: self.pane.clone(),
                set: Some(
                    kv.iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                ),
                unset: None,
            })
            .await
            .unwrap();
    }

    async fn run(&mut self, line: &str) {
        let p = self.pane.clone();
        self.c.type_text(&p, &format!("{line}\n")).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn agt_001_a_hook_state_is_tier_one_and_gets_source_and_since() {
    let mut r = rig(settings(false)).await;
    r.set(&[("agent.state", "done")]).await;
    let meta = r.expect("done", "hook").await;
    assert!(meta["agent.since"].parse::<u64>().unwrap() > 0);
    assert!(meta["agent.evidence"].starts_with("hook"));
    // Hooks naming themselves keep their source and name.
    r.set(&[
        ("agent.state", "working"),
        ("agent.source", "hook"),
        ("agent.name", "claude-code"),
    ])
    .await;
    let meta = r.expect("working", "hook").await;
    assert_eq!(meta["agent.name"], "claude-code");
    // Not a state: ignored.
    r.set(&[("agent.state", "thinking")]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.meta().await["agent.state"], "thinking");
    r.set(&[("agent.state", "blocked")]).await;
    r.expect("blocked", "hook").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agt_001_osc_progress_and_private_reports_are_tier_one() {
    let mut r = rig(settings(false)).await;
    r.run(r"printf '\033]9;4;1;40\007'").await;
    r.expect("working", "osc").await;
    r.run(r"printf '\033]7777;state=blocked\033\\'").await;
    r.expect("blocked", "osc").await;
    r.run(r"printf '\033]9;4;0\007'").await;
    r.expect("idle", "osc").await;
    // An unknown state is ignored.
    r.run(r"printf '\033]7777;state=asleep\007'").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(r.meta().await["agent.state"], "idle");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn agt_002_a_process_waiting_on_the_tty_is_blocked_then_exited_then_idle() {
    let mut r = rig(settings(true)).await;
    let claude = r.claude();
    r.run(&format!(
        "{} -c 'echo started; read x; echo got-$x'",
        claude.display()
    ))
    .await;
    let meta = r.expect("blocked", "process").await;
    assert_eq!(meta["agent.name"], "claude");
    assert!(
        meta["agent.evidence"].contains("waits for input"),
        "{meta:?}"
    );
    r.run("yes").await;
    r.expect("exited", "process").await;
    r.expect("idle", "process").await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn agt_004_a_lower_tier_waits_out_the_hold_and_needs_something_new() {
    let mut r = rig(settings(true)).await;
    let claude = r.claude();
    r.run(&format!(
        "{} -c 'read x; echo got-$x; read y'",
        claude.display()
    ))
    .await;
    r.expect("blocked", "process").await;
    // The hook says done: tier 2 must not take it back within hold_ms…
    r.set(&[("agent.state", "done")]).await;
    r.expect("done", "hook").await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(r.meta().await["agent.state"], "done");
    // …nor after it by reading the same quiet process again (0.11.0)…
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let meta = r.meta().await;
    assert_eq!(
        (meta["agent.state"].as_str(), meta["agent.source"].as_str()),
        ("done", "hook")
    );
    // …only once the pane does something new: output, then a new wait.
    r.run("a").await;
    r.expect("blocked", "process").await;
    r.run("").await;
}

/// A Python under the name of a known agent, sleeping in `select` as an event loop does.
#[cfg(target_os = "linux")]
fn python_agent(r: &Rig, screen: &str) -> Option<String> {
    let python = ["/usr/bin/python3", "/bin/python3", "/usr/local/bin/python3"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())?;
    let dir = r.bin.path().join("py");
    std::fs::create_dir_all(&dir).unwrap();
    let agent = dir.join("claude");
    if !agent.exists() {
        std::os::unix::fs::symlink(python, &agent).unwrap();
    }
    let script = dir.join(format!("agent{}.py", screen.len()));
    std::fs::write(
        &script,
        format!(
            "import select, sys\nsys.stdout.write({screen:?})\nsys.stdout.flush()\nselect.select([], [], [], 30)\n"
        ),
    )
    .unwrap();
    Some(format!("{} {}", agent.display(), script.display()))
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn agt_002_an_event_loop_wait_is_read_off_the_screen() {
    let mut r = rig(settings(true)).await;
    // Idle at its prompt: done, not blocked.
    let Some(idle) = python_agent(&r, "Answered.\r\n\r\n> ") else {
        eprintln!("no python3: skipped");
        return;
    };
    r.run(&idle).await;
    let meta = r.expect("done", "process").await;
    assert_eq!(meta["agent.name"], "claude");
    let e = &meta["agent.evidence"];
    // The kernel names select's sleep differently by version (do_select,
    // poll_schedule_timeout): the classification is what counts.
    assert!(e.contains("waits for a prompt"), "{e}");
    r.run("\u{3}").await;
    r.expect("exited", "process").await;
    // A permission prompt: blocked, with the line it rests on.
    let ask = python_agent(&r, "Do you want to proceed?\r\n> 1. Yes\r\n  2. No\r\n").unwrap();
    r.run(&ask).await;
    let meta = r.expect("blocked", "process").await;
    let e = &meta["agent.evidence"];
    assert!(
        e.contains("waits on a question") && e.contains("1. Yes"),
        "{e}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn agt_003_heuristics_judge_silence_after_output_by_the_last_line() {
    let mut r = rig(settings(false)).await;
    let claude = r.claude();
    // Output, then a question and silence: blocked.
    r.run(&format!(
        "{} -c 'for i in 1 2 3 4 5 6; do echo step $i; sleep 0.1; done; printf \"Proceed? (y/n) \"; read x; echo all-done; sleep 3'",
        claude.display()
    ))
    .await;
    r.expect("working", "heuristic").await;
    r.expect("blocked", "heuristic").await;
    // Answer: more output, then silence on a plain line: done.
    // A slow machine can pause long enough between echoing the answer and printing
    // all-done to judge the echo `done` first; the verdict that counts is on all-done.
    r.run("y").await;
    r.expect_where("done", "heuristic", |m| {
        m.get("agent.evidence")
            .is_some_and(|e| e.contains("all-done"))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agt_003_heuristics_stay_quiet_once_tier_one_spoke() {
    let mut r = rig(settings(false)).await;
    let claude = r.claude();
    r.run(&format!(
        "{} -c 'printf \"\\033]7777;state=working\\007\"; echo busy; sleep 2'",
        claude.display()
    ))
    .await;
    r.expect("working", "osc").await;
    // Silence after output would read as done; tier 1 has spoken, so nothing changes.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let meta = r.meta().await;
    assert_eq!(
        (meta["agent.state"].as_str(), meta["agent.source"].as_str()),
        ("working", "osc")
    );
}
