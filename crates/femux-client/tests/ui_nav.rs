//! REQ-UI-040 (session navigator), REQ-UI-041 (window navigator), REQ-UI-042 (side nav).

mod common;

use std::time::Instant;

use common::Drive;
use femux_client::app::{NavTarget, Overlay};
use femux_client::config::ClientConfig;
use femux_client::nav::{NavKind, rows};
use femux_proto::msg as m;

/// Ticks the app (directory and preview refresh) and lets the daemon answer.
async fn pump(d: &mut Drive) {
    d.app.tick(Instant::now());
    d.settle().await;
}

fn nav_rows(d: &Drive) -> Vec<NavTarget> {
    let Overlay::Navigator(nav) = &d.app.overlay else {
        panic!("no navigator: {:?}", d.app.overlay)
    };
    rows(nav, &d.app.dir, &d.app.model.session)
        .into_iter()
        .map(|r| r.target)
        .collect()
}

/// Moves the selection to `target` with j/k.
async fn select(d: &mut Drive, target: &NavTarget) {
    let at = nav_rows(d)
        .iter()
        .position(|t| t == target)
        .expect("row listed");
    let Overlay::Navigator(nav) = &d.app.overlay else {
        panic!()
    };
    let now = nav.selected;
    let key = if at > now { "j" } else { "k" };
    for _ in 0..at.abs_diff(now) {
        d.press(key).await;
    }
}

