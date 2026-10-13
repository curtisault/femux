//! REQ-UI-030..033: the info bar on any side, modules in config order on their intervals,
//! notifications for hidden panes; and the 0.4.0 acceptance "bar on all four sides with every
//! built-in module".

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::Drive;
use femux_client::config::{BarPosition, ClientConfig};
use femux_client::layout::{BAR_WIDTH, Layout};
use femux_plugin::{BarCtx, ClientPlugin, InfoModule, Span, Spans};
use femux_proto::msg as m;

fn bar_cfg(bar: BarPosition, left: &[&str], right: &[&str]) -> ClientConfig {
    ClientConfig {
        bar,
        bar_left: left.iter().map(|s| s.to_string()).collect(),
        bar_right: right.iter().map(|s| s.to_string()).collect(),
        bar_interval: Duration::from_millis(10),
        ..Default::default()
    }
}

fn infobar() -> Box<dyn ClientPlugin> {
    femux_plugin_infobar::plugin().client.unwrap()
}

/// Every built-in module name, plus a `remote:` one.
fn every_module() -> Vec<&'static str> {
    let mut v: Vec<&str> = femux_plugin_infobar::MODULES
        .iter()
        .copied()
        .filter(|m| *m != "tabs")
        .collect();
    v.push("remote:cpu");
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_030_the_bar_sits_on_any_side_and_the_panes_move_over() {
    for bar in [
        BarPosition::Bottom,
        BarPosition::Top,
        BarPosition::Left,
        BarPosition::Right,
    ] {
        let cfg = bar_cfg(bar, &["session"], &[]);
        let mut d = Drive::with(cfg.clone(), vec![infobar()], 100, 30).await;
        let pane = d.pane.clone();
        d.wait_pane_text(&pane, "$").await;
        let l = Layout::compute(100, 30, &cfg, false);
        // The daemon sizes the pane to the area beside the bar.
        let rect = d.app.model.panes[&pane]
            .screen
            .as_ref()
            .unwrap()
            .rect
            .clone();
        assert_eq!((rect.w, rect.h), (l.panes.w, l.panes.h), "{bar:?}");
        let screen: Vec<String> = d.screen().lines().map(str::to_owned).collect();
        // The prompt is drawn at the pane area's corner.
        let row = &screen[l.panes.y as usize];
        assert_eq!(
            row.chars().nth(l.panes.x as usize),
            Some('$'),
            "{bar:?}: {screen:#?}"
        );
        // The session chip is on the bar's side, one cell in; typing into panes shows no
        // mode label.
        match bar {
            BarPosition::Bottom => assert!(screen[29].starts_with("  session s  "), "{screen:#?}"),
            BarPosition::Top => assert!(screen[0].starts_with("  session s  "), "{screen:#?}"),
            BarPosition::Left => {
                assert!(screen[1].starts_with("  session s "), "{screen:#?}");
                assert!(!screen[4].contains("TERMINAL"), "{screen:#?}");
            }
            BarPosition::Right => {
                let x = (100 - BAR_WIDTH) as usize + 1;
                assert_eq!(&screen[1][x..x + 11], " session s ", "{screen:#?}");
            }
        }
    }
}

/// A module that renders its name in capitals and counts its samples.
struct Probe {
    name: &'static str,
    samples: Arc<Mutex<u32>>,
}

impl InfoModule for Probe {
    fn name(&self) -> &str {
        self.name
    }
    fn sample(&mut self, _ctx: &BarCtx) {
        *self.samples.lock().unwrap() += 1;
    }
    fn render(&self, _ctx: &BarCtx, _w: usize) -> Spans {
        vec![Span::text(self.name.to_uppercase())]
    }
    fn interval(&self) -> Duration {
        Duration::from_millis(200)
    }
}

