//! The socket server (REQ-PROTO-001..004, REQ-PROTO-020/021) and connection pumps.

use std::collections::HashMap;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use femux_proto::msg::{self as m, Response};
use femux_proto::{
    ErrorCode, PROTO_VERSION, Push, Request, decode_envelope, encode_error, io_async::read_frame,
};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::bus::Filter;
use crate::daemon::{ConnCtx, Daemon, SERVED};
use crate::state::ConnInfo;

/// Creates the socket directory (0700) and binds the socket (0600). A stale socket with no
/// listener is replaced; a live one is an error.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("socket path has no directory"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::metadata(dir)?;
    // SAFETY: getuid cannot fail.
    if meta.uid() != unsafe { libc::getuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not ours", dir.display()),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a daemon is already listening on {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Whether a peer may talk to us (REQ-PROTO-002): same UID only.
pub fn peer_allowed(peer_uid: u32, our_uid: u32) -> bool {
    peer_uid == our_uid
}

/// Serves until shutdown. Returns after the exit snapshot, SIGHUPing the panes unless they are
/// meant to outlive the daemon (REQ-LIFE-001), and removing the socket.
pub async fn serve(d: Arc<Daemon>, listener: UnixListener) {
    let next_conn = AtomicU64::new(1);
    let mut shutdown = d.shutdown.subscribe();
    // Saved sessions come back before the first client is accepted (REQ-STORE-004).
    if let Some(backup) = d.open_store() {
        // REQ-LOG-005 (REQ-STORE-007): the saved state could not be loaded.
        femux_log::warn("daemon.store", "store.backup")
            .msg(format!(
                "the store could not be loaded; moved to {}",
                backup.display()
            ))
            .field("backup", backup.display().to_string())
            .emit();
    }
    tokio::spawn(resizer(d.clone()));
    tokio::spawn(crate::persist::store_task(d.clone()));
    #[cfg(target_os = "linux")]
    if d.store.lock().unwrap().is_some() {
        tokio::spawn(crate::logind::watch(d.clone()));
    }
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    loop {
        tokio::select! {
            _ = shutdown.wait_for(|s| *s) => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                // Checked before any byte is read.
                match stream.peer_cred() {
                    Ok(c) if peer_allowed(c.uid(), uid) => {}
                    _ => continue,
                }
                let id = next_conn.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(connection(d.clone(), id, stream));
            }
        }
    }
    d.exit_snapshot();
    if d.exit_kills_panes.load(std::sync::atomic::Ordering::SeqCst) {
        d.kill_all();
    }
    let _ = std::fs::remove_file(&d.cfg.socket);
}

async fn resizer(d: Arc<Daemon>) {
    loop {
        match d.run_resizes() {
            Some(at) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(at.into()) => {}
                    _ = d.resize_kick.notified() => {}
                }
            }
            None => d.resize_kick.notified().await,
        }
    }
}

struct Conn {
    ctx: ConnCtx,
    tx: mpsc::Sender<Vec<u8>>,
    /// Held while building+sending a frame or sending events, so a frame queued after an
    /// events_lost was built after its view was marked for a full frame (REQ-PROTO-041).
    order: Arc<tokio::sync::Mutex<()>>,
    events: Option<(u64, JoinHandle<()>)>,
    pumps: HashMap<String, JoinHandle<()>>,
}

