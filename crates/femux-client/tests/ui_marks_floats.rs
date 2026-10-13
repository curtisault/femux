//! 0.8.0 client UI: the marks overlay (REQ-SESS-004), scratch floats with hide and cycle
//! (REQ-UI-011), broadcast typing and the record toggle (ADR-022).

mod common;

use common::Drive;
use femux_client::app::Overlay;
use femux_client::bindings::Action;
use femux_client::config::ClientConfig;
use femux_proto::msg as m;

async fn split(d: &mut Drive) -> String {
    let before: Vec<String> = d.app.model.panes.keys().cloned().collect();
    d.press_all(&["ctrl+space", "v"]).await;
    d.until("a second pane", |a| a.model.panes.len() == before.len() + 1)
        .await;
    d.app
        .model
        .panes
        .keys()
        .find(|p| !before.contains(p))
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn sess_004_marks_overlay_lists_jumps_and_deletes() {
    let mut d = Drive::with(ClientConfig::default(), vec![], 100, 30).await;
    let a = d.pane.clone();
    d.wait_pane_text(&a, "$").await;
    d.press_all(&["ctrl+space", "m", "a"]).await;
    let b = split(&mut d).await;
    d.until("b focused", |x| {
        x.model.focused_id().as_deref() == Some(b.as_str())
    })
    .await;
    d.press_all(&["ctrl+space", "m", "b"]).await;
    d.until("two marks", |x| x.model.marks.len() == 2).await;
    d.press_all(&["ctrl+space", "M"]).await;
    assert!(matches!(d.app.overlay, Overlay::Marks(0)));
    let rows = d.app.mark_rows();
    assert_eq!(
        rows.iter()
            .map(|r| (r.0.as_str(), r.1.as_str()))
            .collect::<Vec<_>>(),
        [("a", a.as_str()), ("b", b.as_str())]
    );
    let screen = d.screen();
    assert!(
        screen.contains("marks") && screen.contains("tab 1"),
        "{screen}"
    );
    // The letter jumps.
    d.press("a").await;
    d.until("on a", |x| {
        x.model.focused_id().as_deref() == Some(a.as_str())
    })
    .await;
    assert!(matches!(d.app.overlay, Overlay::None));
    // x deletes the selected mark.
    d.press_all(&["ctrl+space", "M", "j", "x"]).await;
    d.until("b's mark gone", |x| !x.model.marks.contains_key("b"))
        .await;
    assert!(d.app.model.marks.contains_key("a"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_011_scratch_float_hides_shows_and_cycles() {
    let mut d = Drive::with(ClientConfig::default(), vec![], 100, 30).await;
    let a = d.pane.clone();
    d.wait_pane_text(&a, "$").await;
    // prefix F with no floats: a new floating shell, focused.
    d.press_all(&["ctrl+space", "F"]).await;
    d.until("a float", |x| {
        x.model
            .current_tab()
            .is_some_and(|t| t.floats.len() == 1 && t.focus == t.floats[0].id)
    })
    .await;
    let f = d.app.model.current_tab().unwrap().floats[0].id.clone();
    d.wait_pane_text(&f, "$").await;
    // prefix F again: hidden (still running), focus back on the tiled pane.
    d.press_all(&["ctrl+space", "F"]).await;
    d.until("floats hidden", |x| {
        x.model
            .current_tab()
            .is_some_and(|t| t.floats_hidden && t.focus != f)
    })
    .await;
    d.until("float off screen", |x| {
        x.model.panes.get(&f).is_some_and(|p| p.screen.is_none())
    })
    .await;
    // prefix g shows them and focuses the float.
    d.press_all(&["ctrl+space", "g"]).await;
    d.until("float back and focused", |x| {
        x.model
            .current_tab()
            .is_some_and(|t| !t.floats_hidden && t.focus == f)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn broadcast_and_record_toggles() {
    let mut d = Drive::with(ClientConfig::default(), vec![], 100, 30).await;
    let a = d.pane.clone();
    d.wait_pane_text(&a, "$").await;
    let b = split(&mut d).await;
    d.wait_pane_text(&b, "$").await;
    d.app.run(Action::Broadcast);
    d.until("broadcast on", |x| {
        x.model.current_tab().is_some_and(|t| t.broadcast)
    })
    .await;
    assert!(d.screen().contains("BROADCAST"), "{}", d.screen());
    d.type_text("echo bc-$((6*7))\n").await;
    d.wait_pane_text(&a, "bc-42").await;
    d.wait_pane_text(&b, "bc-42").await;
    d.app.run(Action::Broadcast);
    d.until("broadcast off", |x| {
        x.model.current_tab().is_some_and(|t| !t.broadcast)
    })
    .await;
    // Record marks the focused pane for the recorder plugin.
    d.app.run(Action::Record);
    d.settle().await;
    let info = d
        .obs
        .call(m::PaneInfo {
            pane_id: d.app.model.focused_id().unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(info.meta.get("recorder.on").map(String::as_str), Some("1"));
}
