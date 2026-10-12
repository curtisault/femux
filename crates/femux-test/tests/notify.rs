//! ADR-018: `session.list` reports last activity; `notify.post` relays `notify.posted`.

use std::time::Duration;

use femux_proto::Event;
use femux_proto::msg::*;
use femux_test::Harness;

#[tokio::test(flavor = "multi_thread")]
async fn last_activity_follows_output_and_input() {
    let h = Harness::start();
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 80, 24).await;
    c.wait_text(&pane, "$").await;
    let list = c.call(SessionList {}).await.unwrap().sessions;
    let first = list[0].last_activity;
    assert!(first >= list[0].created);
    tokio::time::sleep(Duration::from_millis(50)).await;
    c.type_text(&pane, "echo hi\n").await;
    c.wait_text(&pane, "hi").await;
    let later = c.call(SessionList {}).await.unwrap().sessions[0].last_activity;
    assert!(later > first, "{later} > {first}");
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_post_is_relayed_to_the_panes_session() {
    let h = Harness::start();
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    let _ = c.new_session("other").await;
    let mut same = h.client().await;
    same.call(EventsSubscribe {
        session: Some("s".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut elsewhere = h.client().await;
    elsewhere
        .call(EventsSubscribe {
            session: Some("other".into()),
            kinds: Some(vec!["notify.posted".into()]),
            ..Default::default()
        })
        .await
        .unwrap();
    c.call(NotifyPost {
        pane_id: Some(pane.clone()),
        title: "codex needs y/n".into(),
        level: Some(NotifyLevel::Warn),
    })
    .await
    .unwrap();
    let Event::NotifyPosted(n) = same.wait_event("notify.posted").await else {
        panic!()
    };
    assert_eq!(
        (n.pane_id.as_deref(), n.title.as_str(), n.level),
        (Some(pane.as_str()), "codex needs y/n", NotifyLevel::Warn)
    );
    // A pane-less notification reaches every session; the level defaults to info.
    c.call(NotifyPost {
        pane_id: None,
        title: "build done".into(),
        level: None,
    })
    .await
    .unwrap();
    let Event::NotifyPosted(n) = elsewhere.wait_event("notify.posted").await else {
        panic!()
    };
    assert_eq!(
        (n.title.as_str(), n.level),
        ("build done", NotifyLevel::Info)
    );
    assert_eq!(
        elsewhere.events.len(),
        1,
        "the pane's notification stayed in its session"
    );
    let bad = c
        .call(NotifyPost {
            pane_id: Some("01J9Z3K8M2N4P6Q8R0S2T4V6W8".into()),
            title: "x".into(),
            level: None,
        })
        .await
        .unwrap_err();
    assert_eq!(bad.code, femux_proto::ErrorCode::NotFound);
}
