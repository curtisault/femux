//! The femux protocol: types generated from `spec/protocol/*.json`, framing and handshake.
//!
//! Wire format (REQ-PROTO-004): a 4-byte big-endian length, then one MessagePack map
//! `{id, kind, body}` ([`Envelope`]). Message types live in [`msg`] and are generated at build
//! time; there are no hand-written message types to drift from the schema.

use std::io;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;

#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod msg {
    use super::{Call, DecodeError, EncodeError, encode_frame, from_body};
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

pub use msg::{Envelope, ErrorBody, ErrorCode, Event, Push, Request, Response};

/// Protocol major version sent in `hello`.
pub const PROTO_VERSION: u32 = 1;

/// Largest frame body accepted or sent (REQ-PROTO-004).
pub const MAX_FRAME: usize = 16 << 20;

/// Kind of an error response.
pub const ERROR_KIND: &str = "error";

/// A request type with its response type and kinds. Implemented for every request body.
pub trait Call: Serialize + Into<Request> {
    const KIND: &'static str;
    const RESPONSE_KIND: &'static str;
    type Response: DeserializeOwned;
}

#[derive(Debug)]
pub enum DecodeError {
    /// The kind is not in the schema (REQ-PROTO-012).
    UnknownKind(String),
    /// The frame or body does not match its schema.
    Invalid(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::UnknownKind(k) => write!(f, "unknown kind {k:?}"),
            DecodeError::Invalid(m) => write!(f, "invalid message: {m}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl DecodeError {
    /// The error code a daemon answers this with.
    pub fn code(&self) -> ErrorCode {
        match self {
            DecodeError::UnknownKind(_) => ErrorCode::UnknownKind,
            DecodeError::Invalid(_) => ErrorCode::Invalid,
        }
    }
}

#[derive(Debug)]
pub enum EncodeError {
    TooLarge(usize),
    Serialize(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::TooLarge(n) => write!(f, "frame of {n} bytes exceeds {MAX_FRAME}"),
            EncodeError::Serialize(m) => write!(f, "cannot encode: {m}"),
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<EncodeError> for io::Error {
    fn from(e: EncodeError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

/// Decodes a message body into its typed form. Unknown fields are ignored.
pub fn from_body<T: DeserializeOwned>(body: rmpv::Value) -> Result<T, DecodeError> {
    // rmpv's own deserializer cannot read unit enum variants from strings, so go through
    // rmp-serde, which matches how bodies are encoded.
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &body).map_err(|e| DecodeError::Invalid(e.to_string()))?;
    rmp_serde::from_slice(&buf).map_err(|e| DecodeError::Invalid(e.to_string()))
}

#[derive(Serialize)]
struct EnvelopeRef<'a, T> {
    id: u64,
    kind: &'a str,
    body: &'a T,
}

/// Encodes one envelope, length prefix included.
pub fn encode_frame<T: Serialize>(id: u64, kind: &str, body: &T) -> Result<Vec<u8>, EncodeError> {
    let mut out = vec![0u8; 4];
    rmp_serde::encode::write_named(&mut out, &EnvelopeRef { id, kind, body })
        .map_err(|e| EncodeError::Serialize(e.to_string()))?;
    let len = out.len() - 4;
    if len > MAX_FRAME {
        return Err(EncodeError::TooLarge(len));
    }
    out[..4].copy_from_slice(&(len as u32).to_be_bytes());
    Ok(out)
}

/// Encodes an error response to request `id`.
pub fn encode_error(
    id: u64,
    code: ErrorCode,
    message: impl Into<String>,
) -> Result<Vec<u8>, EncodeError> {
    encode_frame(
        id,
        ERROR_KIND,
        &ErrorBody {
            code,
            message: message.into(),
            data: None,
        },
    )
}

/// Decodes a frame body (without the length prefix) into an envelope.
pub fn decode_envelope(frame: &[u8]) -> Result<Envelope, DecodeError> {
    rmp_serde::from_slice(frame).map_err(|e| DecodeError::Invalid(e.to_string()))
}

/// A message from the daemon, classified by envelope id and kind.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMsg {
    Response(u64, Response),
    Error(u64, ErrorBody),
    Event(Event),
    Push(Push),
}

impl ServerMsg {
    pub fn decode(env: Envelope) -> Result<Self, DecodeError> {
        if env.id == 0 {
            if Push::KINDS.contains(&env.kind.as_str()) {
                Push::decode(&env.kind, env.body).map(ServerMsg::Push)
            } else {
                Event::decode(&env.kind, env.body).map(ServerMsg::Event)
            }
        } else if env.kind == ERROR_KIND {
            from_body(env.body).map(|e| ServerMsg::Error(env.id, e))
        } else {
            Response::decode(&env.kind, env.body).map(|r| ServerMsg::Response(env.id, r))
        }
    }
}

/// The feature (message family) a request kind belongs to.
pub fn feature_of(kind: &str) -> &str {
    kind.split('.').next().unwrap_or(kind)
}

/// Splits a byte stream into frames. Feed it bytes; take whole frames out.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete frame body, if one is buffered.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, DecodeError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;
        if len > MAX_FRAME {
            return Err(DecodeError::Invalid(format!(
                "frame of {len} bytes exceeds {MAX_FRAME}"
            )));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let frame = self.buf[4..4 + len].to_vec();
        self.buf.drain(..4 + len);
        Ok(Some(frame))
    }
}

/// Reads one frame body. `Ok(None)` on clean EOF at a frame boundary.
pub fn read_frame(r: &mut impl io::Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = check_len(u32::from_be_bytes(len))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn check_len(len: u32) -> io::Result<usize> {
    let len = len as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds {MAX_FRAME}"),
        ));
    }
    Ok(len)
}

