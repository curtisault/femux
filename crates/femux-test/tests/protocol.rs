//! REQ-PROTO-*: transport, framing, envelope, handshake and frame flow.

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use femux_proto::msg::*;
use femux_proto::{
    ErrorCode, Request, ServerMsg, decode_envelope, encode_frame, io_async::read_frame,
};
use femux_test::{Client, Harness};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

#[tokio::test(flavor = "multi_thread")]
async fn proto_001_socket_and_directory_modes() {
    let h = Harness::start();
    let sock = std::fs::metadata(&h.socket).unwrap().permissions().mode() & 0o777;
    let dir = std::fs::metadata(h.socket.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!((sock, dir), (0o600, 0o700));
    // The default path follows XDG_RUNTIME_DIR / TMPDIR.
    let default = femux_proto::socket_path("default");
    assert!(default.ends_with("default.sock"));
    assert!(
        default
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("femux")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn proto_002_same_uid_peer_is_served() {
    // Refusing other UIDs is unit-tested (femux-daemon `server::tests::other_uids_are_refused`);
    // a same-UID client completes the handshake.
    assert!(!femux_daemon::peer_allowed(0, 1000));
    let h = Harness::start();
    let c = h.client().await;
    assert_eq!(c.hello_ok.as_ref().unwrap().server, "test");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn proto_003_no_tcp_listener() {
    let h = Harness::start();
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    // No socket of this process (daemon included, it runs in-process) is a TCP socket.
    let mut ours = std::collections::HashSet::new();
    for fd in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        if let Ok(t) = std::fs::read_link(fd.path())
            && let Some(ino) = t
                .to_str()
                .and_then(|s| s.strip_prefix("socket:["))
                .and_then(|s| s.strip_suffix(']'))
        {
            ours.insert(ino.to_owned());
        }
    }
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let ino = line.split_whitespace().nth(9).unwrap();
            assert!(!ours.contains(ino), "TCP socket {line}");
        }
    }
    assert!(!ours.is_empty());
}

async fn raw_hello(s: &mut UnixStream, proto: u32, features: &[&str]) -> ServerMsg {
    let hello = Hello {
        proto,
        client: "raw".into(),
        features: features.iter().map(|f| f.to_string()).collect(),
        term: None,
    };
    s.write_all(&Request::from(hello).encode(1).unwrap())
        .await
        .unwrap();
    next(s).await.unwrap()
}

async fn next(s: &mut UnixStream) -> Option<ServerMsg> {
    let frame = tokio::time::timeout(Duration::from_secs(5), read_frame(s))
        .await
        .ok()?
        .ok()??;
    Some(ServerMsg::decode(decode_envelope(&frame).unwrap()).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn proto_004_oversized_frame_closes_connection() {
    let h = Harness::start();
    let mut s = UnixStream::connect(&h.socket).await.unwrap();
    raw_hello(&mut s, 1, &["session"]).await;
    s.write_all(&((16u32 << 20) + 1).to_be_bytes())
        .await
        .unwrap();
    assert!(next(&mut s).await.is_none(), "connection should close");
}

#[tokio::test(flavor = "multi_thread")]
async fn proto_011_ids_and_kinds() {
    let h = Harness::start();
    let mut s = UnixStream::connect(&h.socket).await.unwrap();
    raw_hello(&mut s, 1, &["session", "events"]).await;
    s.write_all(
        &Request::from(EventsSubscribe::default())
            .encode(41)
            .unwrap(),
    )
    .await
    .unwrap();
    assert!(matches!(
        next(&mut s).await,
        Some(ServerMsg::Response(41, _))
    ));
    s.write_all(
        &Request::from(SessionCreate {
            name: "x".into(),
            root: None,
            file: None,
        })
        .encode(42)
        .unwrap(),
    )
    .await
    .unwrap();
    let mut saw_event = false;
    let mut saw_response = false;
    while !(saw_event && saw_response) {
        match next(&mut s).await.unwrap() {
            ServerMsg::Response(42, r) => {
                assert_eq!(r.kind(), "session.create");
                saw_response = true;
            }
            ServerMsg::Event(_) => saw_event = true, // decoded only because its id was 0
            m => panic!("{m:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn proto_012_unknown_kinds_and_fields() {
    let h = Harness::start();
    let mut s = UnixStream::connect(&h.socket).await.unwrap();
    raw_hello(&mut s, 1, &["session"]).await;
    s.write_all(&encode_frame(5, "session.frobnicate", &SessionList {}).unwrap())
        .await
        .unwrap();
    match next(&mut s).await.unwrap() {
        ServerMsg::Error(5, e) => assert_eq!(e.code, ErrorCode::UnknownKind),
        m => panic!("{m:?}"),
    }
    #[derive(serde::Serialize)]
    struct Future {
        name: &'static str,
        colour: &'static str,
    }
    s.write_all(
        &encode_frame(
            6,
            "session.create",
            &Future {
                name: "f",
                colour: "teal",
            },
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert!(matches!(
        next(&mut s).await.unwrap(),
        ServerMsg::Response(6, _)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn proto_020_missing_feature_loses_action_not_connection() {
    let h = Harness::start();
    let mut hello = Client::hello(80, 24);
    hello.features = vec!["session".into()];
    let mut c = Client::connect(&h.socket, hello).await.unwrap();
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let e = c
        .call(TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: None,
            title: None,
            index: None,
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::FeatureMissing);
    // Families the daemon does not serve yet are missing too.
    let mut full = h.client().await;
    assert_eq!(
        full.call(StoreSnapshot {}).await.unwrap_err().code,
        ErrorCode::FeatureMissing
    );
    assert!(
        !full
            .hello_ok
            .as_ref()
            .unwrap()
            .features
            .contains(&"store".to_string())
    );
    // Still connected.
    assert_eq!(c.call(SessionList {}).await.unwrap().sessions.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn proto_021_major_version_mismatch_closes() {
    let h = Harness::start();
    let mut s = UnixStream::connect(&h.socket).await.unwrap();
    match raw_hello(&mut s, 2, &["session"]).await {
        ServerMsg::Error(1, e) => assert_eq!(e.code, ErrorCode::ProtoVersion),
        m => panic!("{m:?}"),
    }
    assert!(next(&mut s).await.is_none(), "connection should close");
}

#[tokio::test(flavor = "multi_thread")]
async fn proto_040_one_unacked_frame_coalesces() {
    let h = Harness::start();
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    c.auto_ack = false;
    let att = c.attach("s", 80, 24).await;
    let pane = att.snapshot.tabs[0].focus.clone();
    c.wait_for("first frame", Duration::from_secs(5), |c| {
        !c.frames.is_empty()
    })
    .await;
    c.type_text(&pane, "for i in 1 2 3 4 5 6 7 8 9; do echo line$i; done\n")
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    while c.recv(Duration::from_millis(50)).await {}
    assert_eq!(c.frames.len(), 1, "no frame while one is unacked");
    let seq = c.frames[0].frame_seq;
    c.call(FrameAck {
        view_id: att.view_id.clone(),
        frame_seq: seq,
    })
    .await
    .unwrap();
    c.wait_for("next frame", Duration::from_secs(5), |c| {
        c.frames.len() == 2
    })
    .await;
    // The next frame already holds the newest state.
    assert!(
        c.screen_text(&pane).contains("line9"),
        "{}",
        c.screen_text(&pane)
    );
}

/// A subscriber that stops reading falls behind; it gets events_lost, then a full frame.
#[tokio::test(flavor = "multi_thread")]
async fn proto_041_full_frame_after_attach_and_events_lost() {
    let h = Harness::start_with(|c| c.events_buffer = 8);
    let mut driver = h.client().await;
    driver
        .call(SessionCreate {
            name: "s".into(),
            root: None,
            file: None,
        })
        .await
        .unwrap();

    let mut s = UnixStream::connect(&h.socket).await.unwrap();
    raw_hello(&mut s, 1, &["session", "events", "frame", "pane"]).await;
    s.write_all(
        &Request::from(SessionAttach {
            name: "s".into(),
            tab: None,
            cols: Some(80),
            rows: Some(24),
        })
        .encode(2)
        .unwrap(),
    )
    .await
    .unwrap();
    let view_id = match next(&mut s).await.unwrap() {
        ServerMsg::Response(2, femux_proto::Response::SessionAttach(ok)) => ok.view_id,
        m => panic!("{m:?}"),
    };
    let pane = driver
        .call(SessionAttach {
            name: "s".into(),
            tab: None,
            cols: Some(80),
            rows: Some(24),
        })
        .await
        .unwrap()
        .snapshot
        .tabs[0]
        .focus
        .clone();
    s.write_all(
        &Request::from(FrameSubscribe {
            view_id: view_id.clone(),
        })
        .encode(3)
        .unwrap(),
    )
    .await
    .unwrap();
    s.write_all(&Request::from(EventsSubscribe::default()).encode(4).unwrap())
        .await
        .unwrap();
    let mut first_full = None;
    let mut seq = 0;
    while first_full.is_none() {
        if let ServerMsg::Push(femux_proto::Push::Frame(f)) = next(&mut s).await.unwrap() {
            first_full = Some(f.full);
            seq = f.frame_seq;
        }
    }
    assert_eq!(first_full, Some(true), "first frame after attach is full");

    // Stop reading while thousands of large events are emitted.
    let big = "x".repeat(2000);
    for i in 0..3000 {
        let set = [(format!("k{i}"), big.clone())].into_iter().collect();
        driver
            .call(PaneSetMeta {
                pane_id: pane.clone(),
                set: Some(set),
                unset: None,
            })
            .await
            .unwrap();
    }
    // Resume: ack the old frame so a new one may come, and look for events_lost then a frame.
    s.write_all(
        &Request::from(FrameAck {
            view_id: view_id.clone(),
            frame_seq: seq,
        })
        .encode(5)
        .unwrap(),
    )
    .await
    .unwrap();
    driver.type_text(&pane, "echo after\n").await;
    let mut lost = false;
    loop {
        match next(&mut s).await.expect("stream ended") {
            ServerMsg::Event(femux_proto::Event::EventsLost(l)) => {
                assert!(l.to >= l.from);
                lost = true;
            }
            ServerMsg::Push(femux_proto::Push::Frame(f)) if lost && f.frame_seq > seq => {
                assert!(f.full, "first frame after events_lost is full");
                break;
            }
            ServerMsg::Push(femux_proto::Push::Frame(f)) => {
                seq = f.frame_seq;
                s.write_all(
                    &Request::from(FrameAck {
                        view_id: view_id.clone(),
                        frame_seq: seq,
                    })
                    .encode(6)
                    .unwrap(),
                )
                .await
                .unwrap();
            }
            _ => {}
        }
    }
}
