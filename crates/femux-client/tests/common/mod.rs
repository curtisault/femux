//! Drives the client's [`App`] against a real daemon: requests go out over one connection,
//! a second connection observes the daemon for assertions.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use femux_client::app::{App, Effect};
use femux_client::config::ClientConfig;
use femux_client::keys::key;
use femux_client::render::render;
use femux_client::theme::Theme;
use femux_proto::Request;
use femux_proto::msg as m;
use femux_test::{Client, Harness};
use termwiz::surface::Surface;

pub struct Drive {
    pub h: Harness,
    pub conn: Client,
    pub obs: Client,
    pub app: App,
    pub pane: String,
    pub tab: String,
    pub effects: Vec<Effect>,
    pub sent: Vec<Request>,
}

impl Drive {
    pub async fn new(cfg: ClientConfig) -> Drive {
        Drive::with(cfg, Vec::new(), 80, 24).await
    }

    pub async fn with(
        cfg: ClientConfig,
        plugins: Vec<Box<dyn femux_plugin::ClientPlugin>>,
        cols: u16,
        rows: u16,
    ) -> Drive {
        let h = Harness::start();
        let mut obs = h.client().await;
        obs.call(m::SessionCreate {
            name: "s".into(),
            root: Some(h.dir.path().to_string_lossy().into()),
            file: None,
        })
        .await
        .unwrap();
        // The connection asks for exactly what the real client asks for, so a request of a
        // family it forgot fails here as it would for users.
        let mut hello = femux_test::Client::hello(cols, rows);
        hello.features = femux_client::FEATURES.map(String::from).to_vec();
        let mut conn = femux_test::Client::connect(&h.socket, hello).await.unwrap();
        conn.call(m::EventsSubscribe {
            session: Some("s".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        let area = femux_client::layout::Layout::compute(cols, rows, &cfg, false).panes;
        let attach = conn
            .call(m::SessionAttach {
                name: "s".into(),
                tab: None,
                cols: Some(area.w),
                rows: Some(area.h),
            })
            .await
            .unwrap();
        let pane = attach.snapshot.tabs[0].focus.clone();
        let tab = attach.snapshot.tabs[0].id.clone();
        let app = App::new(
            cfg,
            plugins,
            vec!["ls".into(), "new".into()],
            &attach,
            cols,
            rows,
        );
        let mut d = Drive {
            h,
            conn,
            obs,
            app,
            pane,
            tab,
            effects: Vec::new(),
            sent: Vec::new(),
        };
        d.settle().await;
        d
    }

    /// Sends what the app queued and feeds back everything the daemon says until it has
    /// been quiet for a moment.
    pub async fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let out = self.app.take_out();
            for (id, req) in out {
                self.sent.push(req.clone());
                self.conn.send_with_id(id, req).await;
            }
            self.effects.extend(self.app.take_effects());
            match self.conn.raw_recv(Duration::from_millis(150)).await {
                Some(msg) => self.app.on_msg(msg),
                None => {
                    if self.app.take_out().is_empty() {
                        return;
                    }
                }
            }
            assert!(Instant::now() < deadline, "daemon never went quiet");
        }
    }

    /// Settles until `cond` holds (or panics after 10 s).
    pub async fn until(&mut self, what: &str, mut cond: impl FnMut(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond(&self.app) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}\n{}",
                self.screen()
            );
            self.settle().await;
        }
    }

    /// Presses `chord` (see `femux_client::keys::key`).
    pub async fn press(&mut self, chord: &str) {
        self.app.on_key(key(chord), Instant::now());
        self.settle().await;
    }

    pub async fn press_all(&mut self, chords: &[&str]) {
        for c in chords {
            self.press(c).await;
        }
    }

    /// Requests sent since the last call, by kind.
    pub fn sent_kinds(&mut self) -> Vec<&'static str> {
        std::mem::take(&mut self.sent)
            .iter()
            .map(|r| r.kind())
            .collect()
    }

    /// The rendered screen as text.
    pub fn screen(&mut self) -> String {
        let mut s = Surface::new(self.app.cols as usize, self.app.rows as usize);
        render(&mut self.app, &Theme::dark(), &mut s);
        s.screen_chars_to_string()
    }

    pub fn screen_line(&mut self, y: usize) -> String {
        self.screen()
            .lines()
            .nth(y)
            .unwrap_or_default()
            .trim_end()
            .to_owned()
    }

    /// Text of a pane as the app sees it.
    pub fn pane_text(&self, pane: &str) -> String {
        let Some(screen) = self
            .app
            .model
            .panes
            .get(pane)
            .and_then(|p| p.screen.as_ref())
        else {
            return String::new();
        };
        (0..screen.rows.len())
            .map(|y| screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub async fn wait_pane_text(&mut self, pane: &str, needle: &str) {
        let p = pane.to_owned();
        let n = needle.to_owned();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.pane_text(&p).contains(&n) {
            assert!(
                Instant::now() < deadline,
                "{n:?} never appeared in pane {p}:\n{}",
                self.pane_text(&p)
            );
            self.settle().await;
        }
    }

    pub async fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let chord = match ch {
                '\n' => "enter".to_owned(),
                ' ' => "space".to_owned(),
                c => c.to_string(),
            };
            self.app.on_key(key(&chord), Instant::now());
        }
        self.settle().await;
    }

    pub async fn layout(&mut self) -> m::LayoutGetOk {
        let tab = self.app.model.current.clone();
        self.obs.call(m::LayoutGet { tab_id: tab }).await.unwrap()
    }
}
