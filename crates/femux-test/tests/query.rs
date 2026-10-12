//! REQ-QRY-*: the daemon answers terminal queries; panes never see host terminal bytes.

use std::time::Duration;

use femux_proto::msg::*;
use femux_test::{Client, Harness};

/// A pane that sends `query` and dumps the reply with od.
fn probe(query: &str) -> String {
    format!("stty -icanon -echo min 0 time 10; printf '{query}'; od -An -c; sleep 100")
}

async fn pane_running(c: &mut Client, cmd: String) -> String {
    c.call(TabCreate {
        session: "s".into(),
        cwd: None,
        cmd: Some(cmd),
        title: None,
        index: Some(0),
    })
    .await
    .unwrap()
    .pane_id
}

#[tokio::test(flavor = "multi_thread")]
async fn qry_001_daemon_answers_queries() {
    let h = Harness::start();
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let da1 = pane_running(&mut c, probe("\\033[c")).await;
    let xtv = pane_running(&mut c, probe("\\033[>q")).await;
    let kitty = pane_running(&mut c, probe("\\033[?u")).await;
    let decrqm = pane_running(&mut c, probe("\\033[?2026$p")).await;
    let mut out = Vec::new();
    for (pane, want) in [
        (&da1, "?   6   2   ;   2   2   c"),
        (&xtv, "f   e   m   u   x"),
        (&kitty, "[   ?   0   u"),
        (&decrqm, "2   0   2   6   ;"),
    ] {
        let tab = c
            .call(PaneInfo {
                pane_id: pane.clone(),
            })
            .await
            .unwrap()
            .tab;
        let att = c
            .call(SessionAttach {
                name: "s".into(),
                tab: Some(tab),
                cols: Some(80),
                rows: Some(24),
            })
            .await
            .unwrap();
        c.call(FrameSubscribe {
            view_id: att.view_id.clone(),
        })
        .await
        .unwrap();
        c.wait_text(pane, want).await;
        out.push(c.screen_text(pane));
        c.call(SessionDetach {
            view_id: att.view_id,
        })
        .await
        .unwrap();
    }
    assert_eq!(out.len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn qry_002_answers_follow_the_attached_client() {
    let h = Harness::start();
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    // No client attached: conservative defaults.
    let before = pane_running(
        &mut c,
        format!("sleep 0.5; {}", probe("\\033]11;?\\033\\\\")),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let mut hello = Client::hello(80, 24);
    hello.term.as_mut().unwrap().bg = Some([255, 255, 255]);
    let mut light = Client::connect(&h.socket, hello).await.unwrap();
    let att = light.attach("s", 80, 24).await;
    light.wait_text(&before, "0   0   0   0   /").await;
    let after = pane_running(&mut light, probe("\\033]11;?\\033\\\\")).await;
    let tab = light
        .call(PaneInfo {
            pane_id: after.clone(),
        })
        .await
        .unwrap()
        .tab;
    light
        .call(TabSelect {
            view_id: Some(att.view_id),
            tab_id: tab,
        })
        .await
        .unwrap();
    light
        .wait_text(&after, "r   g   b   :   f   f   f   f")
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn qry_003_panes_get_encoded_input_only() {
    let h = Harness::start();
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let pane = pane_running(&mut c, "head -n 1 | od -An -c; sleep 100".into()).await;
    let att = c.attach("s", 80, 24).await;
    let _ = att;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // A paste carrying an escape sequence reaches the pane with control bytes neutralized.
    c.call(PaneInput {
        pane_id: pane.clone(),
        events: vec![InputEvent::Paste(Paste {
            text: "a\u{1b}[31mb\n".into(),
        })],
    })
    .await
    .unwrap();
    c.wait_text(&pane, "b  \\n").await;
    assert!(
        !c.screen_text(&pane).contains("033"),
        "{}",
        c.screen_text(&pane)
    );
}
