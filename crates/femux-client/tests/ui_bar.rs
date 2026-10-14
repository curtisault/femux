//! REQ-UI-020, REQ-UI-030..033 (0.10.0): one bar at the top or bottom with the session chip,
//! mode, tabs and modules; modules in config order on their intervals; a short row gives
//! from the right first; notifications for hidden panes.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::Drive;
use femux_client::config::{BarPosition, ClientConfig};
use femux_client::layout::Layout;
use femux_plugin::{BarCtx, ClientPlugin, InfoModule, Span, Spans};
use femux_proto::msg as m;

fn bar_cfg(bar: BarPosition, modules: &[&str]) -> ClientConfig {
    ClientConfig {
        bar,
        bar_modules: modules.iter().map(|s| s.to_string()).collect(),
        bar_interval: Duration::from_millis(10),
        ..Default::default()
    }
}

fn infobar() -> Box<dyn ClientPlugin> {
    femux_plugin_infobar::plugin().client.unwrap()
}

/// Every built-in module name, plus a `remote:` one.
fn every_module() -> Vec<&'static str> {
    let mut v: Vec<&str> = femux_plugin_infobar::MODULES.to_vec();
    v.push("remote:cpu");
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_030_one_bar_at_the_top_or_bottom() {
    for bar in [BarPosition::Bottom, BarPosition::Top] {
        let cfg = bar_cfg(bar, &["clock"]);
        let mut d = Drive::with(cfg.clone(), vec![infobar()], 100, 30).await;
        let pane = d.pane.clone();
        d.wait_pane_text(&pane, "$").await;
        let l = Layout::compute(100, 30, &cfg, false);
        // The daemon sizes the pane to everything but the bar's row.
        let rect = d.app.model.panes[&pane]
            .screen
            .as_ref()
            .unwrap()
            .rect
            .clone();
        assert_eq!((rect.w, rect.h), (100, 29), "{bar:?}");
        assert_eq!((l.panes.w, l.panes.h), (100, 29));
        let screen: Vec<String> = d.screen().lines().map(str::to_owned).collect();
        let (row, other) = match bar {
            BarPosition::Bottom => (29, 0),
            BarPosition::Top => (0, 29),
        };
        // One cell in: the bare session name, then the tabs, the modules at the end.
        assert!(screen[row].starts_with("  s   1 "), "{bar:?}: {screen:#?}");
        assert!(
            screen[row]
                .trim_end()
                .ends_with(&femux_plugin_infobar::local_time()),
            "{screen:#?}"
        );
        // No second row of chrome: the other edge is pane content.
        assert!(!screen[other].contains(" 1 "), "{screen:#?}");
        // The prompt sits at the pane area's corner.
        assert_eq!(
            screen[l.panes.y as usize].chars().next(),
            Some('$'),
            "{screen:#?}"
        );
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
    let cfg = bar_cfg(BarPosition::Bottom, &["cc", "bb", "aa", "missing"]);
    let mut d = Drive::with(cfg, vec![Box::new(Probes(counts.clone()))], 100, 20).await;
    let t0 = Instant::now();
    d.app.tick(t0);
    // In their configured order at the right end; unknown ones vanish.
    let bar = d.screen_line(19);
    assert!(bar.ends_with("CC  BB  AA"), "{bar}");
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
        bar_cfg(BarPosition::Bottom, &[]),
        vec![Box::new(Probes(unused.clone()))],
        80,
        20,
    )
    .await;
    d.app.tick(Instant::now());
    assert_eq!(*unused[0].1.lock().unwrap(), 0);
}

/// A module with a long full form and a short one.
struct Sized {
    name: &'static str,
    full: &'static str,
    short: &'static str,
}

impl InfoModule for Sized {
    fn name(&self) -> &str {
        self.name
    }
    fn render(&self, ctx: &BarCtx, _w: usize) -> Spans {
        vec![Span::text(if ctx.short { self.short } else { self.full })]
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(1)
    }
}

struct SizedPlugin;

impl ClientPlugin for SizedPlugin {
    fn name(&self) -> &'static str {
        "sized"
    }
    fn modules(&self) -> Vec<Box<dyn InfoModule>> {
        vec![
            Box::new(Sized {
                name: "alpha",
                full: "alpha-full-form",
                short: "A1",
            }),
            Box::new(Sized {
                name: "beta",
                full: "beta-full-form",
                short: "B2",
            }),
        ]
    }
}

/// Which module forms one width of the bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Full,
    Short,
    OneModule,
    NoModule,
}

