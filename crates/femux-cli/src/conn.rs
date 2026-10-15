//! One blocking protocol connection for CLI commands, with the handshake done.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use femux_plugin::CliCore;
use femux_proto::msg as m;
use femux_proto::{Call, ErrorBody, Request, Response, decode_envelope, read_frame};

pub struct Conn {
    s: UnixStream,
    next: u64,
    socket: PathBuf,
    pane: Option<String>,
    /// Events read while waiting for a response, for `next_event`.
    events: std::collections::VecDeque<femux_proto::Event>,
}

impl Conn {
    pub fn open(socket: &Path) -> io::Result<Conn> {
        let mut s = UnixStream::connect(socket).map_err(|e| {
            io::Error::new(e.kind(), format!("no daemon at {}: {e}", socket.display()))
        })?;
        let hello = m::Hello {
            proto: femux_proto::PROTO_VERSION,
            client: format!("femux-cli/{}", env!("CARGO_PKG_VERSION")),
            features: femux_daemon::SERVED.iter().map(|f| f.to_string()).collect(),
            term: None,
        };
        s.write_all(&Request::from(hello).encode(1)?)?;
        let mut c = Conn {
            s,
            next: 2,
            socket: socket.to_path_buf(),
            pane: std::env::var("FEMUX_PANE").ok().filter(|p| !p.is_empty()),
            events: std::collections::VecDeque::new(),
        };
        c.wait(1).map_err(|e| io::Error::other(e.message))?;
        Ok(c)
    }

    fn wait(&mut self, id: u64) -> Result<Response, ErrorBody> {
        loop {
            let frame = read_frame(&mut self.s)
                .map_err(|e| internal(e.to_string()))?
                .ok_or_else(|| internal("connection closed"))?;
            let env = decode_envelope(&frame).map_err(|e| internal(e.to_string()))?;
            if env.id == 0 {
                if let Ok(femux_proto::ServerMsg::Event(e)) = femux_proto::ServerMsg::decode(env) {
                    self.events.push_back(e);
                }
                continue;
            }
            if env.id != id {
                continue;
            }
            if env.kind == femux_proto::ERROR_KIND {
                return Err(femux_proto::from_body(env.body).map_err(|e| internal(e.to_string()))?);
            }
            return Response::decode(&env.kind, env.body).map_err(|e| internal(e.to_string()));
        }
    }

    /// Sends `req` and returns its typed response.
    pub fn call<C: Call>(&mut self, req: C) -> Result<C::Response, ErrorBody> {
        let id = self.next;
        self.next += 1;
        let req = req.into();
        let kind = req.kind();
        let bytes = req.encode(id).map_err(|e| internal(e.to_string()))?;
        self.s
            .write_all(&bytes)
            .map_err(|e| internal(e.to_string()))?;
        let t = std::time::Instant::now();
        let resp = self.wait(id);
        // REQ-LOG-005: proto.response, an error at warn.
        match &resp {
            Ok(_) => femux_log::debug("proto", "proto.response")
                .msg(format!("{kind} answered"))
                .field("kind", kind)
                .field("id", id)
                .field("bytes", bytes.len())
                .field("latency_ms", t.elapsed().as_secs_f64() * 1000.0)
                .emit(),
            Err(e) => femux_log::warn("proto", "proto.error")
                .msg(format!("{kind}: {}: {}", e.code.as_str(), e.message))
                .field("kind", kind)
                .field("code", e.code.as_str())
                .emit(),
        }
        let resp = resp?;
        let bytes = resp.encode(id).map_err(|e| internal(e.to_string()))?;
        let env = decode_envelope(&bytes[4..]).map_err(|e| internal(e.to_string()))?;
        femux_proto::from_body(env.body).map_err(|e| internal(e.to_string()))
    }

    /// The pane this command runs in (`$FEMUX_PANE`, REQ-CLI-002).
    pub fn env_pane(&self) -> Option<String> {
        self.pane.clone()
    }
}

fn internal(msg: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code: m::ErrorCode::Internal,
        message: msg.into(),
        data: None,
    }
}

impl CliCore for Conn {
    fn request(&mut self, req: Request) -> Result<Response, ErrorBody> {
        let id = self.next;
        self.next += 1;
        let bytes = req.encode(id).map_err(|e| internal(e.to_string()))?;
        self.s
            .write_all(&bytes)
            .map_err(|e| internal(e.to_string()))?;
        self.wait(id)
    }
    fn pane(&self) -> Option<String> {
        self.pane.clone()
    }
    fn socket(&self) -> &Path {
        &self.socket
    }
    fn stdin(&mut self) -> String {
        let mut s = String::new();
        let _ = io::Read::read_to_string(&mut io::stdin(), &mut s);
        s
    }
    /// A `Conn` exists only when a daemon answered.
    fn start_daemon(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn next_event(&mut self, timeout: Option<std::time::Duration>) -> Option<femux_proto::Event> {
        let deadline = timeout.map(|t| std::time::Instant::now() + t);
        loop {
            if let Some(e) = self.events.pop_front() {
                return Some(e);
            }
            let left = match deadline {
                Some(d) => {
                    let left = d.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return None;
                    }
                    Some(left)
                }
                None => None,
            };
            self.s.set_read_timeout(left).ok()?;
            let frame = read_frame(&mut self.s);
            let _ = self.s.set_read_timeout(None);
            let env = decode_envelope(&frame.ok()??).ok()?;
            if env.id == 0
                && let Ok(femux_proto::ServerMsg::Event(e)) = femux_proto::ServerMsg::decode(env)
            {
                self.events.push_back(e);
            }
        }
    }
}
