//! The femux session daemon: PTYs, terminal state, the session model, views, the event bus and
//! the socket server. It contains no rendering, keybinding, theme or agent code
//! (REQ-ARCH-001); clients reach it only through `femux-proto` (REQ-ARCH-002).

pub mod bus;
mod daemon;
mod frames;
pub mod layout;
#[cfg(target_os = "linux")]
mod logind;
mod persist;
pub mod pty;
mod server;
mod state;
pub mod store;

use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;

pub use daemon::{Config, Daemon, SERVED, Settings, scheme};
pub use persist::encode_history;
pub use server::peer_allowed;

/// Binds the socket and serves until shutdown (last session closed, `server.stop`, or
/// [`Daemon::shutdown`]).
pub async fn run(cfg: Config) -> io::Result<()> {
    let listener = server::bind(&cfg.socket)?;
    let d = Daemon::new(cfg);
    server::serve(d, listener).await;
    Ok(())
}

/// Like [`run`], but also shuts down on SIGTERM (REQ-LIFE-001): a snapshot first, then exit;
/// panes are sent SIGHUP unless restore is enabled.
pub async fn run_with_signals(cfg: Config) -> io::Result<()> {
    let listener = server::bind(&cfg.socket)?;
    let d = Daemon::new(cfg);
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let d2 = d.clone();
    tokio::spawn(async move {
        term.recv().await;
        // Snapshot before anything else can change, then exit (REQ-STORE-003, REQ-LIFE-001).
        let d3 = d2.clone();
        let _ = tokio::task::spawn_blocking(move || d3.exit_snapshot()).await;
        let restore = d2.settings.lock().unwrap().restore.enabled;
        d2.exit_kills_panes
            .store(!restore, std::sync::atomic::Ordering::SeqCst);
        d2.shutdown.send_replace(true);
    });
    server::serve(d, listener).await;
    Ok(())
}

/// A daemon running on its own thread and runtime, for tests and embedding.
pub struct Handle {
    pub daemon: Arc<Daemon>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// Asks the daemon to stop and waits for it.
    pub fn stop(mut self) {
        self.daemon.shutdown.send_replace(true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Waits for the daemon to exit on its own.
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.daemon.shutdown.send_replace(true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts a daemon on a new thread; returns once the socket is bound.
pub fn start(cfg: Config) -> io::Result<Handle> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let listener = {
        let _guard = rt.enter();
        server::bind(&cfg.socket)?
    };
    let d = Daemon::new(cfg);
    let d2 = d.clone();
    let thread = std::thread::Builder::new()
        .name("femux-daemon".into())
        .spawn(move || {
            rt.block_on(server::serve(d2, listener));
            rt.shutdown_timeout(std::time::Duration::from_secs(1));
        })?;
    Ok(Handle {
        daemon: d,
        thread: Some(thread),
    })
}
