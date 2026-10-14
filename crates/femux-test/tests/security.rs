//! §12 security requirements against a running daemon.

use femux_proto::msg as m;
use femux_test::{Client, Harness};

/// The shell asks for the clipboard (OSC 52 `?`) and prints the reply's first `bytes` bytes
/// with `od`, so the pane shows what the program received.
async fn ask_for_clipboard(c: &mut Client, pane: &str, bytes: usize, marker: &str) -> String {
    c.type_text(
        pane,
        &format!(
            "clear; stty raw -echo; printf '\\033]52;c;?\\a'; head -c {bytes} | od -An -c; stty sane; echo {marker}-$((6*7))\n"
        ),
    )
    .await;
    c.wait_text(pane, &format!("{marker}-42")).await;
    c.screen_text(pane)
}

/// REQ-SEC-002: by default a pane's clipboard read gets an empty clipboard, never the text;
/// once `femux reload` puts its command in `read_allow`, the newest clipboard text.
#[tokio::test(flavor = "multi_thread")]
async fn sec_002_clipboard_reads_need_read_allow() {
    let dir = tempfile::tempdir().unwrap();
    let conf = dir.path().join("config.toml");
    std::fs::write(&conf, "").unwrap();
    let h = Harness::start_with(|c| c.config_path = Some(conf.clone()));
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 100, 24).await;
    c.wait_text(&pane, "$").await;
    c.call(m::ClipboardSet {
        text: "hello".into(),
    })
    .await
    .unwrap();
    // The denied reply is `ESC ] 52 ; c ; ESC \`: eight bytes.
    let text = ask_for_clipboard(&mut c, &pane, 8, "denied").await;
    assert!(text.contains("5   2   ;   c   ;"), "{text}");
    assert!(
        !text.contains("a   G   V   s"),
        "denied reads carry no text:\n{text}"
    );
    // Allow the shell under whatever name the system gives it (`/bin/sh` is bash or dash),
    // and `head`, which may already be in the foreground when the request is read.
    let shell = c
        .call(m::PaneInfo {
            pane_id: pane.clone(),
        })
        .await
        .unwrap()
        .fg_cmd;
    std::fs::write(
        &conf,
        format!("[clipboard]\nread_allow = [\"{shell}\", \"head\"]\n"),
    )
    .unwrap();
    c.call(m::ServerReload {}).await.unwrap();
    let text = ask_for_clipboard(&mut c, &pane, 16, "allowed").await;
    assert!(text.contains("a   G   V   s   b   G   8   ="), "{text}");
}

/// Has the pane print a DCS sequence (one femux would pass through) and a marker after it.
async fn print_dcs(c: &mut Client, pane: &str, marker: &str) {
    c.type_text(
        pane,
        &format!("printf '\\033P1;2|femux\\033\\\\'; echo {marker}-$((6*7))\n"),
    )
    .await;
    c.wait_text(pane, &format!("{marker}-42")).await;
}

/// REQ-SEC-004: DCS sequences reach clients as `pane.passthrough` only with
/// `[terminal] passthrough = true`, and are never replayed to a later subscriber.
#[tokio::test(flavor = "multi_thread")]
async fn sec_004_dcs_passthrough_is_opt_in() {
    use base64::Engine;
    let h = Harness::start();
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 80, 24).await;
    c.call(m::EventsSubscribe::default()).await.unwrap();
    c.wait_text(&pane, "$").await;
    print_dcs(&mut c, &pane, "off").await;
    assert!(
        !c.events.iter().any(|e| e.kind() == "pane.passthrough"),
        "off by default"
    );
    drop(c);
    drop(h);

    let h = Harness::start_with(|cfg| cfg.passthrough = true);
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 80, 24).await;
    c.call(m::EventsSubscribe::default()).await.unwrap();
    c.wait_text(&pane, "$").await;
    let start = c.events.len();
    print_dcs(&mut c, &pane, "on").await;
    let femux_proto::Event::PanePassthrough(p) = c.wait_event_from("pane.passthrough", start).await
    else {
        unreachable!()
    };
    assert_eq!(p.pane_id, pane);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&p.data)
        .unwrap();
    assert_eq!(bytes, b"\x1bP1;2|femux\x1b\\");
    // A client that subscribes afterwards asking for history gets no stale sequence.
    let mut late = h.client().await;
    late.call(m::EventsSubscribe {
        since: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();
    late.recv(std::time::Duration::from_millis(300)).await;
    assert!(
        !late.events.iter().any(|e| e.kind() == "pane.passthrough"),
        "never replayed"
    );
}