async fn connection(d: Arc<Daemon>, id: u64, stream: UnixStream) {
    let (mut rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let writer = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if wr.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });

    let Some(ctx) = handshake(&d, id, &mut rd, &tx).await else {
        drop(tx);
        let _ = writer.await;
        return;
    };
    let mut conn = Conn {
        ctx,
        tx: tx.clone(),
        order: Arc::default(),
        events: None,
        pumps: HashMap::new(),
    };

    while let Ok(Some(frame)) = read_frame(&mut rd).await {
        let env = match decode_envelope(&frame) {
            Ok(e) => e,
            Err(e) => {
                let _ = tx
                    .send(encode_error(0, e.code(), e.to_string()).unwrap())
                    .await;
                continue;
            }
        };
        let rid = env.id;
        let body = crate::logging::trace_body(&env.body);
        let started = std::time::Instant::now();
        let reply = match Request::decode(&env.kind, env.body) {
            Err(e) => encode_error(rid, e.code(), e.to_string()),
            Ok(req) => {
                let kind = req.kind();
                let r = handle(&d, &mut conn, req);
                let took = started.elapsed();
                let reply = match &r {
                    Ok(resp) => resp.encode(rid),
                    Err((code, msg)) => encode_error(rid, *code, msg.clone()),
                };
                let size = reply.as_ref().map_or(0, Vec::len);
                let err = r.as_ref().err().map(|(c, m)| (c.as_str(), m.as_str()));
                crate::logging::answered(id, kind, took, size, err, body);
                reply
            }
        };
        match reply {
            Ok(bytes) => {
                if tx.send(bytes).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                let _ = tx
                    .send(encode_error(rid, ErrorCode::Internal, e.to_string()).unwrap())
                    .await;
            }
        }
        // The request ended the daemon: stop reading and let the reply drain first.
        if conn.ctx.exit.get() {
            break;
        }
    }
    let exit = conn.ctx.exit.get();

    if let Some((sub, task)) = conn.events.take() {
        task.abort();
        d.bus.lock().unwrap().unsubscribe(sub);
    }
    for (_, p) in conn.pumps.drain() {
        p.abort();
    }
    d.conn_closed(id);
    drop(conn);
    drop(tx);
    // The writer ends once every queued message, the last reply included, is written.
    let _ = writer.await;
    if exit {
        d.shutdown.send_replace(true);
    }
}

/// Reads `hello`, answers `hello_ok`, and returns the negotiated context (REQ-PROTO-020/021).
async fn handshake(
    d: &Arc<Daemon>,
    id: u64,
    rd: &mut tokio::net::unix::OwnedReadHalf,
    tx: &mpsc::Sender<Vec<u8>>,
) -> Option<ConnCtx> {
    let frame = read_frame(rd).await.ok()??;
    let env = decode_envelope(&frame).ok()?;
    let fail = |code, msg: &str| encode_error(env.id, code, msg).unwrap();
    if env.kind != "hello" {
        let _ = tx
            .send(fail(ErrorCode::Invalid, "the first message must be hello"))
            .await;
        return None;
    }
    let hello: m::Hello = match femux_proto::from_body(env.body) {
        Ok(h) => h,
        Err(e) => {
            let _ = tx.send(fail(ErrorCode::Invalid, &e.to_string())).await;
            return None;
        }
    };
    if hello.proto != PROTO_VERSION {
        let msg = format!(
            "protocol {} is not supported; this daemon speaks {PROTO_VERSION}",
            hello.proto
        );
        let _ = tx.send(fail(ErrorCode::ProtoVersion, &msg)).await;
        return None;
    }
    let features: Vec<String> = hello
        .features
        .iter()
        .filter(|f| SERVED.contains(&f.as_str()))
        .cloned()
        .collect();
    femux_log::debug("daemon.server", "client.connect")
        .msg(format!("{} connected", hello.client))
        .field("conn", id)
        .field("client", hello.client.clone())
        .field("features", features.clone())
        .field("terminal", hello.term.as_ref().and_then(|t| t.name.clone()))
        .emit();
    let seq = d.bus.lock().unwrap().seq();
    d.state
        .lock()
        .unwrap()
        .conns
        .insert(id, ConnInfo { caps: hello.term });
    let ok = m::HelloOk {
        proto: PROTO_VERSION,
        server: d.cfg.name.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        features: SERVED.iter().map(|s| s.to_string()).collect(),
        seq,
    };
    tx.send(Response::Hello(ok).encode(env.id).ok()?)
        .await
        .ok()?;
    Some(ConnCtx {
        id,
        features,
        exit: Default::default(),
    })
}

