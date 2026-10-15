//! REQ-UI-045 (0.11.0): help is a grouped, scrolling, filterable box over the panes, at four
//! fifths of the height, closed by `q`, `esc` or `?` and by a click outside it.

mod common;

use common::Drive;
use femux_client::app::{MouseKind, Overlay};
use femux_client::config::ClientConfig;
use femux_proto::msg as m;

fn help_state(d: &Drive) -> femux_client::help::HelpState {
    match &d.app.overlay {
        Overlay::Help(st) => st.clone(),
        o => panic!("help is not open: {o:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_045_help_scrolls_filters_and_closes() {
    let mut d = Drive::with(ClientConfig::default(), Vec::new(), 120, 40).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.press_all(&["ctrl+space", "?"]).await;
    let screen = d.screen();
    assert!(screen.contains("help  prefix is ctrl+space"), "{screen}");
    assert!(
        screen.contains("j/k scroll · / filter · q close"),
        "{screen}"
    );
    assert!(screen.contains("how the prefix works"), "{screen}");
    assert!(screen.contains("h j k l"), "{screen}");
    assert!(
        screen.contains("focus the pane in that direction"),
        "{screen}"
    );
    // Four fifths of the pane area's height: the box's corners are 31 rows apart.
    let rows: Vec<&str> = screen.lines().collect();
    let top = rows.iter().position(|l| l.contains('╭')).unwrap();
    let bottom = rows.iter().position(|l| l.contains('╰')).unwrap();
    assert_eq!(bottom - top + 1, 39 * 4 / 5, "{screen}");
    assert!(
        rows[top + 1].contains("1–"),
        "the position: {}",
        rows[top + 1]
    );

    // Keys scroll; the bar and the panes stay where they were.
    d.press("j").await;
    assert_eq!(help_state(&d).scroll, 1);
    d.press("G").await;
    let end = help_state(&d).scroll;
    assert!(end > 1);
    d.press("g").await;
    assert_eq!(help_state(&d).scroll, 0);
    // The wheel over the box scrolls.
    d.app.on_mouse(60, 20, MouseKind::WheelDown, vec![]);
    assert_eq!(help_state(&d).scroll, 3);

    // `/` filters by key, action or description; esc clears the filter, then closes.
    d.press("/").await;
    d.type_text("zoom").await;
    d.press("enter").await;
    let screen = d.screen();
    assert!(
        screen.contains("zoom toggle") && !screen.contains("h j k l"),
        "{screen}"
    );
    d.press("escape").await;
    assert!(help_state(&d).filter.is_empty());
    d.press("escape").await;
    assert!(matches!(d.app.overlay, Overlay::None));

    // q and ? close it too, and so does a click outside the box.
    for close in ["q", "?"] {
        d.press_all(&["ctrl+space", "?"]).await;
        d.press(close).await;
        assert!(matches!(d.app.overlay, Overlay::None), "{close}");
    }
    d.press_all(&["ctrl+space", "?"]).await;
    d.app
        .on_mouse(60, 20, MouseKind::Press(m::MouseButton::Left), vec![]);
    assert!(
        matches!(d.app.overlay, Overlay::Help(_)),
        "a click inside stays"
    );
    d.app
        .on_mouse(1, 1, MouseKind::Press(m::MouseButton::Left), vec![]);
    assert!(matches!(d.app.overlay, Overlay::None));
}
