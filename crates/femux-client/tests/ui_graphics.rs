//! REQ-GFX-001 / REQ-UI-052 (client side): a pane program's Kitty image is read from the
//! daemon, transmitted to the host once and placed at the pane's screen position; overlays
//! hide it and closing them brings it back.

mod common;

use std::time::{Duration, Instant};

use common::Drive;
use femux_client::config::ClientConfig;

const ICAT: &str =
    "printf '\\033_Ga=T,f=24,s=2,v=2,i=7,c=4,r=2;/wAA/wAA/wAA/wAA\\033\\\\'; echo; echo shown\n";

/// Syncs graphics until the host output contains `needle`; returns everything written.
async fn host_until(d: &mut Drive, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut all = String::new();
    loop {
        d.app.sync_graphics();
        d.settle().await;
        all.push_str(&String::from_utf8(d.app.graphics.take_output()).unwrap());
        if all.contains(needle) {
            return all;
        }
        assert!(Instant::now() < deadline, "never wrote {needle:?}: {all:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gfx_001_images_reach_the_host_at_the_pane_position() {
    let mut d = Drive::with(ClientConfig::default(), vec![], 80, 24).await;
    d.app.graphics.enabled = true;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.type_text(ICAT).await;
    d.wait_pane_text(&pane, "shown").await;
    let out = host_until(&mut d, "a=p,").await;
    // Transmitted once as host image 1, RGB 2x2.
    assert!(
        out.contains("\x1b_Ga=t,i=1,f=24,s=2,v=2,q=2,m=0;/wAA/wAA/wAA/wAA\x1b\\"),
        "{out:?}"
    );
    // Placed at the pane's screen position (the tab strip takes row 1).
    let p = &d.app.frame_images[0];
    let area = d.app.layout().panes;
    let row = area.y as i32 + p.y + 1;
    assert!(
        out.contains(&format!("\x1b[{row};1H\x1b_Ga=p,i=1,p=1,c=4,r=2,")),
        "{out:?}"
    );
    // Nothing more while nothing changes.
    d.app.sync_graphics();
    assert!(d.app.graphics.take_output().is_empty());
    // A full-screen overlay hides it; closing it puts it back without reading again.
    d.press_all(&["ctrl+space", "W"]).await;
    let out = host_until(&mut d, "a=d,").await;
    assert!(out.contains("\x1b_Ga=d,d=i,i=1,p=1,q=2\x1b\\"));
    d.press("q").await;
    let out = host_until(&mut d, "a=p,").await;
    assert!(!out.contains("a=t,"), "{out:?}");
    let reads = d
        .sent_kinds()
        .iter()
        .filter(|k| **k == "image.read")
        .count();
    assert_eq!(reads, 1);
}

/// yazi draws previews with Kitty Unicode placeholders: a virtual placement plus U+10EEEE
/// cells. They reach the host as ordinary placements and the cells show blank.
#[tokio::test(flavor = "multi_thread")]
async fn gfx_001_unicode_placeholders_are_drawn_as_images() {
    let mut d = Drive::with(ClientConfig::default(), vec![], 80, 24).await;
    d.app.graphics.enabled = true;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    // The placeholder (U+10EEEE) and row/column diacritics (U+0305) as UTF-8 octal escapes,
    // which every printf understands.
    let ph = "\\364\\216\\273\\256";
    let zero = "\\314\\205";
    d.type_text(&format!(
        "printf '\\033_Ga=T,U=1,f=24,s=2,v=2,i=5,c=2,r=1,q=2;/wAA/wAA/wAA/wAA\\033\\\\\\033[38;5;5m{ph}{zero}{zero}{ph}\\033[0m\\n'; echo placed\n"
    ))
    .await;
    d.wait_pane_text(&pane, "placed").await;
    let out = host_until(&mut d, "a=p,").await;
    assert!(out.contains(",c=2,r=1,"), "{out:?}");
    assert!(!d.screen().contains('\u{10EEEE}'));
}