/// Requests that need the connection itself; everything else goes to the daemon.
fn handle(d: &Arc<Daemon>, conn: &mut Conn, req: Request) -> Result<Response, (ErrorCode, String)> {
    let feature_ok = |f: &str| conn.ctx.features.iter().any(|x| x == f);
    match req {
        Request::EventsSubscribe(r) if feature_ok("events") => {
            if let Some((sub, task)) = conn.events.take() {
                task.abort();
                d.bus.lock().unwrap().unsubscribe(sub);
            }
            let (sub, notify) = d
                .bus
                .lock()
                .unwrap()
                .subscribe(Filter::from_request(&r), r.since);
            let (d2, tx, conn_id, order) =
                (d.clone(), conn.tx.clone(), conn.ctx.id, conn.order.clone());
            let task = tokio::spawn(async move {
                loop {
                    notify.notified().await;
                    let _ordered = order.lock().await;
                    let events = d2.bus.lock().unwrap().take(sub);
                    for e in events {
                        if matches!(e, femux_proto::Event::EventsLost(_)) {
                            // REQ-PROTO-041: the next frame after events_lost is full.
                            let mut st = d2.state.lock().unwrap();
                            for v in st.views.values_mut().filter(|v| v.conn == conn_id) {
                                v.frames.need_full = true;
                            }
                        }
                        let Ok(bytes) = e.encode(0) else { continue };
                        if tx.send(bytes).await.is_err() {
                            return;
                        }
                    }
                }
            });
            conn.events = Some((sub, task));
            Ok(Response::EventsSubscribe(m::Empty {}))
        }
        Request::EventsUnsubscribe(_) if feature_ok("events") => {
            if let Some((sub, task)) = conn.events.take() {
                task.abort();
                d.bus.lock().unwrap().unsubscribe(sub);
            }
            Ok(Response::EventsUnsubscribe(m::Empty {}))
        }
        Request::FrameSubscribe(r) => {
            let view_id = r.view_id.clone();
            let resp = d.handle(&conn.ctx, Request::FrameSubscribe(r))?;
            if !conn.pumps.contains_key(&view_id) {
                let task = tokio::spawn(frame_pump(
                    d.clone(),
                    view_id.clone(),
                    conn.tx.clone(),
                    conn.order.clone(),
                ));
                conn.pumps.insert(view_id, task);
            }
            Ok(resp)
        }
        Request::SessionDetach(r) => {
            if let Some(p) = conn.pumps.remove(&r.view_id) {
                p.abort();
            }
            d.handle(&conn.ctx, Request::SessionDetach(r))
        }
        req => d.handle(&conn.ctx, req),
    }
}

/// Sends a view's frames: one at a time, each waiting for the previous ack (REQ-PROTO-040).
async fn frame_pump(
    d: Arc<Daemon>,
    view_id: String,
    tx: mpsc::Sender<Vec<u8>>,
    order: Arc<tokio::sync::Mutex<()>>,
) {
    let mut epoch = d.epoch.subscribe();
    let Some(wake) = d
        .state
        .lock()
        .unwrap()
        .views
        .get(&view_id)
        .map(|v| v.frames.wake.clone())
    else {
        return;
    };
    loop {
        let ordered = order.lock().await;
        if !d.state.lock().unwrap().views.contains_key(&view_id) {
            return;
        }
        let frame = crate::frames::build(&d.state, &view_id);
        if let Some(f) = frame {
            let Ok(bytes) = Push::Frame(f).encode(0) else {
                return;
            };
            if tx.send(bytes).await.is_err() {
                return;
            }
        }
        drop(ordered);
        tokio::select! {
            r = epoch.changed() => if r.is_err() { return },
            _ = wake.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_uids_are_refused() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(0, 1000));
        assert!(!peer_allowed(1001, 1000));
    }
}
