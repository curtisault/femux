//! What the daemon writes to the log (SPEC §3.9, REQ-LOG-005): requests and their answers,
//! the events it publishes, and the metric counters behind `metrics` (REQ-LOG-006). What may
//! be written follows REQ-LOG-004: request bodies only at `trace`, with typed text, pastes,
//! clipboard text, payloads and command lines replaced by their size or program name.

use std::time::Duration;

use femux_log::Level;
use femux_proto::Event;
use serde_json::{Value, json};

/// Requests a busy client sends many times a second: logged one by one only at `trace`, and
/// always counted in `metrics`.
const HIGH_RATE: &[&str] = &[
    "frame.ack",
    "pane.input",
    "pane.info",
    "pane.resize",
    "scrollback.read",
    "image.read",
];

/// Body fields that hold what REQ-LOG-004 keeps out of the log.
const SECRET_KEYS: &[&str] = &["text", "data", "events", "env", "history"];

/// A request body as `trace` may log it.
pub fn redact(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| {
                    let v = if SECRET_KEYS.contains(&k.as_str()) {
                        json!({"redacted": serde_json::to_string(&v).map_or(0, |s| s.len())})
                    } else if k == "cmd" {
                        v.as_str()
                            .map_or(Value::Null, |c| femux_log::program(c).into())
                    } else {
                        redact(v)
                    };
                    (k, v)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(redact).collect()),
        v => v,
    }
}

/// The body of a request for `trace`, decoded from its wire form.
pub fn trace_body(body: &rmpv::Value) -> Option<Value> {
    if !femux_log::enabled(Level::Trace) {
        return None;
    }
    femux_proto::from_body::<Value>(body.clone())
        .ok()
        .map(redact)
}

/// One answered request (REQ-LOG-005 `proto.response`, an error at `warn`).
pub fn answered(
    conn: u64,
    kind: &str,
    took: Duration,
    reply_bytes: usize,
    error: Option<(&str, &str)>,
    body: Option<Value>,
) {
    if femux_log::debug_mode() {
        femux_log::count(&format!("requests.{kind}"), 1);
        femux_log::time(&format!("request.{kind}"), took);
    }
    if let Some((code, msg)) = error {
        femux_log::warn("proto", "proto.error")
            .msg(format!("{kind}: {code}: {msg}"))
            .field("conn", conn)
            .field("kind", kind)
            .field("code", code)
            .field("message", msg)
            .emit();
        return;
    }
    let level = if HIGH_RATE.contains(&kind) {
        Level::Trace
    } else {
        Level::Debug
    };
    let r = femux_log::rec(level, "proto", "proto.response");
    if !r.on() {
        return;
    }
    let mut r = r
        .msg(format!(
            "{kind} answered in {:.1} ms",
            took.as_secs_f64() * 1000.0
        ))
        .field("conn", conn)
        .field("kind", kind)
        .field("bytes", reply_bytes)
        .field("latency_ms", took.as_secs_f64() * 1000.0);
    if let Some(b) = body {
        r = r.field("body", b);
    }
    r.emit();
}

/// Every event the bus publishes (REQ-LOG-005 `bus.event`), and the records some of them stand
/// for. Output and passthrough are only counted: their bytes stay out (REQ-LOG-004).
pub fn published(e: &Event) {
    if !femux_log::enabled(Level::Info) && !femux_log::enabled(Level::Debug) {
        return;
    }
    let kind = e.kind();
    if femux_log::debug_mode() {
        femux_log::count(&format!("events.{kind}"), 1);
    }
    match e {
        Event::PaneOutput(_) | Event::PanePassthrough(_) => return,
        Event::PaneExited(x) => femux_log::info("daemon.pty", "pane.exit")
            .pane(&x.pane_id)
            .msg(format!("pane exited with {}", x.code))
            .field("code", x.code)
            .emit(),
        Event::PaneFgChanged(x) => femux_log::debug("daemon.pty", "pane.fg")
            .pane(&x.pane_id)
            .msg(format!("foreground is {}", femux_log::program(&x.cmd)))
            .field("pid", x.pid)
            .field("program", femux_log::program(&x.cmd))
            .emit(),
        Event::ViewAttached(x) => femux_log::info("daemon.view", "client.attach")
            .view(&x.view_id)
            .session(&x.session)
            .msg(format!("a view attached to {}", x.session))
            .emit(),
        Event::ViewDetached(x) => femux_log::info("daemon.view", "client.detach")
            .view(&x.view_id)
            .session(&x.session)
            .msg(format!("a view detached from {}", x.session))
            .emit(),
        Event::ConfigReloaded(_) => femux_log::info("daemon", "config.reload")
            .msg("config reloaded")
            .emit(),
        _ => {}
    }
    femux_log::debug("daemon.bus", "bus.event")
        .msg(kind)
        .field("kind", kind)
        .field("seq", e.seq())
        .emit();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_004_trace_bodies_keep_secrets_out() {
        let body = json!({
            "pane_id": "P",
            "events": [{"t": "paste", "text": "hunter2"}],
            "cmd": "TOKEN=abc deploy --password hunter2",
            "nested": {"text": "clipboard secret", "ok": 1},
        });
        let r = redact(body);
        let s = r.to_string();
        assert!(
            !s.contains("hunter2") && !s.contains("abc") && !s.contains("secret"),
            "{s}"
        );
        assert_eq!(r["pane_id"], "P");
        assert_eq!(r["cmd"], "deploy");
        assert_eq!(r["nested"]["ok"], 1);
        assert!(r["events"]["redacted"].as_u64().unwrap() > 0);
    }
}
