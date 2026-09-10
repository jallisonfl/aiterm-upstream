use serde::{de::DeserializeOwned, de::Error as _, Deserialize, Deserializer, Serialize};
use std::fmt;
use std::io::{self, Cursor, Write};

use crate::terminal::MAX_SCREEN_FRAME_BYTES;

pub const PROTOCOL_VERSION: u16 = 1;
const MAX_TERMINAL_DIMENSION: u16 = 512;
pub const MAX_REQUEST_KIND_BYTES: usize = 64;

const KNOWN_REQUESTS: &[&str] = &[
    "gateway.routes",
    "transport.direct",
    "usage.report",
    "session.list",
    "session.roster",
    "session.star",
    "session.rename",
    "session.bring_in",
    "session.preview",
    "session.conversation",
    "session.spine",
    "session.spine.subscribe",
    "session.changes",
    "session.web_preview",
    "file.read",
    "file.write_markdown",
    "file.render_svg",
    "markdown.parse",
    "session.open",
    "session.close",
    "session.delete",
    "session.fork",
    "session.stop",
    "agent.list",
    "agent.action",
    "tab.list",
    "tab.open",
    "tab.close",
    "terminal.attach",
    "terminal.input",
    "terminal.resize",
    "terminal.detach",
    "terminal.scrollback",
    "terminal.resume",
    "terminal.upload.begin",
    "terminal.upload.chunk",
    "terminal.upload.finish",
    "terminal.upload.cancel",
    // Taking input ownership is its own request because it is a deliberate act.
    // Attaching gives a second client a read-only view; only this says "I am
    // typing now", and the broker announces it to everyone else on the stream.
    "terminal.focus",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEnvelope {
    version: u16,
    request_id: u64,
    kind: String,
    #[serde(with = "serde_bytes")]
    payload: Vec<u8>,
}

#[derive(Deserialize)]
struct RequestEnvelopeProbe {
    request_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRequest {
    request_id: u64,
    kind: String,
    payload: Vec<u8>,
}

impl RemoteRequest {
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        validate_terminal_frame(bytes)?;
        let request_id = ciborium::from_reader::<RequestEnvelopeProbe, _>(bytes)
            .ok()
            .and_then(|probe| probe.request_id);
        let envelope: RequestEnvelope = decode_exact(bytes).map_err(|_| {
            ProtocolError::correlated(
                request_id,
                "protocol.invalid_cbor",
                "invalid request envelope",
            )
        })?;
        if envelope.version != PROTOCOL_VERSION {
            return Err(ProtocolError::correlated(
                Some(envelope.request_id),
                "protocol.unsupported_version",
                "unsupported protocol version",
            ));
        }
        if envelope.kind.len() > MAX_REQUEST_KIND_BYTES {
            return Err(ProtocolError::correlated(
                Some(envelope.request_id),
                "protocol.invalid_request_kind",
                "request kind is too long",
            ));
        }
        if !KNOWN_REQUESTS.contains(&envelope.kind.as_str()) {
            return Err(ProtocolError::correlated(
                Some(envelope.request_id),
                "protocol.unknown_request",
                "unknown request kind",
            ));
        }
        Ok(Self {
            request_id: envelope.request_id,
            kind: envelope.kind,
            payload: envelope.payload,
        })
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// Decode exactly one CBOR value. Ciborium intentionally accepts a valid
/// prefix, so every authoritative wire decoder must also prove EOF.
pub fn decode_exact<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ()> {
    let mut cursor = Cursor::new(bytes);
    let value = ciborium::from_reader(&mut cursor).map_err(|_| ())?;
    if usize::try_from(cursor.position()).ok() != Some(bytes.len()) {
        return Err(());
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemoteEvent {
    pub version: u16,
    pub request_id: u64,
    pub kind: String,
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProtocolError {
    code: &'static str,
    message: &'static str,
    request_id: Option<u64>,
}

impl ProtocolError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            request_id: None,
        }
    }

    fn correlated(request_id: Option<u64>, code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            request_id,
        }
    }

    pub fn invalid_terminal_size() -> Self {
        Self::new(
            "terminal.invalid_size",
            "terminal dimensions must be between 1 and 512",
        )
    }

    pub fn frame_too_large() -> Self {
        Self::new(
            "protocol.frame_too_large",
            "terminal frame exceeds the one mebibyte limit",
        )
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &'static str {
        self.message
    }

    pub fn request_id(&self) -> Option<u64> {
        self.request_id
    }
}

/// Reject a serialized terminal frame before it reaches a remote transport.
pub fn validate_terminal_frame(frame: &[u8]) -> Result<(), ProtocolError> {
    if frame.len() >= MAX_SCREEN_FRAME_BYTES {
        return Err(ProtocolError::frame_too_large());
    }
    Ok(())
}

/// Serialize a typed terminal frame into a capped buffer before it can be
/// handed to a remote sender.
///
/// This bounds only the serialized frame. It is not a raw-byte validation or
/// authorization boundary; transports must still authenticate, authorize, and
/// validate any input they accept before calling application code.
pub fn encode_terminal_frame<T: Serialize>(frame: &T) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = CappedFrameWriter::new();
    let result = ciborium::into_writer(frame, &mut writer);
    if writer.overflowed {
        return Err(ProtocolError::frame_too_large());
    }
    result.map_err(|_| {
        ProtocolError::new("protocol.invalid_frame", "unable to encode terminal frame")
    })?;
    Ok(writer.into_bytes())
}

struct CappedFrameWriter {
    bytes: Vec<u8>,
    overflowed: bool,
}

impl CappedFrameWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            overflowed: false,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for CappedFrameWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() >= MAX_SCREEN_FRAME_BYTES.saturating_sub(self.bytes.len()) {
            self.overflowed = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "terminal frame exceeds maximum size",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TerminalSize {
    cols: u16,
    rows: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalSizeWire {
    cols: u16,
    rows: u16,
}

impl<'de> Deserialize<'de> for TerminalSize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TerminalSizeWire::deserialize(deserializer)?;
        Self::try_new(wire.cols, wire.rows).map_err(D::Error::custom)
    }
}

impl TerminalSize {
    pub fn try_new(cols: u16, rows: u16) -> Result<Self, ProtocolError> {
        if !(1..=MAX_TERMINAL_DIMENSION).contains(&cols)
            || !(1..=MAX_TERMINAL_DIMENSION).contains(&rows)
        {
            return Err(ProtocolError::invalid_terminal_size());
        }
        Ok(Self { cols, rows })
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }
}