#[cfg(feature = "tokio")]
pub mod io_async {
    //! Frame I/O over tokio streams.
    use std::io;

    use tokio::io::{AsyncRead, AsyncReadExt};

    /// Reads one frame body. `Ok(None)` on clean EOF at a frame boundary.
    pub async fn read_frame(r: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<Vec<u8>>> {
        let mut len = [0u8; 4];
        match r.read_exact(&mut len).await {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = super::check_len(u32::from_be_bytes(len))?;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await?;
        Ok(Some(buf))
    }
}

/// Directory holding server sockets (REQ-PROTO-001): `$XDG_RUNTIME_DIR/femux` on Linux,
/// `$TMPDIR/femux-<uid>` on macOS. Linux without `XDG_RUNTIME_DIR` falls back to
/// `/tmp/femux-<uid>`.
pub fn socket_dir() -> PathBuf {
    socket_dir_from(
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        std::env::var_os("TMPDIR").map(PathBuf::from),
        uid(),
        cfg!(target_os = "macos"),
    )
}

fn socket_dir_from(xdg: Option<PathBuf>, tmp: Option<PathBuf>, uid: u32, macos: bool) -> PathBuf {
    match xdg.filter(|p| !macos && p.is_absolute()) {
        Some(x) => x.join("femux"),
        None => tmp
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| "/tmp".into())
            .join(format!("femux-{uid}")),
    }
}

/// Socket path for server `name` (REQ-PROTO-001).
pub fn socket_path(name: &str) -> PathBuf {
    socket_dir().join(format!("{name}.sock"))
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { getuid() }
}

unsafe extern "C" {
    fn getuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::msg::*;
    use super::*;

    #[test]
    fn frame_roundtrip_and_classification() {
        let req = SessionCreate {
            name: "work".into(),
            root: None,
            file: None,
        };
        let bytes = Request::from(req.clone()).encode(7).unwrap();
        let mut d = FrameDecoder::default();
        d.push(&bytes[..3]);
        assert!(d.next_frame().unwrap().is_none());
        d.push(&bytes[3..]);
        let env = decode_envelope(&d.next_frame().unwrap().unwrap()).unwrap();
        assert_eq!((env.id, env.kind.as_str()), (7, "session.create"));
        assert_eq!(
            Request::decode(&env.kind, env.body).unwrap(),
            Request::SessionCreate(req)
        );

        let bytes = encode_error(7, ErrorCode::NotFound, "no such session").unwrap();
        let env = decode_envelope(&bytes[4..]).unwrap();
        match ServerMsg::decode(env).unwrap() {
            ServerMsg::Error(7, e) => assert_eq!(e.code, ErrorCode::NotFound),
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn unknown_kind_and_fields() {
        let bytes = encode_frame(1, "session.frobnicate", &SessionList {}).unwrap();
        let env = decode_envelope(&bytes[4..]).unwrap();
        let err = Request::decode(&env.kind, env.body).unwrap_err();
        assert_eq!(err.code(), ErrorCode::UnknownKind);

        #[derive(serde::Serialize)]
        struct Extra {
            name: &'static str,
            future_field: u32,
        }
        let bytes = encode_frame(
            1,
            "session.kill",
            &Extra {
                name: "a",
                future_field: 3,
            },
        )
        .unwrap();
        let env = decode_envelope(&bytes[4..]).unwrap();
        let req = Request::decode(&env.kind, env.body).unwrap();
        assert_eq!(req, Request::SessionKill(SessionKill { name: "a".into() }));
    }

    #[test]
    fn oversized_frames_rejected() {
        let mut d = FrameDecoder::default();
        d.push(&((MAX_FRAME as u32) + 1).to_be_bytes());
        assert!(d.next_frame().is_err());
        let big = ClipboardSet {
            text: "x".repeat(MAX_FRAME),
        };
        assert!(matches!(
            Request::from(big).encode(1),
            Err(EncodeError::TooLarge(_))
        ));
    }

    #[test]
    fn socket_dirs() {
        let x = Some(PathBuf::from("/run/user/1000"));
        let t = Some(PathBuf::from("/var/folders/xy/T"));
        assert_eq!(
            socket_dir_from(x.clone(), t.clone(), 1000, false),
            PathBuf::from("/run/user/1000/femux")
        );
        assert_eq!(
            socket_dir_from(x, t.clone(), 501, true),
            PathBuf::from("/var/folders/xy/T/femux-501")
        );
        assert_eq!(
            socket_dir_from(None, None, 1000, false),
            PathBuf::from("/tmp/femux-1000")
        );
    }
}
