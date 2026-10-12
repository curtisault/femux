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
        let bytes = req.into().encode(id).map_err(|e| internal(e.to_string()))?;
        self.s
            .write_all(&bytes)
            .map_err(|e| internal(e.to_string()))?;
        let resp = self.wait(id)?;
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
}
