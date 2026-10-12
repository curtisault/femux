//! REQ-PTY-*: pane processes, foreground tracking, signals, ids and environment.

use std::time::{Duration, Instant};

use femux_proto::msg::*;
use femux_test::{Client, Harness};

async fn session(h: &Harness, name: &str) -> (Client, String, String) {
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: name.into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let att = c.attach(name, 80, 24).await;
    let pane = att.snapshot.tabs[0].focus.clone();
    c.wait_text(&pane, "$").await;
    (c, att.view_id, pane)
}

async fn info(c: &mut Client, pane: &str) -> PaneInfoOk {
    c.call(PaneInfo {
        pane_id: pane.into(),
    })
    .await
    .unwrap()
}

#[cfg(target_os = "linux")]
fn stat(pid: u32) -> Vec<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    // Fields after "(comm)": state ppid pgrp session tty_nr tpgid ...
    s[s.rfind(')').unwrap() + 2..]
        .split(' ')
        .map(String::from)
        .collect()
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn pty_001_own_session_and_group() {
    let h = Harness::start();
    let (mut c, _, pane) = session(&h, "s").await;
    let pid = info(&mut c, &pane).await.fg_pid;
    let f = stat(pid);
    assert_eq!(f[2], pid.to_string(), "process group leader");
    assert_eq!(f[3], pid.to_string(), "session leader");
    assert_ne!(f[4], "0", "has a controlling terminal");
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_002_foreground_process_and_cwd() {
    let h = Harness::start();
    let (mut c, _, pane) = session(&h, "s").await;
    c.call(EventsSubscribe {
        kinds: Some(vec!["pane.fg_changed".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    let shell = info(&mut c, &pane).await;
    // The shell itself (named "sh", or "bash" on macOS where /bin/sh execs it).
    assert!(!shell.fg_cmd.is_empty() && shell.fg_cmd != "sleep");
    std::fs::create_dir(h.dir.path().join("sub")).unwrap();
    c.type_text(&pane, "cd sub && sleep 30\n").await;
    match c.wait_event("pane.fg_changed").await {
        femux_proto::Event::PaneFgChanged(e) => assert_eq!(e.cmd, "sleep"),
        e => panic!("{e:?}"),
    }
    let now = info(&mut c, &pane).await;
    assert_eq!(now.fg_cmd, "sleep");
    assert_ne!(now.fg_pid, shell.fg_pid);
    assert!(now.cwd.ends_with("/sub"), "{}", now.cwd);
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_003_busy_pane_does_not_block_others() {
    let h = Harness::start();
    let (mut c, _, a) = session(&h, "s").await;
    let b = c
        .call(LayoutSplit {
            pane_id: a.clone(),
            dir: SplitDir::V,
            ratio: None,
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    c.wait_text(&b, "$").await;
    c.type_text(&a, "yes flood\n").await;
    c.wait_text(&a, "flood").await;
    let t = Instant::now();
    c.type_text(&b, "echo quiet-marker\n").await;
    c.wait_text(&b, "quiet-marker").await;
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    c.call(PaneSignal {
        pane_id: a,
        sig: Signal::Sigint,
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_004_signals() {
    let h = Harness::start();
    let (mut c, view, pane) = session(&h, "s").await;
    // Compare by pid: on macOS /bin/sh execs bash, so the shell's name is not "sh".
    let shell = info(&mut c, &pane).await.fg_pid;
    for sig in [
        Signal::Sigint,
        Signal::Sigterm,
        Signal::Sigkill,
        Signal::Sighup,
    ] {
        c.type_text(&pane, "sleep 100\n").await;
        let p = pane.clone();
        c.wait_for("sleep in fg", Duration::from_secs(5), |_| true)
            .await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while info(&mut c, &pane).await.fg_cmd != "sleep" {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        c.call(PaneSignal {
            pane_id: p.clone(),
            sig,
        })
        .await
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while info(&mut c, &pane).await.fg_pid != shell {
            assert!(Instant::now() < deadline, "{sig:?} did not stop sleep");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    // SIGWINCH reaches the foreground program.
    let t = c
        .call(TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: Some("trap 'echo got-winch' WINCH; while :; do sleep 0.1; done".into()),
            title: None,
            index: None,
        })
        .await
        .unwrap();
    c.call(TabSelect {
        view_id: Some(view),
        tab_id: t.tab_id,
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    c.call(PaneSignal {
        pane_id: t.pane_id.clone(),
        sig: Signal::Sigwinch,
    })
    .await
    .unwrap();
    c.wait_text(&t.pane_id, "got-winch").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_005_pane_ids_are_ulids() {
    let h = Harness::start();
    let (mut c, _, pane) = session(&h, "s").await;
    let b = c
        .call(LayoutSplit {
            pane_id: pane.clone(),
            dir: SplitDir::H,
            ratio: None,
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    for id in [&pane, &b] {
        assert_eq!(id.len(), 26);
        assert!(
            id.chars()
                .all(|ch| ch.is_ascii_digit() || (ch.is_ascii_uppercase() && !"ILOU".contains(ch))),
            "{id}"
        );
    }
    assert!(pane < b, "ULIDs sort by creation time");
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_006_live_environment_is_unfiltered() {
    // Persistence filtering is unit-tested (femux-daemon `pty::tests::env_filter_matches_names_and_prefixes`);
    // the live environment keeps variables outside `[env] allow`.
    let h = Harness::start_with(|c| c.env.push(("AWS_SECRET_TOKEN".into(), "live-only".into())));
    let (mut c, _, pane) = session(&h, "s").await;
    c.type_text(&pane, "echo token=$AWS_SECRET_TOKEN\n").await;
    c.wait_text(&pane, "token=live-only").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_007_pane_environment() {
    let h = Harness::start();
    let (mut c, _, pane) = session(&h, "s").await;
    c.type_text(&pane, "echo \"T=$TERM P=$TERM_PROGRAM V=$TERM_PROGRAM_VERSION\"; echo \"F=$FEMUX_PANE\"; echo \"S=$FEMUX_SOCKET\"\n").await;
    c.wait_text(&pane, "S=/").await;
    let text = c.screen_text(&pane);
    assert!(
        text.contains(&format!(
            "T=xterm-256color P=femux V={}",
            env!("CARGO_PKG_VERSION")
        )),
        "{text}"
    );
    assert!(text.contains(&format!("F={pane}")), "{text}");
    assert!(
        text.contains(&format!("S={}", h.socket.display())),
        "{text}"
    );

    let h = Harness::start_with(|c| c.term = "femux-direct".into());
    let (mut c, _, pane) = session(&h, "s").await;
    c.type_text(&pane, "echo T=$TERM\n").await;
    c.wait_text(&pane, "T=femux-direct").await;
}
