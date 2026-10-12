//! REQ-UI-044: the agent inbox lists blocked, error and done panes across sessions, jumps
//! with one key and answers a blocked agent with one key.

mod common;

use std::time::{Duration, Instant};

use common::Drive;
use femux_client::app::Overlay;
use femux_client::config::ClientConfig;
use femux_client::inbox::entries;
use femux_proto::msg as m;

async fn pump(d: &mut Drive) {
    d.app.tick(Instant::now());
    d.settle().await;
}

async fn set(d: &mut Drive, pane: &str, kv: &[(&str, &str)]) {
    d.obs
        .call(m::PaneSetMeta {
            pane_id: pane.into(),
            set: Some(
                kv.iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ),
            unset: None,
        })
        .await
        .unwrap();
}

async fn text_of(d: &mut Drive, pane: &str) -> String {
    let r = d
        .obs
        .call(m::ScrollbackRead {
            pane_id: pane.into(),
            from_line: 0,
            count: 100,
        })
        .await
        .unwrap();
    r.lines
        .iter()
        .map(|l| l.iter().map(|c| c.ch.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn listed(d: &Drive) -> Vec<(String, String)> {
    let Overlay::Inbox(st) = &d.app.overlay else {
        panic!("no inbox: {:?}", d.app.overlay)
    };
    entries(&d.app.dir, st)
        .into_iter()
        .map(|e| (e.pane, e.state))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_044_inbox_jumps_and_answers_across_sessions() {
    let mut d = Drive::with(
        ClientConfig::default(),
        vec![femux_plugin_agents::client()],
        120,
        30,
    )
    .await;
    let here = d.pane.clone();
    d.wait_pane_text(&here, "$").await;
    // Another session holds an agent waiting for an answer.
    let (_, there) = d.obs.new_session("infra").await;
    d.obs.type_text(&there, "read a; echo answered-$a\n").await;
    set(
        &mut d,
        &there,
        &[
            ("agent.state", "blocked"),
            ("agent.name", "codex"),
            ("agent.source", "hook"),
        ],
    )
    .await;
    set(
        &mut d,
        &here,
        &[
            ("agent.state", "done"),
            ("agent.name", "opencode"),
            ("agent.source", "process"),
        ],
    )
    .await;

    d.press_all(&["ctrl+space", "i"]).await;
    assert!(matches!(d.app.overlay, Overlay::Inbox(_)));
    let deadline = Instant::now() + Duration::from_secs(10);
    while listed(&d).len() < 2 {
        assert!(Instant::now() < deadline, "{}", d.screen());
        pump(&mut d).await;
    }
    assert_eq!(
        listed(&d),
        [
            (there.clone(), "blocked".into()),
            (here.clone(), "done".into())
        ]
    );
    pump(&mut d).await;
    let screen = d.screen();
    assert!(screen.contains("Inbox"), "{screen}");
    assert!(screen.contains("2 across 2 sessions"), "{screen}");
    assert!(
        screen.contains("codex") && screen.contains("infra · 1"),
        "{screen}"
    );
    assert!(
        screen.contains("source: hook") && screen.contains("source: process"),
        "{screen}"
    );
    assert!(screen.contains("INBOX"), "{screen}");

    // `y` answers the selected blocked agent: the letter, then Enter.
    d.press("y").await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !text_of(&mut d, &there).await.contains("answered-y") {
        assert!(
            Instant::now() < deadline,
            "{}",
            text_of(&mut d, &there).await
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // `x` dismisses: the pane goes back to idle and leaves the list.
    d.press("j").await;
    d.press("x").await;
    let info = d
        .obs
        .call(m::PaneInfo {
            pane_id: here.clone(),
        })
        .await
        .unwrap();
    assert_eq!(info.meta["agent.state"], "idle");
    assert_eq!(listed(&d), [(there.clone(), "blocked".into())]);

    // `/` filters, `1` jumps to the first row: another session.
    d.press_all(&["/", "z", "z", "enter"]).await;
    assert!(listed(&d).is_empty());
    d.press_all(&["/", "backspace", "backspace", "enter"]).await;
    d.press("1").await;
    d.until("attached to infra", |a| {
        a.model.session == "infra" && a.model.focused_id().as_deref() == Some(there.as_str())
    })
    .await;
    assert!(matches!(d.app.overlay, Overlay::None));
}