async fn sessions(d: &mut Drive) -> Vec<String> {
    d.obs
        .call(m::SessionList {})
        .await
        .unwrap()
        .sessions
        .into_iter()
        .map(|s| s.name)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_040_session_navigator_lists_attaches_renames_creates_and_kills() {
    let mut d = Drive::with(ClientConfig::default(), vec![], 120, 30).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.obs
        .call(m::SessionCreate {
            name: "other".into(),
            root: None,
            file: None,
        })
        .await
        .unwrap();
    d.obs
        .call(m::TabCreate {
            session: "other".into(),
            cwd: None,
            cmd: None,
            title: Some("second".into()),
            index: None,
            restore: None,
        })
        .await
        .unwrap();
    d.press_all(&["ctrl+space", "s"]).await;
    pump(&mut d).await;
    let screen = d.screen();
    for want in [
        "Sessions",
        "2 sessions",
        "session s",
        "1 tab · attached",
        "session other",
        "2 tabs",
        "last activity",
        "SESSIONS",
    ] {
        assert!(screen.contains(want), "{want}:\n{screen}");
    }
    // Sessions only, until tab expands the selected one to its tabs.
    assert_eq!(nav_rows(&d).len(), 2);
    assert!(!d.screen().contains("tab 1"), "{}", d.screen());
    d.press("tab").await;
    assert!(nav_rows(&d).len() > 2);
    assert!(d.screen().contains("tab 1"), "{}", d.screen());
    d.press("tab").await;
    assert_eq!(nav_rows(&d).len(), 2);
    // Filter, then attach to the other session.
    d.press("/").await;
    d.type_text("oth").await;
    d.press("enter").await;
    assert_eq!(nav_rows(&d)[0], NavTarget::Session("other".into()));
    d.press("enter").await;
    d.until("attached to other", |a| a.model.session == "other")
        .await;
    d.until("its two tabs", |a| a.model.tabs.len() == 2).await;
    assert!(matches!(d.app.overlay, Overlay::None));
    // Rename the attached session from the navigator.
    d.press_all(&["ctrl+space", "s"]).await;
    pump(&mut d).await;
    select(&mut d, &NavTarget::Session("other".into())).await;
    d.press("r").await;
    for _ in 0.."other".len() {
        d.press("backspace").await;
    }
    d.type_text("renamed").await;
    d.press("enter").await;
    pump(&mut d).await;
    assert!(
        matches!(d.app.overlay, Overlay::Navigator(_)),
        "back in the navigator"
    );
    assert_eq!(sessions(&mut d).await, ["renamed", "s"]);
    assert_eq!(d.app.model.session, "renamed");
    // n creates, x kills (after a confirmation).
    d.press("n").await;
    d.type_text("fresh").await;
    d.press("enter").await;
    pump(&mut d).await;
    assert!(sessions(&mut d).await.contains(&"fresh".to_owned()));
    select(&mut d, &NavTarget::Session("fresh".into())).await;
    d.press("x").await;
    assert!(d.screen().contains("kill session fresh? y/n"));
    d.press("y").await;
    pump(&mut d).await;
    assert!(!sessions(&mut d).await.contains(&"fresh".to_owned()));
    // q closes.
    d.press("q").await;
    assert!(matches!(d.app.overlay, Overlay::None));
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_040_prefix_shift_s_names_and_starts_a_session() {
    let mut d = Drive::with(ClientConfig::default(), vec![], 120, 30).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    // esc cancels: nothing is created.
    d.press_all(&["ctrl+space", "S"]).await;
    let Overlay::Prompt { title, .. } = &d.app.overlay else {
        panic!("no prompt: {:?}", d.app.overlay)
    };
    assert_eq!(title, "new session");
    d.press("escape").await;
    assert!(matches!(d.app.overlay, Overlay::None));
    assert_eq!(sessions(&mut d).await, ["s"]);
    // A taken name is refused and named in the status line; the client stays put.
    d.press_all(&["ctrl+space", "S"]).await;
    let Overlay::Prompt { input, .. } = &d.app.overlay else {
        panic!("no prompt")
    };
    for _ in 0..input.chars().count() {
        d.press("backspace").await;
    }
    d.type_text("s").await;
    d.press("enter").await;
    pump(&mut d).await;
    assert!(
        d.app
            .message
            .as_deref()
            .is_some_and(|m| m.contains("exists")),
        "{:?}",
        d.app.message
    );
    assert_eq!(d.app.model.session, "s");
    // A new name creates the session and switches to it.
    d.press_all(&["ctrl+space", "S"]).await;
    let Overlay::Prompt { input, .. } = &d.app.overlay else {
        panic!("no prompt")
    };
    for _ in 0..input.chars().count() {
        d.press("backspace").await;
    }
    d.type_text("work").await;
    d.press("enter").await;
    d.until("attached to work", |a| a.model.session == "work")
        .await;
    assert!(sessions(&mut d).await.contains(&"work".to_owned()));
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_041_pane_navigator_previews_panes_live_and_goes_to_them() {
    let mut d = Drive::with(ClientConfig::default(), vec![], 120, 30).await;
    let first = d.pane.clone();
    d.wait_pane_text(&first, "$").await;
    let t2 = d
        .obs
        .call(m::TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: None,
            title: Some("work".into()),
            index: None,
            restore: None,
        })
        .await
        .unwrap();
    d.obs
        .type_text(&t2.pane_id, "echo preview-$((6*7))\n")
        .await;
    d.press_all(&["ctrl+space", "w"]).await;
    pump(&mut d).await;
    let target = NavTarget::Pane {
        session: "s".into(),
        tab: t2.tab_id.clone(),
        pane: t2.pane_id.clone(),
    };
    let listed = nav_rows(&d);
    assert!(listed.contains(&target), "{listed:#?}");
    assert!(listed.contains(&NavTarget::Pane {
        session: "s".into(),
        tab: d.tab.clone(),
        pane: first.clone()
    }));
    select(&mut d, &target).await;
    // The preview follows the pane's screen.
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while !d.screen().contains("preview-42") {
        assert!(Instant::now() < deadline, "{}", d.screen());
        pump(&mut d).await;
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(matches!(&d.app.overlay, Overlay::Navigator(n) if n.kind == NavKind::Panes));
    let screen = d.screen();
    assert!(
        screen.contains("Panes") && screen.contains("PANES"),
        "{screen}"
    );
    d.press("enter").await;
    d.until("on that tab and pane", |a| {
        a.model.current == t2.tab_id && a.model.focused_id().as_deref() == Some(t2.pane_id.as_str())
    })
    .await;
}
