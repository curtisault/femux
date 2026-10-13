//! 0.2.0 acceptance (SPEC §13): every default binding works and copy mode search passes.

mod common;

use common::Drive;
use femux_client::app::{Effect, Mode, Overlay};
use femux_client::config::ClientConfig;
use femux_proto::msg as m;

#[tokio::test(flavor = "multi_thread")]
async fn copy_mode_search_selects_and_yanks() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.obs.call(m::EventsSubscribe::default()).await.unwrap();
    d.type_text("i=1; while [ $i -le 40 ]; do echo line$i; i=$((i+1)); done\n")
        .await;
    d.wait_pane_text(&pane, "line40").await;
    d.press_all(&["ctrl+space", "["]).await;
    assert_eq!(d.app.mode, Mode::Copy);
    d.until("screen lines read", |a| {
        a.copy.as_ref().unwrap().lines.len() >= 22
    })
    .await;
    // ? searches up from the cursor: the newest "line1x" is line19.
    d.press("?").await;
    d.type_text("line1[0-9]").await;
    d.press("enter").await;
    d.until("jumped", |a| {
        let cm = a.copy.as_ref().unwrap();
        cm.lines.get(&cm.cur.0).is_some_and(|l| {
            l.iter()
                .map(|c| c.ch.as_str())
                .collect::<String>()
                .starts_with("line19")
        })
    })
    .await;
    let at19 = d.app.copy.as_ref().unwrap().cur.0;
    assert!(at19 < 0, "line19 scrolled into history: {at19}");
    assert!(
        !d.app.copy.as_ref().unwrap().matches.is_empty(),
        "matches highlighted"
    );
    // n continues in the same direction, N reverses.
    d.press("n").await;
    d.until("line18", |a| a.copy.as_ref().unwrap().cur.0 == at19 - 1)
        .await;
    d.press("N").await;
    d.until("back to line19", |a| a.copy.as_ref().unwrap().cur.0 == at19)
        .await;
    // / searches down; gg and G reach both ends.
    d.press("/").await;
    d.type_text("line3[0-9]").await;
    d.press("enter").await;
    d.until("line30", |a| a.copy.as_ref().unwrap().cur.0 == at19 + 11)
        .await;
    d.press_all(&["g", "g"]).await;
    d.until("top", |a| {
        let cm = a.copy.as_ref().unwrap();
        cm.first == Some(cm.cur.0)
    })
    .await;
    d.press("G").await;
    assert_eq!(d.app.copy.as_ref().unwrap().cur.0, 21);
    // Prompt marks: with no shell integration there are none, and [ is harmless.
    d.press("[").await;
    // Line selection + yank: OSC 52 on the host and clipboard.set for everyone.
    d.press("k").await;
    d.press("k").await;
    d.press("V").await;
    d.press("k").await;
    let want = d.app.copy.as_ref().unwrap().selection_text().unwrap();
    assert!(want.starts_with("line3") && want.contains('\n'), "{want:?}");
    d.press("y").await;
    assert_eq!(d.app.mode, Mode::Terminal);
    assert!(d.effects.contains(&Effect::Clipboard(want.clone())));
    let ev = d.obs.wait_event("clipboard.changed").await;
    let femux_proto::Event::ClipboardChanged(c) = ev else {
        panic!()
    };
    assert_eq!(
        (c.text, c.source),
        (want.clone(), m::ClipboardSource::Client)
    );
    // ] pastes it back into the pane.
    d.sent.clear();
    d.press_all(&["ctrl+space", "p"]).await;
    assert!(
        d.sent
            .iter()
            .any(|r| matches!(r, femux_proto::Request::PaneInput(i)
        if i.events.iter().any(|e| matches!(e, m::InputEvent::Paste(p) if p.text == want))))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn all_default_bindings_work() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let a = d.pane.clone();
    d.wait_pane_text(&a, "$").await;
    // s / v: split below / right; focus follows the new pane.
    d.press_all(&["ctrl+space", "|"]).await;
    d.until("2 panes", |x| x.model.panes.len() == 2).await;
    let b = d.app.model.focused_id().unwrap();
    d.press_all(&["ctrl+space", "-"]).await;
    d.until("3 panes", |x| x.model.panes.len() == 3).await;
    let c = d.app.model.focused_id().unwrap();
    // h j k l: focus.
    d.press_all(&["ctrl+space", "k"]).await;
    d.until("focus b", |x| {
        x.model.focused_id().as_deref() == Some(b.as_str())
    })
    .await;
    d.press_all(&["ctrl+space", "h"]).await;
    d.until("focus a", |x| {
        x.model.focused_id().as_deref() == Some(a.as_str())
    })
    .await;
    d.press_all(&["ctrl+space", "l"]).await;
    d.press_all(&["ctrl+space", "j"]).await;
    d.until("focus c", |x| {
        x.model.focused_id().as_deref() == Some(c.as_str())
    })
    .await;
    // H J K L: resize.
    let before = d.layout().await;
    d.press_all(&["ctrl+space", "H"]).await;
    d.press_all(&["ctrl+space", "K"]).await;
    d.until("resized", |_| true).await;
    let after = d.layout().await;
    assert_ne!(before.tree, after.tree, "H/K moved separators");
    // = equalize, r rotate, w swap.
    d.press_all(&["ctrl+space", "="]).await;
    d.settle().await;
    let eq = d.layout().await;
    assert_ne!(eq.tree, after.tree, "equalize changed ratios");
    d.press_all(&["ctrl+space", "r"]).await;
    d.settle().await;
    let rot = d.layout().await;
    assert_ne!(rot.tree, eq.tree, "rotate flipped the split");
    d.press_all(&["ctrl+space", "e"]).await;
    d.settle().await;
    let sw = d.layout().await;
    assert_ne!(sw.tree, rot.tree, "swap reordered panes");
    // z zoom, f float / tile.
    d.press_all(&["ctrl+space", "z"]).await;
    d.until("zoomed", |x| x.model.current_tab().unwrap().zoomed)
        .await;
    d.press_all(&["ctrl+space", "z"]).await;
    d.until("unzoomed", |x| !x.model.current_tab().unwrap().zoomed)
        .await;
    d.press_all(&["ctrl+space", "f"]).await;
    d.until("float", |x| {
        x.model.current_tab().unwrap().floats.len() == 1
    })
    .await;
    d.press_all(&["ctrl+space", "f"]).await;
    d.until("tile", |x| x.model.current_tab().unwrap().floats.is_empty())
        .await;
    // m<a-z> / '<a-z>: set and jump to a mark across tabs.
    d.press_all(&["ctrl+space", "m", "q"]).await;
    d.until("mark", |x| x.model.marks.get("q") == Some(&c))
        .await;
    // t / T / n / p / 1-9: tabs.
    d.press_all(&["ctrl+space", "t"]).await;
    d.until("tab 2", |x| {
        x.model.tabs.len() == 2 && x.model.current == x.model.tabs[1].id
    })
    .await;
    d.press_all(&["ctrl+space", "T"]).await;
    assert!(matches!(d.app.overlay, Overlay::Prompt { .. }));
    // The prompt starts with the current title ("sh <dir>"): clear it.
    for _ in 0..60 {
        d.press("backspace").await;
    }
    d.type_text("two").await;
    d.press("enter").await;
    d.until("renamed", |x| {
        x.model.tabs[1].custom_title.as_deref() == Some("two")
    })
    .await;
    d.press_all(&["ctrl+space", "N"]).await;
    d.until("tab 1", |x| x.model.current == x.model.tabs[0].id)
        .await;
    d.press_all(&["ctrl+space", "n"]).await;
    d.until("tab 2 again", |x| x.model.current == x.model.tabs[1].id)
        .await;
    d.press_all(&["ctrl+space", "1"]).await;
    d.until("tab 1 by number", |x| x.model.current == x.model.tabs[0].id)
        .await;
    d.press_all(&["ctrl+space", "2"]).await;
    d.until("tab 2 by number", |x| x.model.current == x.model.tabs[1].id)
        .await;
    d.press_all(&["ctrl+space", "'", "q"]).await;
    d.until("jumped to mark", |x| {
        x.model.current == x.model.tabs[0].id && x.model.focused_id().as_deref() == Some(c.as_str())
    })
    .await;
    // [ copy mode, ] paste (nothing yet: message), : palette, ? help, S W b inert.
    d.press_all(&["ctrl+space", "["]).await;
    assert_eq!(d.app.mode, Mode::Copy);
    d.press("q").await;
    d.press_all(&["ctrl+space", "p"]).await;
    assert_eq!(d.app.message.as_deref(), Some("nothing to paste"));
    d.press_all(&["ctrl+space", ":"]).await;
    assert!(matches!(d.app.overlay, Overlay::Palette(_)));
    d.press("escape").await;
    d.press_all(&["ctrl+space", "?"]).await;
    assert!(matches!(d.app.overlay, Overlay::Help));
    assert!(d.screen().contains("prefix table"));
    d.press("escape").await;
    // S / W: the navigators (0.4.0), q closes them; b toggles the side nav.
    for k in ["s", "w"] {
        d.press_all(&["ctrl+space", k]).await;
        assert!(matches!(d.app.overlay, Overlay::Navigator(_)), "{k}");
        d.press("q").await;
        assert!(matches!(d.app.overlay, Overlay::None), "{k}");
    }
    d.press_all(&["ctrl+space", "b"]).await;
    assert!(d.app.sidenav_open);
    d.press_all(&["ctrl+space", "b"]).await;
    assert!(!d.app.sidenav_open);
    // x: close pane; confirmation when a program runs in it.
    d.type_text("sleep 30\n").await;
    d.until("running", |x| x.model.panes[&c].running()).await;
    d.press_all(&["ctrl+space", "x"]).await;
    assert!(matches!(d.app.overlay, Overlay::Confirm { .. }));
    d.press("n").await;
    assert!(d.app.model.panes.contains_key(&c), "n keeps the pane");
    d.press_all(&["ctrl+space", "x"]).await;
    d.press("y").await;
    d.until("closed", |x| !x.model.panes.contains_key(&c)).await;
    // prefix space: sticky normal; esc back. prefix prefix passes through. d detaches.
    d.press_all(&["ctrl+space", "space"]).await;
    assert_eq!(d.app.mode, Mode::Normal);
    d.press("escape").await;
    assert_eq!(d.app.mode, Mode::Terminal);
    d.press_all(&["ctrl+space", "d"]).await;
    assert!(d.effects.contains(&Effect::Exit));
}
