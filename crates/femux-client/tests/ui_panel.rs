//! REQ-UI-042 (0.10.0): the panel. `prefix b` shows it, `prefix ctrl+b` focuses it; its
//! `workspace` and `agents` sections fold; `enter` and clicks go where a row points; blocked
//! agents answer in place; the right edge drags.

mod common;

use common::Drive;
use femux_client::app::{MouseKind, Overlay};
use femux_client::config::ClientConfig;
use femux_client::panel::{RowKind, rows};
use femux_proto::Request;
use femux_proto::msg as m;

async fn pump(d: &mut Drive) {
    d.app.refresh_dir();
    for _ in 0..4 {
        d.settle().await;
    }
}

/// The index of the first row matching `pred`.
fn row(d: &Drive, pred: impl Fn(&RowKind) -> bool) -> usize {
    rows(&d.app)
        .iter()
        .position(|r| pred(&r.kind))
        .unwrap_or_else(|| {
            panic!(
                "no such row: {:#?}",
                rows(&d.app).iter().map(|r| &r.kind).collect::<Vec<_>>()
            )
        })
}

/// Moves the focused panel's selection to row `i` with j/k.
async fn select(d: &mut Drive, i: usize) {
    while d.app.panel.selected < i {
        d.press("j").await;
    }
    while d.app.panel.selected > i {
        d.press("k").await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_042_the_panel_shows_folds_focuses_and_resizes() {
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
    d.press_all(&["ctrl+space", "t"]).await;
    d.until("two tabs", |a| a.model.tabs.len() == 2).await;

    // prefix b: the panel opens at 28 columns and the daemon resizes the panes.
    d.press_all(&["ctrl+space", "b"]).await;
    pump(&mut d).await;
    assert!(d.app.panel.open && !d.app.panel.focused);
    assert_eq!(d.app.layout().panes.x, 28);
    d.until("pane resized", |a| {
        a.model
            .focused()
            .and_then(|p| p.screen.as_ref())
            .is_some_and(|s| s.rect.w == 120 - 28)
    })
    .await;
    pump(&mut d).await;
    let screen = d.screen();
    let titles: Vec<String> = d
        .app
        .model
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{} {}", i + 1, d.app.model.tab_title(t)))
        .collect();
    // The attached session open to its tabs and panes, the other closed, both sections open.
    for want in ["▾ workspace", "▾ s", "▸ other", "▾ agents", "$ "]
        .into_iter()
        .chain(titles.iter().map(String::as_str))
    {
        assert!(screen.contains(want), "{want}:\n{screen}");
    }
    // Column 27 is the panel's edge on every row above the bar (`┇` is its drag handle).
    for line in screen.lines().take(29) {
        assert!(
            matches!(line.chars().nth(27), Some('│' | '┇')),
            "{line:?}\n{screen}"
        );
    }

    // A blocked agent in the other tab's pane.
    let second = d.app.model.tabs[1].panes()[0].clone();
    let since = (femux_client::nav::now_ms() - 65_000).to_string();
    d.obs
        .call(m::PaneSetMeta {
            pane_id: second.clone(),
            set: Some(
                [
                    ("agent.state", "blocked"),
                    ("agent.name", "codex"),
                    ("agent.since", since.as_str()),
                ]
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .into(),
            ),
            unset: None,
        })
        .await
        .unwrap();
    // The agents section reads the session directory, refreshed while the panel is open.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !rows(&d.app)
        .iter()
        .any(|r| matches!(&r.kind, RowKind::Agent { blocked: true, .. }))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "no agent row:\n{}",
            d.screen()
        );
        pump(&mut d).await;
    }
    let screen = d.screen();
    // Glyph, name, how long it has waited, and the answer it takes.
    assert!(
        screen.contains("■ codex  1m0") && screen.contains("[y] [n]"),
        "{screen}"
    );

    // prefix ctrl+b: the panel takes the keys and says so.
    d.press_all(&["ctrl+space", "ctrl+b"]).await;
    assert!(d.app.panel.focused);
    assert!(d.screen().contains("PANEL"));
    // y on the blocked agent answers it: the letter, then enter, into its pane.
    let agent = row(&d, |k| matches!(k, RowKind::Agent { .. }));
    select(&mut d, agent).await;
    d.sent.clear();
    d.press("y").await;
    let answered = d.sent.iter().any(|r| {
        matches!(r, Request::PaneInput(i) if i.pane_id == second
            && matches!(i.events.first(), Some(m::InputEvent::KeyEvent(k)) if k.key == "y"))
    });
    assert!(answered, "{:?}", d.sent);
    // tab jumps between section headers; h folds one, its header then sums it up.
    d.press("tab").await;
    assert!(matches!(
        rows(&d.app)[d.app.panel.selected].kind,
        RowKind::Header(ref s) if s == "workspace"
    ));
    d.press("tab").await;
    assert!(matches!(
        rows(&d.app)[d.app.panel.selected].kind,
        RowKind::Header(ref s) if s == "agents"
    ));
    d.press("h").await;
    assert!(d.app.panel.collapsed.contains("agents"));
    let screen = d.screen();
    assert!(
        screen.contains("▸ agents") && screen.contains("■1"),
        "{screen}"
    );
    d.press("l").await;
    assert!(!d.app.panel.collapsed.contains("agents"));
    // h/l on a session folds it.
    let other = row(&d, |k| matches!(k, RowKind::Session(s) if s == "other"));
    select(&mut d, other).await;
    d.press("l").await;
    assert!(d.app.panel.toggled.contains("other"));
    d.press("h").await;
    assert!(!d.app.panel.toggled.contains("other"));
    // enter on the first tab selects it and gives the keys back.
    let first = d.app.model.tabs[0].id.clone();
    let t1 = row(
        &d,
        |k| matches!(k, RowKind::Tab { tab, .. } if *tab == first),
    );
    select(&mut d, t1).await;
    d.press("enter").await;
    d.until("tab 1", |a| a.model.current == first).await;
    assert!(!d.app.panel.focused);
    // esc also gives them back.
    d.press_all(&["ctrl+space", "ctrl+b"]).await;
    d.press("escape").await;
    assert!(!d.app.panel.focused && matches!(d.app.overlay, Overlay::None));

    // A click acts without focusing: the other session.
    let _ = d.screen();
    let i = row(&d, |k| matches!(k, RowKind::Session(s) if s == "other"));
    let y = d.app.panel.hits.iter().find(|(_, r)| *r == i).unwrap().0;
    d.app
        .on_mouse(5, y, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.settle().await;
    d.until("other session", |a| a.model.session == "other")
        .await;
    assert!(!d.app.panel.focused);
    // Dragging the right edge resizes it, never below 16 or past half the terminal.
    let _ = d.screen();
    d.app
        .on_mouse(27, 10, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(39, 10, MouseKind::Move, vec![]);
    d.app.on_mouse(39, 10, MouseKind::Release, vec![]);
    assert_eq!(d.app.panel.width, 40);
    assert_eq!(d.app.layout().panes.x, 40);
    d.app
        .on_mouse(39, 10, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(2, 10, MouseKind::Move, vec![]);
    d.app.on_mouse(2, 10, MouseKind::Release, vec![]);
    assert_eq!(d.app.panel.width, 16);
    // prefix b hides it; the panes get the width back.
    d.press_all(&["ctrl+space", "b"]).await;
    assert!(!d.app.panel.open);
    d.until("full width", |a| {
        a.model
            .focused()
            .and_then(|p| p.screen.as_ref())
            .is_some_and(|s| s.rect.w == 120)
    })
    .await;
}

/// `[panel] open = true`: a new client starts with it, at the configured width.
#[tokio::test(flavor = "multi_thread")]
async fn ui_042_panel_config_opens_it_at_its_width() {
    let cfg = ClientConfig {
        panel_open: true,
        panel_width: 32,
        panel_sections: vec!["agents".into()],
        ..Default::default()
    };
    let mut d = Drive::with(cfg, vec![], 120, 30).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    assert!(d.app.panel.open);
    assert_eq!(d.app.layout().panes.x, 32);
    d.until("pane sized", |a| {
        a.model
            .panes
            .get(&pane)
            .and_then(|p| p.screen.as_ref())
            .is_some_and(|s| s.rect.w == 120 - 32)
    })
    .await;
    let screen = d.screen();
    assert!(
        screen.contains("▾ agents") && !screen.contains("workspace"),
        "{screen}"
    );
}
