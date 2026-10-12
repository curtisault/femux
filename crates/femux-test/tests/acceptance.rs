//! 0.1.0 acceptance: create a session, split, run `ls`, assert on the cell grid, detach and
//! reattach with an identical screen.

use std::time::Duration;

use femux_proto::msg::*;
use femux_test::Harness;

#[tokio::test(flavor = "multi_thread")]
async fn create_split_ls_detach_reattach() {
    let h = Harness::start();
    let work = h.dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    for f in ["alpha.txt", "beta.txt"] {
        std::fs::write(work.join(f), "x").unwrap();
    }

    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "acc".into(),
        root: Some(work.to_string_lossy().into()),
        file: None,
    })
    .await
    .unwrap();
    let att = c.attach("acc", 81, 24).await;
    let left = att.snapshot.tabs[0].focus.clone();
    c.wait_text(&left, "$").await;

    let right = c
        .call(LayoutSplit {
            pane_id: left.clone(),
            dir: SplitDir::H,
            ratio: None,
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    c.wait_text(&right, "$").await;
    // The view's size reaches the tab after the resize debounce (REQ-VIEW-004).
    let (l, r) = (left.clone(), right.clone());
    c.wait_for("layout at 81 columns", Duration::from_secs(5), |c| {
        c.panes.get(&l).map(|p| p.rect.clone())
            == Some(Rect {
                x: 0,
                y: 0,
                w: 40,
                h: 24,
            })
            && c.panes.get(&r).map(|p| p.rect.clone())
                == Some(Rect {
                    x: 41,
                    y: 0,
                    w: 40,
                    h: 24,
                })
    })
    .await;

    c.type_text(&right, "ls\n").await;
    c.wait_text(&right, "beta.txt").await;

    // Assert on the JSON cell grid: the row after the command lists both files.
    let grid = c.screen(&right);
    assert_eq!(
        (grid["cols"].as_u64(), grid["rows"].as_u64()),
        (Some(40), Some(24))
    );
    let rows: Vec<String> = grid["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r.as_array()
                .unwrap()
                .iter()
                .map(|c| c["ch"].as_str().unwrap())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect();
    let cmd = rows
        .iter()
        .position(|r| r.starts_with("$ ls"))
        .expect("command row");
    assert!(
        rows[cmd + 1].contains("alpha.txt") && rows[cmd + 1].contains("beta.txt"),
        "{rows:#?}"
    );
    assert_eq!(grid["cells"][cmd + 1][0]["ch"], "a");
    assert_eq!(grid["cells"][cmd + 1][0]["fg"], "default");

    // Let the prompt settle, then remember both screens.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while c.recv(Duration::from_millis(100)).await {}
    let before = (c.screen(&left), c.screen(&right));

    c.call(SessionDetach {
        view_id: att.view_id.clone(),
    })
    .await
    .unwrap();
    drop(c);

    let mut c = h.client().await;
    c.attach("acc", 81, 24).await;
    c.wait_for("both panes", Duration::from_secs(5), |c| {
        c.panes.contains_key(&left) && c.panes.contains_key(&right)
    })
    .await;
    assert!(c.frames[0].full, "first frame after attach is full");
    assert_eq!((c.screen(&left), c.screen(&right)), before);
}
