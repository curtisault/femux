//! REQ-NOTIFY-001/002: the notify plugin's daemon side raises `[notify]` events and runs
//! webhooks, against a live daemon.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use femux_config::Config;
use femux_plugin::run_daemon_plugin;
use femux_plugin_notify::{Actions, Daemon};
use femux_proto::Event;
use femux_proto::msg as m;
use femux_test::{Client, Harness};

fn actions(toml: &str) -> Actions {
    Config::parse(toml).unwrap().notify
}

async fn set(c: &mut Client, pane: &str, kv: &[(&str, &str)]) {
    c.call(m::PaneSetMeta {
        pane_id: pane.into(),
        set: Some(
            kv.iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ),
        unset: None,
    })
    .await
    .unwrap();
}

fn posted(c: &Client) -> Vec<m::NotifyPosted> {
    c.events
        .iter()
        .filter_map(|e| match e {
            Event::NotifyPosted(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_001_agent_states_and_exits_become_notifications() {
    let h = Harness::start();
    let socket = h.socket.clone();
    let a = actions(
        "[notify]\nagent_done = { sound = \"bell\" }\nagent_error = { desktop = true }\npane_exited = { desktop = false }\n",
    );
    let task = tokio::spawn(async move {
        run_daemon_plugin(&socket, Box::new(Daemon::with(a, Box::new(|_, _| {})))).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.call(m::EventsSubscribe {
        kinds: Some(vec!["notify.posted".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    set(
        &mut c,
        &pane,
        &[("agent.state", "working"), ("agent.name", "claude")],
    )
    .await;
    set(&mut c, &pane, &[("agent.state", "blocked")]).await; // not configured
    set(&mut c, &pane, &[("agent.state", "done")]).await;
    set(&mut c, &pane, &[("agent.state", "done")]).await; // a repeat is not news
    set(&mut c, &pane, &[("agent.state", "error")]).await;
    c.wait_for("two notifications", Duration::from_secs(10), |c| {
        posted(c).len() >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    while c.recv(Duration::from_millis(100)).await {}
    let got: Vec<(String, String, m::NotifyLevel, bool)> = posted(&c)
        .into_iter()
        .map(|n| (n.event.unwrap(), n.title, n.level, n.shown))
        .collect();
    assert_eq!(
        got,
        [
            (
                "agent_done".into(),
                "claude done".into(),
                m::NotifyLevel::Done,
                false
            ),
            (
                "agent_error".into(),
                "claude error".into(),
                m::NotifyLevel::Error,
                false
            ),
        ]
    );
    // The pane's shell exits: pane_exited (another session keeps the daemon running).
    c.new_session("keep").await;
    c.type_text(&pane, "exit 3\n").await;
    c.wait_for("pane_exited", Duration::from_secs(10), |c| {
        posted(c)
            .iter()
            .any(|n| n.event.as_deref() == Some("pane_exited") && n.title.ends_with("(3)"))
    })
    .await;
    task.abort();
}

/// A one-shot HTTP server: returns the body of the first request.
fn serve_once() -> (String, mpsc::Receiver<String>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hook", l.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut r = BufReader::new(s);
        let mut len = 0;
        let mut first = String::new();
        r.read_line(&mut first).unwrap();
        loop {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        r.read_exact(&mut body).unwrap();
        let mut s = r.into_inner();
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
        tx.send(format!(
            "{} {}",
            first.trim(),
            String::from_utf8(body).unwrap()
        ))
        .unwrap();
    });
    (url, rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_001_webhooks_post_json() {
    let (url, rx) = serve_once();
    let h = Harness::start();
    let socket = h.socket.clone();
    let a = actions(&format!(
        "[notify]\nagent_error = {{ webhook = \"{url}\" }}\n"
    ));
    let task =
        tokio::spawn(async move { run_daemon_plugin(&socket, Box::new(Daemon::new(a))).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    set(
        &mut c,
        &pane,
        &[("agent.state", "error"), ("agent.name", "codex")],
    )
    .await;
    let got = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
        .await
        .unwrap()
        .expect("webhook called");
    assert!(got.starts_with("POST /hook HTTP/1.1 "), "{got}");
    let json = &got[got.find('{').unwrap()..];
    let body: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(body["event"], "agent_error");
    assert_eq!(body["title"], "codex error");
    assert_eq!(body["level"], "error");
    assert_eq!(body["pane_id"], pane.as_str());
    task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_002_only_when_hidden_webhooks_skip_shown_panes() {
    let (tx, rx) = mpsc::channel::<serde_json::Value>();
    let tx = std::sync::Mutex::new(tx);
    let h = Harness::start();
    let socket = h.socket.clone();
    let a = actions("[notify]\nagent_done = { webhook = \"http://x\", only_when_hidden = true }\n");
    let poster = Box::new(move |_url: String, body: serde_json::Value| {
        tx.lock().unwrap().send(body).unwrap();
    });
    let task =
        tokio::spawn(
            async move { run_daemon_plugin(&socket, Box::new(Daemon::with(a, poster))).await },
        );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    // No view shows the pane: the webhook runs.
    set(&mut c, &pane, &[("agent.state", "done")]).await;
    let first = tokio::task::spawn_blocking(move || {
        let v = rx.recv_timeout(Duration::from_secs(10));
        (v, rx)
    })
    .await
    .unwrap();
    let (v, rx) = first;
    assert_eq!(v.unwrap()["shown"], false);
    // A client attaches and shows it: the next done stays quiet.
    let mut viewer = h.client().await;
    viewer.attach("s", 80, 24).await;
    set(&mut c, &pane, &[("agent.state", "working")]).await;
    set(&mut c, &pane, &[("agent.state", "done")]).await;
    let none = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_millis(800)))
        .await
        .unwrap();
    assert!(none.is_err(), "{none:?}");
    task.abort();
}
