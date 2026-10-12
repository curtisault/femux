//! REQ-VT-*: per-pane terminal state, cell diffs, scrollback, coalescing, prompt marks.

use std::time::Duration;

use femux_proto::msg::*;
use femux_test::{Client, Harness};

async fn attached(h: &Harness) -> (Client, String, String) {
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let att = c.attach("s", 80, 24).await;
    let pane = att.snapshot.tabs[0].focus.clone();
    c.wait_text(&pane, "$").await;
    (c, att.view_id, pane)
}

#[tokio::test(flavor = "multi_thread")]
async fn vt_001_each_pane_has_its_own_terminal() {
    let h = Harness::start();
    let (mut c, _, a) = attached(&h).await;
    let b = c
        .call(LayoutSplit {
            pane_id: a.clone(),
            dir: SplitDir::V,
            ratio: None,
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    c.wait_text(&b, "$").await;
    c.type_text(&a, "echo only-in-a\n").await;
    c.wait_text(&a, "only-in-a").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    while c.recv(Duration::from_millis(100)).await {}
    assert!(!c.screen_text(&b).contains("only-in-a"));
}

#[tokio::test(flavor = "multi_thread")]
async fn vt_002_frames_carry_cell_diffs() {
    let h = Harness::start();
    let (mut c, _, pane) = attached(&h).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    while c.recv(Duration::from_millis(100)).await {}
    let n = c.frames.len();
    c.type_text(&pane, "x").await;
    c.wait_text(&pane, "$ x").await;
    let diffs = &c.frames[n..];
    assert!(
        diffs.iter().all(|f| !f.full),
        "typing produces partial frames"
    );
    let rows: usize = diffs
        .iter()
        .flat_map(|f| &f.panes)
        .map(|p| p.cells.len())
        .sum();
    assert!(rows <= 2, "only the changed rows are sent ({rows})");
    for f in &c.frames {
        for p in &f.panes {
            assert!(
                p.cells
                    .iter()
                    .flat_map(|r| &r.run)
                    .all(|cell| !cell.ch.contains('\u{1b}')),
                "no raw escape bytes"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn vt_003_scrollback_default_and_configurable() {
    assert_eq!(femux_daemon::Config::new("x").scrollback_lines, 10_000);
    let h = Harness::start_with(|c| c.scrollback_lines = 100);
    let (mut c, _, pane) = attached(&h).await;
    c.type_text(&pane, "seq 1 5000\n").await;
    c.wait_text(&pane, "\n5000\n$").await;
    let lines = c
        .call(ScrollbackRead {
            pane_id: pane.clone(),
            from_line: -6000,
            count: 6024,
        })
        .await
        .unwrap()
        .lines;
    // 5,000 lines were written; libghostty-vt prunes whole pages, so history stays within a
    // page of the 100-line cap, on either side (see the SPEC Note under REQ-VT-003).
    let history = lines.len() - 24;
    assert!((1..2000).contains(&history), "{history}");
    let last: String = lines[lines.len() - 24..]
        .iter()
        .flatten()
        .map(|c| c.ch.as_str())
        .collect();
    assert!(last.contains("4999"), "{last:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn vt_004_slow_client_gets_newest_state_not_backlog() {
    let h = Harness::start();
    let (mut c, view, pane) = attached(&h).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    while c.recv(Duration::from_millis(100)).await {}
    c.auto_ack = false;
    let n = c.frames.len();
    c.type_text(&pane, "seq 1 20000\n").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    while c.recv(Duration::from_millis(100)).await {}
    assert_eq!(c.frames.len(), n + 1, "one frame in flight, no backlog");
    let seq = c.frames.last().unwrap().frame_seq;
    c.call(FrameAck {
        view_id: view,
        frame_seq: seq,
    })
    .await
    .unwrap();
    c.wait_for("next frame", Duration::from_secs(5), |c| {
        c.frames.len() == n + 2
    })
    .await;
    assert!(
        c.screen_text(&pane).contains("\n20000\n$"),
        "{}",
        c.screen_text(&pane)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vt_005_prompt_marks() {
    let h = Harness::start();
    let (mut c, _, pane) = attached(&h).await;
    c.type_text(
        &pane,
        "printf '\\033]133;A\\007first\\n'; printf '\\033]133;A\\007second\\n'\n",
    )
    .await;
    c.wait_text(&pane, "second").await;
    let prompts = c
        .call(ScrollbackMarks {
            pane_id: pane.clone(),
        })
        .await
        .unwrap()
        .prompts;
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    let mut texts = Vec::new();
    for l in prompts {
        let line = c
            .call(ScrollbackRead {
                pane_id: pane.clone(),
                from_line: l,
                count: 1,
            })
            .await
            .unwrap()
            .lines;
        texts.push(line[0].iter().map(|c| c.ch.as_str()).collect::<String>());
    }
    assert_eq!(texts, ["first", "second"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vt_006_title_and_cwd_captured() {
    let h = Harness::start();
    let (mut c, _, pane) = attached(&h).await;
    c.type_text(
        &pane,
        "printf '\\033]2;my-title\\007\\033]7;file://host/tmp\\007'\n",
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let i = c
            .call(PaneInfo {
                pane_id: pane.clone(),
            })
            .await
            .unwrap();
        if i.title == "my-title" && i.cwd == "/tmp" {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{i:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
