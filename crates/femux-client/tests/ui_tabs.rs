//! REQ-UI-020 (tab strip markers) and REQ-UI-021 (title precedence).

mod common;

use common::Drive;
use femux_client::app::App;
use femux_client::config::ClientConfig;

/// The tabs as the bar shows them: its row after the session chip (`  s  `), trailing blanks
/// trimmed (REQ-UI-030, 0.10.0: the tabs are part of the one bar).
fn strip(d: &mut Drive) -> String {
    let row = d.app.rows as usize - 1;
    d.screen_line(row).chars().skip(5).collect()
}

/// Settles until the tab strip reads what `want` computes from the current model.
async fn wait_strip(d: &mut Drive, want: impl Fn(&App) -> String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let expected = want(&d.app);
        if strip(d) == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "strip {:?} never became {expected:?}",
            strip(d)
        );
        d.settle().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_020_tab_strip_shows_running_urgent_and_zoom() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.until("pane info", |a| a.model.panes[&pane].known).await;
    // The shell's own name: sh here, bash on macOS, where /bin/sh is bash and the name the
    // daemon reports settles shortly after spawn. Read it when asserting, never earlier.
    wait_strip(&mut d, |a| {
        format!(" 1 {}", a.model.tab_title(&a.model.tabs[0]))
    })
    .await;
    // A new tab becomes current; a long-running command marks it.
    d.press_all(&["ctrl+space", "t"]).await;
    d.until("second tab", |a| {
        a.model.tabs.len() == 2 && a.model.current == a.model.tabs[1].id
    })
    .await;
    let p2 = d.app.model.focused_id().unwrap();
    d.wait_pane_text(&p2, "$").await;
    d.until("pane 2 info", |a| a.model.panes[&p2].known).await;
    d.type_text("sleep 30\n").await;
    d.until("running", |a| a.model.panes[&p2].running()).await;
    wait_strip(&mut d, |a| {
        format!(" 1 {}  2 sleep ●", a.model.tab_title(&a.model.tabs[0]))
    })
    .await;
    // Back on tab 1, a bell in tab 2 marks it urgent; selecting it clears the mark.
    d.press_all(&["ctrl+space", "1"]).await;
    d.until("tab 1", |a| a.model.current == a.model.tabs[0].id)
        .await;
    d.obs
        .call(femux_proto::msg::PaneSignal {
            pane_id: p2.clone(),
            sig: femux_proto::msg::Signal::Sigint,
        })
        .await
        .unwrap();
    // Wait for the shell to take the terminal back before typing the bell.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let info = d
            .obs
            .call(femux_proto::msg::PaneInfo {
                pane_id: p2.clone(),
            })
            .await
            .unwrap();
        if info.fg_pid == info.pid {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "sleep never died");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    d.obs.type_text(&p2, "printf '\\a'\n").await;
    d.until("urgent", |a| a.model.tabs[1].urgent).await;
    wait_strip(&mut d, |a| {
        format!(
            " 1 {}  2 {} !",
            a.model.tab_title(&a.model.tabs[0]),
            a.model.tab_title(&a.model.tabs[1])
        )
    })
    .await;
    d.press_all(&["ctrl+space", "n"]).await;
    d.until("tab 2", |a| {
        a.model.current == a.model.tabs[1].id && !a.model.tabs[1].urgent
    })
    .await;
    // Zoom marker.
    d.press_all(&["ctrl+space", "z"]).await;
    d.until("zoomed", |a| a.model.tabs[1].zoomed).await;
    wait_strip(&mut d, |a| {
        format!(
            " 1 {}  2 {} Z",
            a.model.tab_title(&a.model.tabs[0]),
            a.model.tab_title(&a.model.tabs[1])
        )
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_021_title_precedence() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.until("pane info", |a| a.model.panes[&pane].known).await;
    // At the prompt: the shell and its directory.
    wait_strip(&mut d, |a| {
        let p = &a.model.panes[&pane];
        format!(" 1 {} {}", p.fg_cmd, femux_client::model::short_dir(&p.cwd))
    })
    .await;
    assert!(!d.app.model.panes[&pane].fg_cmd.is_empty());
    // A shell's own OSC 2 title does not replace that…
    d.type_text("printf '\\033]2;shell-title\\a'\n").await;
    d.until("osc 2 title", |a| {
        a.model.panes[&pane].title == "shell-title"
    })
    .await;
    d.settle().await;
    assert!(!strip(&mut d).contains("shell-title"), "{}", strip(&mut d));
    // …but a program's does: OSC 2 beats the fg command.
    d.type_text("printf '\\033]2;notes\\a'; sleep 30\n").await;
    d.until("program title", |a| {
        a.model.panes[&pane].title == "notes" && a.model.panes[&pane].running()
    })
    .await;
    wait_strip(&mut d, |_| " 1 notes ●".to_owned()).await;
    // A user rename beats everything.
    d.press_all(&["ctrl+space", "T"]).await;
    d.press_all(&[
        "backspace",
        "backspace",
        "backspace",
        "backspace",
        "backspace",
    ])
    .await;
    d.type_text("work").await;
    d.press("enter").await;
    d.until("renamed", |a| {
        a.model.tabs[0].custom_title.as_deref() == Some("work")
    })
    .await;
    assert_eq!(strip(&mut d), " 1 work ●");
    d.obs
        .call(femux_proto::msg::PaneSignal {
            pane_id: pane.clone(),
            sig: femux_proto::msg::Signal::Sigint,
        })
        .await
        .unwrap();
    d.until("back at the prompt", |a| !a.model.panes[&pane].running())
        .await;
    d.type_text("printf '\\033]2;other\\a'\n").await;
    d.until("osc 2 again", |a| a.model.panes[&pane].title == "other")
        .await;
    assert_eq!(strip(&mut d), " 1 work");
    // A fresh attach gets the rename from the snapshot (custom_title, ADR-016 #2).
    let snap = d.obs.attach("s", 80, 22).await.snapshot;
    assert_eq!(snap.tabs[0].custom_title.as_deref(), Some("work"));
    assert_eq!(snap.tabs[0].title, "work");
}
