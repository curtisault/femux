//! REQ-LIFE-002 (in-process). REQ-LIFE-001 needs a real process; see femux-cli `tests/binary.rs`.

use std::time::{Duration, Instant};

use femux_proto::msg::*;
use femux_test::Harness;

async fn wait_finished(h: &Harness, want: bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if h.handle.as_ref().unwrap().is_finished() == want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn life_002_exit_with_last_session() {
    let h = Harness::start();
    let mut c = h.client().await;
    // A fresh daemon with no sessions stays up.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!h.handle.as_ref().unwrap().is_finished());
    c.call(SessionCreate {
        name: "a".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    c.call(SessionCreate {
        name: "b".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    c.call(SessionKill { name: "a".into() }).await.unwrap();
    assert!(!wait_finished(&h, true).await, "one session left");
    c.call(SessionKill { name: "b".into() }).await.unwrap();
    assert!(
        wait_finished(&h, true).await,
        "daemon should exit with its last session"
    );
    assert!(!h.socket.exists(), "socket removed on exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn life_002_keep_alive() {
    let h = Harness::start_with(|c| c.keep_alive = true);
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "a".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    c.call(SessionKill { name: "a".into() }).await.unwrap();
    assert!(
        !wait_finished(&h, true).await,
        "keep_alive keeps the daemon up"
    );
    // Closing the last pane closes its tab and session too.
    c.call(SessionCreate {
        name: "b".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let p = c
        .call(SessionAttach {
            name: "b".into(),
            tab: None,
            cols: None,
            rows: None,
        })
        .await
        .unwrap()
        .snapshot
        .tabs[0]
        .focus
        .clone();
    c.call(EventsSubscribe::default()).await.unwrap();
    c.type_text(&p, "exit\n").await;
    c.wait_event("session.closed").await;
    assert!(c.call(SessionList {}).await.unwrap().sessions.is_empty());
}

/// A request that ends the daemon (killing the last session, `server.stop`) is answered before
/// the daemon goes away. Repeated because the old failure was a race that lost the reply now
/// and then.
#[tokio::test(flavor = "multi_thread")]
async fn life_002_the_request_that_ends_the_daemon_is_answered() {
    for round in 0..20 {
        let h = Harness::start();
        let mut c = h.client().await;
        c.call(SessionCreate {
            name: "a".into(),
            root: None,
            file: None,
        })
        .await
        .unwrap();
        if round % 2 == 0 {
            c.call(SessionKill { name: "a".into() })
                .await
                .unwrap_or_else(|e| panic!("round {round}: kill unanswered: {e:?}"));
        } else {
            c.call(ServerStop { kill_panes: true })
                .await
                .unwrap_or_else(|e| panic!("round {round}: stop unanswered: {e:?}"));
        }
        assert!(wait_finished(&h, true).await, "round {round}: daemon exits");
        assert!(!h.socket.exists(), "round {round}: socket removed");
    }
}

/// With no request in flight, the last shell exiting still ends the daemon.
#[tokio::test(flavor = "multi_thread")]
async fn life_002_exit_when_the_last_shell_exits() {
    let h = Harness::start();
    let mut c = h.client().await;
    let (_, pane) = c.new_session("a").await;
    c.attach("a", 80, 24).await;
    c.wait_text(&pane, "$").await;
    c.type_text(&pane, "exit\n").await;
    assert!(
        wait_finished(&h, true).await,
        "daemon exits with its last shell"
    );
}
