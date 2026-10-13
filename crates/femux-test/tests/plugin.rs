//! ADR-020: `plugin.publish` is relayed as `plugin.data` to every subscriber, whatever
//! session it watches.

use femux_proto::Event;
use femux_proto::msg::*;
use femux_test::Harness;

#[tokio::test(flavor = "multi_thread")]
async fn plugin_data_reaches_every_subscriber() {
    let h = Harness::start();
    let mut publisher = h.client().await;
    let mut watcher = h.client().await;
    watcher.new_session("s").await;
    watcher
        .call(EventsSubscribe {
            session: Some("s".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    publisher
        .call(PluginPublish {
            plugin: "hosts".into(),
            data: [("hostname".to_owned(), "build-box".to_owned())].into(),
        })
        .await
        .unwrap();
    let Event::PluginData(d) = watcher.wait_event_from("plugin.data", 0).await else {
        panic!()
    };
    assert_eq!(d.plugin, "hosts");
    assert_eq!(d.data["hostname"], "build-box");
    assert!(d.seq > 0 && d.ts > 0);
}