struct Probes(Vec<(&'static str, Arc<Mutex<u32>>)>);

impl ClientPlugin for Probes {
    fn name(&self) -> &'static str {
        "probes"
    }
    fn modules(&self) -> Vec<Box<dyn InfoModule>> {
        self.0
            .iter()
            .map(|(n, c)| {
                Box::new(Probe {
                    name: n,
                    samples: c.clone(),
                }) as Box<dyn InfoModule>
            })
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_031_modules_come_from_plugins_in_config_order_on_their_interval() {
    let counts: Vec<(&'static str, Arc<Mutex<u32>>)> = vec![
        ("aa", Arc::default()),
        ("bb", Arc::default()),
        ("cc", Arc::default()),
    ];
    let cfg = bar_cfg(
        BarPosition::Bottom,
        &["session", "cc"],
        &["bb", "aa", "missing"],
    );
    let mut d = Drive::with(cfg, vec![Box::new(Probes(counts.clone()))], 100, 20).await;
    let t0 = Instant::now();
    d.app.tick(t0);
    let bar = d.screen_line(19);
    // Left modules follow the mode; right ones in their configured order; unknown ones vanish.
    let (c, b, a) = (
        bar.find("CC").unwrap(),
        bar.find("BB").unwrap(),
        bar.find("AA").unwrap(),
    );
    assert!(c < b && b < a, "{bar}");
    assert!(bar.ends_with("BB │ AA"), "{bar}");
    // Each module samples on its own interval, not every tick.
    for ms in [50, 100, 150, 210, 260, 420] {
        d.app.tick(t0 + Duration::from_millis(ms));
    }
    for (name, n) in &counts {
        assert_eq!(*n.lock().unwrap(), 3, "{name}: first tick, 210 ms, 420 ms");
    }
    // A module not configured is never sampled.
    let unused: Vec<(&'static str, Arc<Mutex<u32>>)> = vec![("zz", Arc::default())];
    let mut d = Drive::with(
        bar_cfg(BarPosition::Bottom, &[], &[]),
        vec![Box::new(Probes(unused.clone()))],
        80,
        20,
    )
    .await;
    d.app.tick(Instant::now());
    assert_eq!(*unused[0].1.lock().unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_033_notifications_show_while_their_pane_is_hidden() {
    let cfg = bar_cfg(BarPosition::Bottom, &["session"], &["notifications"]);
    let mut d = Drive::with(cfg, vec![infobar()], 100, 20).await;
    let first = d.pane.clone();
    d.wait_pane_text(&first, "$").await;
    d.obs.call(m::EventsSubscribe::default()).await.unwrap();
    let post = |pane: Option<&str>, title: &str| m::NotifyPost {
        event: None,
        pane_id: pane.map(str::to_owned),
        title: title.into(),
        level: Some(m::NotifyLevel::Warn),
    };
    // A visible pane's notification is not repeated in the bar.
    d.obs.call(post(Some(&first), "seen here")).await.unwrap();
    d.settle().await;
    d.app.tick(Instant::now());
    assert!(!d.screen_line(19).contains("seen here"));
    // A pane in another tab is hidden: its notification shows.
    d.press_all(&["ctrl+space", "t"]).await;
    d.until("second tab", |a| {
        a.model.tabs.len() == 2 && a.model.current == a.model.tabs[1].id
    })
    .await;
    d.obs
        .call(post(Some(&first), "codex needs y/n"))
        .await
        .unwrap();
    d.settle().await;
    d.app.tick(Instant::now());
    assert!(
        d.screen_line(19).ends_with("codex needs y/n"),
        "{}",
        d.screen_line(19)
    );
    // Back on its tab, it is visible again and leaves the bar.
    d.press_all(&["ctrl+space", "1"]).await;
    d.until("first tab", |a| a.model.current == a.model.tabs[0].id)
        .await;
    assert!(
        !d.screen_line(19).contains("codex"),
        "{}",
        d.screen_line(19)
    );
    // Zoom hides the other panes of the tab (the zoom case of UI-033).
    d.press_all(&["ctrl+space", "v"]).await;
    d.until("two panes", |a| {
        a.model.current_tab().unwrap().panes().len() == 2
    })
    .await;
    d.press_all(&["ctrl+space", "z"]).await;
    d.until("zoomed", |a| a.model.current_tab().unwrap().zoomed)
        .await;
    assert!(
        d.screen_line(19).ends_with("codex needs y/n"),
        "{}",
        d.screen_line(19)
    );
    // A notification about no pane always shows, newest first.
    d.obs.call(post(None, "build done")).await.unwrap();
    d.settle().await;
    assert!(
        d.screen_line(19).ends_with("build done"),
        "{}",
        d.screen_line(19)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn acceptance_0_4_bar_on_all_four_sides_with_every_built_in_module() {
    let right = every_module();
    for bar in [
        BarPosition::Bottom,
        BarPosition::Top,
        BarPosition::Left,
        BarPosition::Right,
    ] {
        let cfg = bar_cfg(bar, &["session", "tabs"], &right);
        let rows = if bar.vertical() { 50 } else { 20 };
        let mut d = Drive::with(cfg, vec![infobar()], 300, rows).await;
        let pane = d.pane.clone();
        d.wait_pane_text(&pane, "$").await;
        d.obs
            .call(m::PaneSetMeta {
                pane_id: pane.clone(),
                set: Some([("agent.state".to_owned(), "working".to_owned())].into()),
                unset: None,
            })
            .await
            .unwrap();
        d.press_all(&["ctrl+space", "t"]).await;
        d.until("tab 2", |a| a.model.tabs.len() == 2).await;
        d.obs
            .call(m::NotifyPost {
                event: None,
                pane_id: Some(pane.clone()),
                title: "agent done".into(),
                level: Some(m::NotifyLevel::Done),
            })
            .await
            .unwrap();
        d.settle().await;
        // Two samples, apart, give the rate modules something to show.
        let t0 = Instant::now();
        d.app.tick(t0);
        std::thread::sleep(Duration::from_millis(300));
        d.app.tick(t0 + Duration::from_millis(1100));
        let screen = d.screen();
        // A column clips to its width, so a long hostname shows only its start there.
        let host: String = femux_plugin_infobar::hostname()
            .chars()
            .take(if bar.vertical() {
                (BAR_WIDTH - 2) as usize
            } else {
                usize::MAX
            })
            .collect();
        let clock = femux_plugin_infobar::local_time();
        let mut want = vec![
            "cpu".to_owned(),
            "mem".to_owned(),
            "net".to_owned(),
            "↓".to_owned(),
            "◐".to_owned(),
            "agent done".to_owned(),
            host,
            clock[..4].to_owned(),
        ];
        if bar.vertical() {
            // Module names label their blocks; tabs list their titles.
            want.extend(["remote:cpu", "hostname", "notifications"].map(String::from));
            // The tab's title is the shell's name: sh, or bash on macOS.
            let second = &d.app.model.tabs[1];
            want.push(format!("2 {}", d.app.model.tab_title(second)));
        } else {
            // A row counts them instead of repeating the strip.
            want.push("tab 2/2".to_owned());
        }
        for w in &want {
            assert!(
                screen.contains(w.as_str()),
                "{bar:?} lacks {w:?}:\n{screen}"
            );
        }
        // Battery and git show only where there is a battery or a repository.
    }
}

/// A module wider than a column bar is clipped to the bar: nothing spills into the panes.
struct Wide;

impl InfoModule for Wide {
    fn name(&self) -> &str {
        "wide"
    }
    fn render(&self, _ctx: &BarCtx, _w: usize) -> Spans {
        // A glyph no tab title or temp directory name contains.
        vec![Span::text("░".repeat(80))]
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(1)
    }
}

struct WidePlugin;

impl ClientPlugin for WidePlugin {
    fn name(&self) -> &'static str {
        "wide"
    }
    fn modules(&self) -> Vec<Box<dyn InfoModule>> {
        vec![Box::new(Wide)]
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_030_a_column_bar_never_draws_over_the_panes() {
    for bar in [BarPosition::Left, BarPosition::Right] {
        let cfg = bar_cfg(bar, &["wide"], &[]);
        let mut d = Drive::with(cfg.clone(), vec![Box::new(WidePlugin)], 100, 30).await;
        let pane = d.pane.clone();
        d.wait_pane_text(&pane, "$").await;
        let l = Layout::compute(100, 30, &cfg, false);
        let screen: Vec<String> = d.screen().lines().map(str::to_owned).collect();
        for (y, row) in screen.iter().enumerate() {
            let in_panes: String = row
                .chars()
                .skip(l.panes.x as usize)
                .take(l.panes.w as usize)
                .collect();
            assert!(!in_panes.contains('░'), "{bar:?} row {y}: {row:?}");
        }
        let count: usize = screen.iter().map(|r| r.matches('░').count()).sum();
        assert_eq!(count, (BAR_WIDTH - 2) as usize, "{bar:?}");
    }
}

/// REQ-UI-032 (0.6.0): `remote:` modules show what the daemon host's `hosts` plugin publishes,
/// and a client attached through `--host` names the host in its session chip.
#[tokio::test(flavor = "multi_thread")]
async fn ui_032_remote_modules_show_the_daemon_hosts_values() {
    let cfg = bar_cfg(
        BarPosition::Bottom,
        &["session"],
        &["remote:hostname", "remote:cpu", "cpu"],
    );
    let hosts = femux_plugin_hosts::plugin().client.unwrap();
    let mut d = Drive::with(cfg, vec![hosts, infobar()], 120, 30).await;
    d.app.remote = Some("box".into());
    d.obs
        .call(m::PluginPublish {
            plugin: "hosts".into(),
            data: [
                ("hostname".to_owned(), "build-box".to_owned()),
                ("cpu".to_owned(), "12.6".to_owned()),
            ]
            .into(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        d.app.tick(Instant::now());
        d.settle().await;
        let line = d.screen_line(29);
        if line.contains("build-box:cpu 13%") {
            assert!(line.contains(" session box:s "), "{line}");
            // The plain `cpu` is still the client's own machine.
            assert!(line.matches("cpu").count() >= 2, "{line}");
            break;
        }
        assert!(Instant::now() < deadline, "{line}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
