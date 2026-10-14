//! §12 security requirements against a running daemon.

use femux_proto::msg as m;
use femux_test::{Client, Harness};

/// The shell asks for the clipboard (OSC 52 `?`) and prints the reply's bytes with `od`, so
/// the pane shows what the program would have received.
async fn ask_for_clipboard(h: &Harness, bytes: usize) -> (Client, String) {
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 100, 24).await;
    c.wait_text(&pane, "$").await;
    c.call(m::ClipboardSet {
        text: "hello".into(),
    })
    .await
    .unwrap();
    c.type_text(
        &pane,
        &format!(
            "stty raw -echo; printf '\\033]52;c;?\\a'; head -c {bytes} | od -An -c; stty sane; echo done-$((6*7))\n"
        ),
    )
    .await;
    c.wait_text(&pane, "done-42").await;
    (c, pane)
}

/// REQ-SEC-002: by default a pane's clipboard read gets an empty clipboard, never the text;
/// a command in `read_allow` gets the newest clipboard text.
#[tokio::test(flavor = "multi_thread")]
async fn sec_002_clipboard_reads_need_read_allow() {
    let h = Harness::start();
    // The denied reply is `ESC ] 52 ; c ; ESC \`: eight bytes.
    let (c, pane) = ask_for_clipboard(&h, 8).await;
    let text = c.screen_text(&pane);
    assert!(text.contains("5   2   ;   c   ;"), "{text}");
    assert!(
        !text.contains("a   G   V   s"),
        "denied reads carry no text:\n{text}"
    );
    drop(c);
    drop(h);

    // With the shell allowed (and `head`, which may already be in the foreground when the
    // daemon reads the request), the read gets base64 "hello".
    let h = Harness::start_with(|cfg| cfg.read_allow = vec!["sh".into(), "head".into()]);
    let (c, pane) = ask_for_clipboard(&h, 16).await;
    let text = c.screen_text(&pane);
    assert!(text.contains("a   G   V   s   b   G   8   ="), "{text}");
}
