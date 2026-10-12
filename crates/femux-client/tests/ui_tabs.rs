//! REQ-UI-020 (tab strip markers) and REQ-UI-021 (title precedence).

mod common;

use common::Drive;
use femux_client::config::ClientConfig;

#[tokio::test(flavor = "multi_thread")]
async fn ui_020_tab_strip_shows_running_urgent_and_zoom() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.until("pane info", |a| a.model.panes[&pane].known).await;
    assert_eq!(d.screen_line(0), " 1 sh");
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
    assert_eq!(d.screen_line(0), " 1 sh   2 sleep ●");
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
    assert!(d.screen_line(0).contains("2 sh !"), "{}", d.screen_line(0));
    d.press_all(&["ctrl+space", "n"]).await;
    d.until("tab 2", |a| {
        a.model.current == a.model.tabs[1].id && !a.model.tabs[1].urgent
    })
    .await;
    // Zoom marker.
    d.press_all(&["ctrl+space", "z"]).await;
    d.until("zoomed", |a| a.model.tabs[1].zoomed).await;
    assert!(d.screen_line(0).ends_with("2 sh Z"), "{}", d.screen_line(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_021_title_precedence() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.until("pane info", |a| a.model.panes[&pane].known).await;
    // fg command (the shell) beats the cwd basename.
    assert_eq!(d.screen_line(0), " 1 sh");
    // OSC 2 beats the fg command.
    d.type_text("printf '\\033]2;notes\\a'\n").await;
    d.until("osc 2 title", |a| a.model.panes[&pane].title == "notes")
        .await;
    assert_eq!(d.screen_line(0), " 1 notes");
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
    assert_eq!(d.screen_line(0), " 1 work");
    d.type_text("printf '\\033]2;other\\a'\n").await;
    d.until("osc 2 again", |a| a.model.panes[&pane].title == "other")
        .await;
    assert_eq!(d.screen_line(0), " 1 work");
    // A fresh attach gets the rename from the snapshot (custom_title, ADR-016 #2).
    let snap = d.obs.attach("s", 80, 22).await.snapshot;
    assert_eq!(snap.tabs[0].custom_title.as_deref(), Some("work"));
    assert_eq!(snap.tabs[0].title, "work");
}
