//! REQ-UI-046 (0.11.0): a right click on a pane or a tab opens a menu of actions for it at the
//! pointer; panes whose program tracks the mouse, or told to with `send right-clicks`, get
//! the click, and shift opens the menu anyway.

mod common;

use std::time::Duration;

use common::Drive;
use femux_client::app::{MouseKind, Overlay};
use femux_client::config::ClientConfig;
use femux_proto::msg as m;

fn right(d: &mut Drive, x: u16, y: u16, shift: bool) {
    let mods = if shift { vec![m::Mod::Shift] } else { vec![] };
    d.app
        .on_mouse(x, y, MouseKind::Press(m::MouseButton::Right), mods.clone());
    d.app.on_mouse(x, y, MouseKind::Release, mods);
}

fn labels(d: &Drive) -> Vec<String> {
    match &d.app.overlay {
        Overlay::Menu(st) => st.items.iter().map(|i| i.label.clone()).collect(),
        o => panic!("no menu: {o:?}"),
    }
}

fn panes(d: &Drive) -> usize {
    d.app.model.panes.len()
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_046_a_right_click_on_a_pane_opens_its_menu() {
    let mut d = Drive::with(ClientConfig::default(), Vec::new(), 100, 30).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    right(&mut d, 10, 5, false);
    d.settle().await;
    let l = labels(&d);
    assert_eq!(l[0], "split right");
    assert_eq!(l.len(), 8);
    // At the pointer, the first item highlighted.
    let screen = d.screen();
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(rows[5].find('┌'), Some(10), "{screen}");
    assert!(
        rows[6].contains("split right") && rows[13].contains("close pane"),
        "{screen}"
    );
    // j/k move, esc closes.
    d.press("j").await;
    match &d.app.overlay {
        Overlay::Menu(st) => assert_eq!(st.selected, 1),
        o => panic!("{o:?}"),
    }
    d.press("escape").await;
    assert!(matches!(d.app.overlay, Overlay::None));
    // Hover highlights; a click runs the item: split right.
    right(&mut d, 10, 5, false);
    d.app.on_mouse(12, 6, MouseKind::Move, vec![]);
    d.app
        .on_mouse(12, 6, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.until("a second pane", |a| a.model.panes.len() == 2).await;
    assert!(matches!(d.app.overlay, Overlay::None));
    // A click elsewhere only closes it.
    right(&mut d, 10, 5, false);
    d.app
        .on_mouse(60, 20, MouseKind::Press(m::MouseButton::Left), vec![]);
    assert!(matches!(d.app.overlay, Overlay::None));
    d.settle().await;
    assert_eq!(panes(&d), 2);
    // Zoom from the menu, then the item says unzoom.
    right(&mut d, 2, 2, false);
    d.settle().await;
    d.press_all(&["j", "j", "enter"]).await;
    d.until("zoomed", |a| {
        a.model.current_tab().is_some_and(|t| t.zoomed)
    })
    .await;
    right(&mut d, 2, 2, false);
    assert_eq!(labels(&d)[2], "unzoom");
    d.press("escape").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_046_right_clicks_go_to_programs_that_track_the_mouse_or_were_given_them() {
    let mut d = Drive::with(ClientConfig::default(), Vec::new(), 100, 30).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    // Give this pane right clicks from the menu.
    right(&mut d, 10, 5, false);
    let i = labels(&d)
        .iter()
        .position(|l| l == "send right-clicks to this pane")
        .unwrap();
    for _ in 0..i {
        d.press("j").await;
    }
    d.press("enter").await;
    assert!(d.app.right_clicks.contains(&pane));
    d.sent_kinds();
    right(&mut d, 10, 5, false);
    assert!(matches!(d.app.overlay, Overlay::None), "the pane got it");
    d.settle().await;
    assert!(d.sent_kinds().contains(&"pane.input"));
    // Shift opens the menu anyway; its item now turns the menu back on.
    right(&mut d, 10, 5, true);
    assert!(labels(&d).contains(&"open the menu on right-click".to_owned()));
    d.press("escape").await;
    d.app.right_clicks.clear();
    // A program that tracks the mouse gets the right click.
    d.type_text("printf '\\033[?1000h'; sleep 30\n").await;
    d.until("mouse tracking", |a| {
        a.model.panes[&pane]
            .screen
            .as_ref()
            .is_some_and(|s| s.mouse)
    })
    .await;
    right(&mut d, 10, 5, false);
    assert!(matches!(d.app.overlay, Overlay::None));
    right(&mut d, 10, 5, true);
    assert!(matches!(d.app.overlay, Overlay::Menu(_)));
    d.press("escape").await;
    d.press("ctrl+c").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_046_a_right_click_on_a_tab_opens_the_tab_menu() {
    let mut d = Drive::with(ClientConfig::default(), Vec::new(), 100, 30).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.screen();
    let y = d.app.layout().bar.y;
    let (x, _, _) = d.app.tab_hits[0].clone();
    right(&mut d, x, y, false);
    assert_eq!(labels(&d), ["new tab", "rename tab", "close tab"]);
    // The menu stays whole above the bottom bar's row.
    match &d.app.overlay {
        Overlay::Menu(st) => assert!(st.y + st.size().1 <= 30),
        o => panic!("{o:?}"),
    }
    d.press("enter").await;
    d.until("a second tab", |a| a.model.tabs.len() == 2).await;
    // Close the new tab with a program running in it: it asks first.
    d.type_text("sleep 30\n").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    d.settle().await;
    d.screen();
    let current = d.app.model.current.clone();
    let (x, _, _) = d
        .app
        .tab_hits
        .iter()
        .find(|(_, _, t)| *t == current)
        .unwrap()
        .clone();
    right(&mut d, x, y, false);
    d.settle().await;
    d.press_all(&["j", "j", "enter"]).await;
    assert!(
        matches!(&d.app.overlay, Overlay::Confirm { text, .. } if text.contains("close tab")),
        "{:?}",
        d.app.overlay
    );
    d.press("y").await;
    d.until("one tab left", |a| a.model.tabs.len() == 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_046_mouse_off_means_no_menu() {
    let cfg = ClientConfig {
        mouse: false,
        ..Default::default()
    };
    let mut d = Drive::new(cfg).await;
    right(&mut d, 10, 5, false);
    assert!(matches!(d.app.overlay, Overlay::None));
}