/// REQ-UI-030 (0.10.0): a short row gives from the right first: modules switch to their short
/// form, then titles shrink to 8 columns, then modules drop from the right end, then titles
/// shrink further; tab numbers always show.
#[tokio::test(flavor = "multi_thread")]
async fn ui_030_a_short_row_gives_from_the_right_first() {
    let cfg = bar_cfg(BarPosition::Bottom, &["alpha", "beta"]);
    let mut d = Drive::with(cfg, vec![Box::new(SizedPlugin)], 200, 20).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    // Three tabs titled like their shells: `sh <temp dir>`, longer than 8 columns.
    d.press_all(&["ctrl+space", "t"]).await;
    d.press_all(&["ctrl+space", "t"]).await;
    d.until("three tabs", |a| a.model.tabs.len() == 3).await;
    let longest = d
        .app
        .model
        .tabs
        .iter()
        .map(|t| d.app.model.tab_title(t).chars().count())
        .max()
        .unwrap();
    assert!(longest > 8);
    // The widest title shown, `…` included.
    let widest = |bar: &str| -> usize {
        bar.split("  ")
            .filter_map(|part| {
                let part = part.trim();
                let (n, title) = part.split_once(' ')?;
                n.parse::<u8>().ok()?;
                Some(title.chars().count())
            })
            .max()
            .unwrap_or(0)
    };
    let mut seen = Vec::new();
    for cols in (30..=200).rev() {
        d.app.on_resize(cols, 20);
        let bar = d.screen_line(19);
        assert!(bar.chars().count() <= cols as usize, "{cols}: {bar}");
        for n in ["1", "2", "3"] {
            assert!(
                bar.contains(&format!(" {n} ")) || bar.ends_with(&format!(" {n}")),
                "{cols}: {bar}"
            );
        }
        let level = match (
            bar.contains("alpha-full"),
            bar.contains("A1"),
            bar.contains("B2"),
        ) {
            (true, _, _) => Level::Full,
            (_, true, true) => Level::Short,
            (_, true, false) => Level::OneModule,
            _ => Level::NoModule,
        };
        assert!(
            !(bar.contains("B2") && !bar.contains("A1")),
            "{cols}: {bar}"
        );
        match level {
            // Full forms only with every title whole.
            Level::Full => assert!(!bar.contains('…'), "{cols}: {bar}"),
            // A module drops only once titles have given way (to 8 columns; dropping it
            // hands its room back to them).
            Level::OneModule => assert!(widest(&bar) < longest, "{cols}: {bar}"),
            _ => {}
        }
        if seen.last() != Some(&level) {
            seen.push(level);
        }
    }
    assert_eq!(
        seen,
        [Level::Full, Level::Short, Level::OneModule, Level::NoModule],
        "the bar gives way in order"
    );
    // The narrowest: titles cut with `…`, every tab number still there.
    d.app.on_resize(30, 20);
    let bar = d.screen_line(19);
    assert!(bar.contains('…') && bar.contains(" 3"), "{bar}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_033_notifications_show_while_their_pane_is_hidden() {
    let cfg = bar_cfg(BarPosition::Bottom, &["notifications"]);
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
    d.press_all(&["ctrl+space", "|"]).await;
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

/// The 0.10.0 bar with every built-in module, at the top and at the bottom.
#[tokio::test(flavor = "multi_thread")]
async fn acceptance_0_10_one_bar_with_every_built_in_module() {
    let modules = every_module();
    for bar in [BarPosition::Bottom, BarPosition::Top] {
        let cfg = bar_cfg(bar, &modules);
        let mut d = Drive::with(cfg, vec![infobar()], 300, 20).await;
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
        let row = d.screen_line(if bar == BarPosition::Top { 0 } else { 19 });
        let clock = femux_plugin_infobar::local_time();
        let second = d.app.model.tab_title(&d.app.model.tabs[1]);
        for w in [
            "cpu ".to_owned(),
            "mem ".to_owned(),
            "net ↓".to_owned(),
            "◐ 1".to_owned(),
            "agent done".to_owned(),
            femux_plugin_infobar::hostname(),
            clock[..4].to_owned(),
            format!(" 2 {second}"),
        ] {
            assert!(row.contains(w.as_str()), "{bar:?} lacks {w:?}:\n{row}");
        }
        // Battery and git show only where there is a battery or a repository.
    }
}

/// REQ-UI-032 (0.6.0): `remote:` modules show what the daemon host's `hosts` plugin publishes,
/// and a client attached through `--host` names the host in its session chip.
#[tokio::test(flavor = "multi_thread")]
async fn ui_032_remote_modules_show_the_daemon_hosts_values() {
    let cfg = bar_cfg(
        BarPosition::Bottom,
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
            assert!(line.contains(" box:s "), "{line}");
            // The plain `cpu` is still the client's own machine.
            assert!(line.matches("cpu").count() >= 2, "{line}");
            break;
        }
        assert!(Instant::now() < deadline, "{line}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
