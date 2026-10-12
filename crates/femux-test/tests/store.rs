//! REQ-STORE-*, REQ-PTY-005 (stable ids), `server.reload`: a daemon writes its state, is
//! restarted against the same store, and the state comes back.

use std::time::{Duration, Instant};

use femux_proto::Event;
use femux_proto::msg::*;
use femux_test::{Client, Harness};

fn restoring(h: &mut Harness) {
    h.restart_with(|c| c.restore.enabled = true);
}

async fn info(c: &mut Client, pane: &str) -> PaneInfoOk {
    c.call(PaneInfo {
        pane_id: pane.into(),
    })
    .await
    .unwrap()
}

/// Waits until `pane`'s foreground command is `cmd`.
async fn wait_fg(c: &mut Client, pane: &str, cmd: &str) -> PaneInfoOk {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let i = info(c, pane).await;
        if i.fg_cmd == cmd {
            return i;
        }
        assert!(Instant::now() < deadline, "{pane} never ran {cmd}: {i:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What must survive a restart: ids, user titles, trees, floats, zoom. Derived titles follow
/// the foreground process and may differ until re-run commands start.
type TabShape = (String, Option<String>, LayoutNode, Vec<FloatNode>, bool);

fn tabs_of(snap: &SessionSnapshot) -> Vec<TabShape> {
    snap.tabs
        .iter()
        .map(|t| {
            (
                t.id.clone(),
                t.custom_title.clone(),
                t.tree.clone(),
                t.floats.clone(),
                t.zoomed,
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn store_004_sessions_come_back_with_their_ids_layout_and_marks() {
    let mut h = Harness::start_with(|c| {
        c.restore.enabled = true;
        c.env.push(("FEMUX_TEST_SECRET".into(), "hunter2".into()));
    });
    let work = h.dir.path().join("work");
    std::fs::create_dir_all(work.join("sub")).unwrap();
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: Some(work.to_string_lossy().into()),
        file: None,
    })
    .await
    .unwrap();
    let att = c.attach("s", 81, 25).await;
    let t1 = att.snapshot.tabs[0].id.clone();
    let a = att.snapshot.tabs[0].focus.clone();
    c.wait_text(&a, "$").await;
    // A split, a float, a mark, metadata and a cwd change in the first tab.
    let b = c
        .call(LayoutSplit {
            pane_id: a.clone(),
            dir: SplitDir::H,
            ratio: Some(Ratio { num: 1, den: 3 }),
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    let f = c
        .call(LayoutSplit {
            pane_id: b.clone(),
            dir: SplitDir::V,
            ratio: None,
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    c.call(LayoutFloat {
        pane_id: f.clone(),
        rect: Some(Rect {
            x: 2,
            y: 2,
            w: 30,
            h: 8,
        }),
    })
    .await
    .unwrap();
    c.call(PaneMark {
        pane_id: b.clone(),
        mark: "q".into(),
    })
    .await
    .unwrap();
    c.call(PaneSetMeta {
        pane_id: a.clone(),
        set: Some([("agent.name".to_owned(), "x".to_owned())].into()),
        unset: None,
    })
    .await
    .unwrap();
    c.call(TabRename {
        tab_id: t1.clone(),
        title: "main".into(),
    })
    .await
    .unwrap();
    c.type_text(&a, "cd sub\n").await;
    let sub = work.join("sub").canonicalize().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while info(&mut c, &a).await.cwd != sub.to_string_lossy() {
        assert!(Instant::now() < deadline, "cwd never changed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // A tab whose command is re-run on restore, and one whose command is not.
    let rerun = c
        .call(TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: Some("sleep 300".into()),
            title: None,
            index: None,
            restore: Some(true),
        })
        .await
        .unwrap();
    let plain = c
        .call(TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: Some("sleep 301".into()),
            title: None,
            index: None,
            restore: None,
        })
        .await
        .unwrap();
    wait_fg(&mut c, &rerun.pane_id, "sleep").await;
    c.call(StoreSnapshot {}).await.unwrap();
    let before = c
        .call(SessionAttach {
            name: "s".into(),
            tab: None,
            cols: Some(81),
            rows: Some(25),
        })
        .await
        .unwrap()
        .snapshot;
    let seq_before = c.hello_ok.as_ref().unwrap().seq;
    // The persisted environment is filtered by [env] allow (REQ-PTY-006, REQ-SEC-005).
    let db = h.cfg.db.clone().unwrap();
    drop(c);

    restoring(&mut h);
    let saved = femux_daemon::store::Store::open(&db, "test")
        .unwrap()
        .saved
        .unwrap();
    let env = &saved.panes[0].env;
    assert!(env.contains_key("PATH"), "{env:?}");
    assert!(!env.contains_key("FEMUX_TEST_SECRET"), "{env:?}");
    assert!(env.keys().all(|k| k == "PATH"
        || k == "HOME"
        || k == "SHELL"
        || k == "LANG"
        || k == "TERM"
        || k == "EDITOR"
        || k.starts_with("LC_")));

    let mut c = h.client().await;
    // REQ-EVT-001: seq keeps counting across the restart.
    assert!(c.hello_ok.as_ref().unwrap().seq >= seq_before);
    let after = c
        .call(SessionAttach {
            name: "s".into(),
            tab: None,
            cols: Some(81),
            rows: Some(25),
        })
        .await
        .unwrap()
        .snapshot;
    // REQ-PTY-005: the same tab and pane ids, the same tree, floats, titles and marks.
    assert_eq!(tabs_of(&after), tabs_of(&before));
    assert_eq!(after.marks.get("q"), Some(&b));
    assert_eq!(after.tabs[0].focus, before.tabs[0].focus);
    let list = c.call(SessionList {}).await.unwrap().sessions;
    assert_eq!(list[0].root, work.to_string_lossy());
    // Panes come back as shells in their cwd, marked restored, with their metadata.
    let ia = info(&mut c, &a).await;
    assert_eq!(ia.cwd, sub.to_string_lossy());
    assert!(ia.restored && !ia.restore);
    assert_eq!(ia.meta.get("agent.name").map(String::as_str), Some("x"));
    assert_eq!(ia.cmd, None);
    // restore = true re-runs the command; without it the pane is a plain shell.
    let ir = wait_fg(&mut c, &rerun.pane_id, "sleep").await;
    assert!(ir.restore && !ir.restored);
    assert_eq!(ir.cmd.as_deref(), Some("sleep 300"));
    let ip = info(&mut c, &plain.pane_id).await;
    assert!(
        ip.restored && ip.cmd.is_none() && ip.fg_cmd != "sleep",
        "{ip:?}"
    );
    // REQ-STORE-008 (daemon side): a command running in a restored pane clears the mark.
    c.attach("s", 81, 25).await;
    c.wait_text(&a, "$").await;
    c.type_text(&a, "sleep 5\n").await;
    wait_fg(&mut c, &a, "sleep").await;
    assert!(!info(&mut c, &a).await.restored);
}

#[tokio::test(flavor = "multi_thread")]
async fn store_004_restore_is_off_by_default() {
    let mut h = Harness::start();
    let mut c = h.client().await;
    let _ = c.new_session("s").await;
    c.call(StoreSnapshot {}).await.unwrap();
    drop(c);
    h.restart();
    let mut c = h.client().await;
    assert!(c.call(SessionList {}).await.unwrap().sessions.is_empty());
    // The state was saved all the same; enabling restore brings it back.
    c.call(SessionCreate {
        name: "t".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    c.call(StoreSnapshot {}).await.unwrap();
    drop(c);
    restoring(&mut h);
    let mut c = h.client().await;
    let names: Vec<String> = c
        .call(SessionList {})
        .await
        .unwrap()
        .sessions
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["t"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn store_005_reruns_are_staggered() {
    let mut h = Harness::start_with(|c| c.restore.stagger_ms = 1500);
    let mut c = h.client().await;
    let _ = c.new_session("s").await;
    let mut panes = Vec::new();
    for n in 0..2 {
        panes.push(
            c.call(TabCreate {
                session: "s".into(),
                cwd: None,
                cmd: Some(format!("sleep 30{n}")),
                title: None,
                index: None,
                restore: Some(true),
            })
            .await
            .unwrap()
            .pane_id,
        );
    }
    c.call(StoreSnapshot {}).await.unwrap();
    drop(c);
    restoring(&mut h);
    let started = Instant::now();
    let mut c = h.client().await;
    wait_fg(&mut c, &panes[0], "sleep").await;
    // The second re-run waits one stagger after the first.
    assert_eq!(info(&mut c, &panes[1]).await.pid, 0, "started too early");
    wait_fg(&mut c, &panes[1], "sleep").await;
    assert!(started.elapsed() >= Duration::from_millis(1400));
}

#[tokio::test(flavor = "multi_thread")]
async fn store_006_scrollback_is_opt_in() {
    for keep in [true, false] {
        let mut h = Harness::start_with(|c| c.restore.scrollback = keep);
        let mut c = h.client().await;
        let (_, pane) = c.new_session("s").await;
        c.attach("s", 80, 24).await;
        c.wait_text(&pane, "$").await;
        c.type_text(
            &pane,
            "i=1; while [ $i -le 60 ]; do echo kept$i; i=$((i+1)); done\n",
        )
        .await;
        c.wait_text(&pane, "kept60").await;
        c.call(StoreSnapshot {}).await.unwrap();
        drop(c);
        restoring(&mut h);
        let mut c = h.client().await;
        c.attach("s", 80, 24).await;
        c.wait_text(&pane, "$").await;
        let read = c
            .call(ScrollbackRead {
                pane_id: pane.clone(),
                from_line: -100,
                count: 200,
            })
            .await
            .unwrap();
        let text: String = read
            .lines
            .iter()
            .map(|l| l.iter().map(|c| c.ch.as_str()).collect::<String>() + "\n")
            .collect();
        assert_eq!(text.contains("kept60"), keep, "scrollback={keep}:\n{text}");
        if keep {
            // History before the screen came back too.
            assert!(text.contains("kept20\n"), "{text}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn store_003_snapshots_follow_layout_changes_and_a_dirty_tick() {
    let h = Harness::start_with(|c| {
        c.snapshot_debounce = Duration::from_millis(200);
        c.snapshot_interval = Duration::from_millis(1500);
    });
    let mut c = h.client().await;
    c.call(EventsSubscribe::default()).await.unwrap();
    // A layout change is saved after the debounce, once for a burst of changes.
    let (tab, pane) = c.new_session("s").await;
    let t0 = Instant::now();
    c.call(TabRename {
        tab_id: tab.clone(),
        title: "x".into(),
    })
    .await
    .unwrap();
    c.wait_event("store.snapshot").await;
    let took = t0.elapsed();
    assert!(took >= Duration::from_millis(150), "{took:?}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    while c.recv(Duration::from_millis(50)).await {}
    let snaps = |c: &Client| {
        c.events
            .iter()
            .filter(|e| matches!(e, Event::StoreSnapshot(_)))
            .count()
    };
    assert_eq!(snaps(&c), 1, "one snapshot for the burst");
    // Metadata is saved on the next tick, not after the debounce.
    c.call(PaneSetMeta {
        pane_id: pane.clone(),
        set: Some([("k".to_owned(), "v".to_owned())].into()),
        unset: None,
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    while c.recv(Duration::from_millis(50)).await {}
    let after_meta = snaps(&c);
    c.wait_for("tick snapshot", Duration::from_secs(5), |c| {
        snaps(c) > after_meta
    })
    .await;
    // A clean daemon takes none.
    let n = snaps(&c);
    tokio::time::sleep(Duration::from_millis(3200)).await;
    while c.recv(Duration::from_millis(50)).await {}
    assert_eq!(snaps(&c), n, "no snapshot while clean");
    // store.snapshot writes now and answers its time.
    let r = c.call(StoreSnapshot {}).await.unwrap();
    assert!(r.ts > 1_700_000_000_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn store_007_a_bad_store_is_backed_up_and_the_daemon_starts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state/test.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"garbage").unwrap();
    let mut cfg = Harness::config(dir.path());
    cfg.db = Some(db.clone());
    cfg.restore.enabled = true;
    let handle = femux_daemon::start(cfg.clone()).unwrap();
    let mut c = Client::connect(&cfg.socket, Client::hello(80, 24))
        .await
        .unwrap();
    assert!(c.call(SessionList {}).await.unwrap().sessions.is_empty());
    let backups: Vec<_> = std::fs::read_dir(dir.path().join("state/session-backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read(&backups[0]).unwrap(), b"garbage");
    // The fresh store works.
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    c.call(StoreSnapshot {}).await.unwrap();
    handle.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn server_reload_applies_the_config_or_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let conf = dir.path().join("config.toml");
    std::fs::write(&conf, "[daemon]\nkeep_alive = false\n").unwrap();
    let h = Harness::start_with(|c| c.config_path = Some(conf.clone()));
    let mut c = h.client().await;
    c.call(EventsSubscribe::default()).await.unwrap();
    // An invalid file is refused with its line, and nothing changes.
    std::fs::write(&conf, "[daemon]\nkeep_alive = true\nbogus = 1\n").unwrap();
    let e = c.call(ServerReload {}).await.unwrap_err();
    assert_eq!(e.code, femux_proto::ErrorCode::Invalid);
    assert!(e.message.contains("line 3"), "{}", e.message);
    assert!(!h.daemon().settings.lock().unwrap().keep_alive);
    // A valid one is applied and announced.
    std::fs::write(
        &conf,
        "[daemon]\nkeep_alive = true\n\n[restore]\nstagger_ms = 7\n",
    )
    .unwrap();
    c.call(ServerReload {}).await.unwrap();
    c.wait_event("config.reloaded").await;
    {
        let s = h.daemon().settings.lock().unwrap();
        assert!(s.keep_alive);
        assert_eq!(s.restore.stagger_ms, 7);
    }
    // keep_alive now holds the daemon up past its last session.
    let _ = c.new_session("s").await;
    c.call(SessionKill { name: "s".into() }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!h.handle.as_ref().unwrap().is_finished());
}
