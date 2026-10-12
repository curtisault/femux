//! REQ-PLUG-002/003: a daemon plugin reaches the core only over the protocol.

use std::time::Duration;

use femux_plugin::{DaemonPlugin, run_daemon_plugin};
use femux_proto::msg::*;
use femux_proto::{Event, Request};
use femux_test::Harness;

/// Tags every spawned pane with `tagger.seen=1`.
struct Tagger;

impl DaemonPlugin for Tagger {
    fn name(&self) -> &'static str {
        "tagger"
    }
    fn subscription(&self) -> EventsSubscribe {
        EventsSubscribe {
            kinds: Some(vec!["pane.spawned".into()]),
            ..Default::default()
        }
    }
    fn on_event(&mut self, ev: &Event) -> Vec<Request> {
        match ev {
            Event::PaneSpawned(p) => vec![
                PaneSetMeta {
                    pane_id: p.pane_id.clone(),
                    set: Some([("tagger.seen".to_owned(), "1".to_owned())].into()),
                    unset: None,
                }
                .into(),
            ],
            _ => vec![],
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn plug_002_daemon_plugin_subscribes_and_writes_metadata() {
    let h = Harness::start();
    let socket = h.socket.clone();
    let task = tokio::spawn(async move { run_daemon_plugin(&socket, Box::new(Tagger)).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.call(EventsSubscribe::default()).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let info = c
            .call(PaneInfo {
                pane_id: pane.clone(),
            })
            .await
            .unwrap();
        if info.meta.get("tagger.seen").map(String::as_str) == Some("1") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "plugin never tagged the pane"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    task.abort();
}
