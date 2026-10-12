//! REQ-STORE-003 on Linux: snapshot when logind announces a shutdown or reboot
//! (`PrepareForShutdown(true)`). Best effort: without a system bus or logind this does
//! nothing, and SIGTERM still takes its snapshot. A delay inhibitor lock, when logind grants
//! one, holds the shutdown back until the snapshot is written.

use std::sync::Arc;

use futures_util::StreamExt;
use zbus::zvariant::OwnedFd;

use crate::daemon::Daemon;

const DEST: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const IFACE: &str = "org.freedesktop.login1.Manager";

async fn delay_lock(proxy: &zbus::Proxy<'_>) -> Option<OwnedFd> {
    proxy
        .call(
            "Inhibit",
            &("shutdown", "femux", "Saving sessions", "delay"),
        )
        .await
        .ok()
}

/// What to do with a `PrepareForShutdown` argument: `true` means shutdown is starting.
pub fn should_snapshot(start: bool) -> bool {
    start
}

/// Watches logind until the daemon shuts down or the bus goes away.
pub async fn watch(d: Arc<Daemon>) {
    let Ok(conn) = zbus::Connection::system().await else {
        return;
    };
    let Ok(proxy) = zbus::Proxy::new(&conn, DEST, PATH, IFACE).await else {
        return;
    };
    let Ok(mut signals) = proxy.receive_signal("PrepareForShutdown").await else {
        return;
    };
    let mut lock = delay_lock(&proxy).await;
    let mut shutdown = d.shutdown.subscribe();
    loop {
        let msg = tokio::select! {
            _ = shutdown.wait_for(|s| *s) => return,
            msg = signals.next() => match msg {
                Some(m) => m,
                None => return,
            },
        };
        let Ok(start) = msg.body().deserialize::<bool>() else {
            continue;
        };
        if should_snapshot(start) {
            let d2 = d.clone();
            let _ = tokio::task::spawn_blocking(move || d2.snapshot_now()).await;
            // Releasing the lock lets the shutdown proceed.
            lock = None;
        } else if lock.is_none() {
            // The shutdown was cancelled: hold a lock for the next one.
            lock = delay_lock(&proxy).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_start_of_a_shutdown_snapshots() {
        assert!(should_snapshot(true));
        assert!(!should_snapshot(false));
    }

    /// Without a reachable system bus the watcher gives up at once instead of holding up the
    /// daemon.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_system_bus_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: this test process sets the variable before any thread reads it here.
        unsafe {
            std::env::set_var(
                "DBUS_SYSTEM_BUS_ADDRESS",
                format!("unix:path={}", dir.path().join("none").display()),
            )
        };
        let d = Daemon::new(crate::Config::new("logind-test"));
        tokio::time::timeout(std::time::Duration::from_secs(5), watch(d))
            .await
            .expect("watch returns when there is no bus");
    }
}
