//! The kb-term/1 wire contract: text frames, binary frames, and the runner RPC.
//!
//! The TypeScript twin (tap) and kb-server mirror this module; it is pinned by
//! the golden JSON files under `tests/golden`. Later changes are additive
//! only, with the goldens updated in the same change. Server-side types
//! tolerate additive fields on read (no `deny_unknown_fields`) so the twin can
//! ignore unknown keys; client and request types use `deny_unknown_fields` so
//! malformed input fails loudly. All types serialize `None` as an explicit
//! `null` (no `skip_serializing_if`) and the goldens record those nulls.
//!
//! The runner talks JSON over runner.sock; kb-termd (runner), the kb-server
//! relay, the local terminal client, the tap, and the SPA all consume these
//! shapes. Binary frames carry an 8-byte big-endian u64 absolute offset of
//! payload byte 0 followed by the payload.
//!
//! [`Harness`] mirrors `kb_core::sessions::HARNESSES` in kb-core without a
//! dependency edge; a drift test pinning [`Harness::ALL`] to that constant
//! lands in a later unit (this crate stays kb-core-free on purpose).

use serde::{Deserialize, Serialize};

/// The wire protocol name for this contract.
pub const PROTO: &str = "kb-term/1";

/// A harness known to the terminal substrate (closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
    /// OpenCode.
    Opencode,
    /// Grok.
    Grok,
    /// Kimi.
    Kimi,
    /// Omp.
    Omp,
}

impl Harness {
    /// Every harness in wire order; the order is significant and pinned.
    pub const ALL: [Harness; 6] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Opencode,
        Harness::Grok,
        Harness::Kimi,
        Harness::Omp,
    ];

    /// The lowercase wire name of this harness.
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Grok => "grok",
            Harness::Kimi => "kimi",
            Harness::Omp => "omp",
        }
    }
}

/// Whether `s` is a valid terminal session id.
///
/// Ids are 8..=12 characters drawn from `[a-z0-9]` only.
pub fn is_valid_term_id(s: &str) -> bool {
    (8..=12).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Server-to-client text frames (tag `t`, lowercase variant names, snake_case
/// fields). Unknown fields are tolerated on read so newer servers stay
/// readable by older clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ServerFrame {
    /// Protocol and session handshake.
    Hello {
        /// Protocol name, e.g. [`PROTO`].
        proto: String,
        /// Session id.
        session: String,
        /// Stream epoch.
        epoch: String,
        /// Harness running in the session.
        harness: Harness,
        /// Bound session id, if any.
        bound_sid: Option<String>,
    },
    /// Terminal profile the viewer should apply.
    Profile {
        /// xterm version, e.g. `"6.0.0"`.
        xterm: String,
        /// Serialize version, e.g. `"0.14.0"`.
        serialize: String,
        /// Width table, e.g. `"unicode-graphemes@0.4.0"`.
        width: String,
        /// Image protocol version, e.g. `"0.9.0"`.
        image: String,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
        /// Scrollback lines.
        scrollback: u32,
        /// Whether scroll-on-erase-in-display is enabled.
        scroll_on_erase_in_display: bool,
        /// Cell width in pixels.
        cell_px_w: u16,
        /// Cell height in pixels.
        cell_px_h: u16,
        /// Viewer theme, if any (null until a theme ships).
        theme: Option<serde_json::Value>,
    },
    /// A stored image definition.
    Imagedef {
        /// Per-epoch marker sequence (0 for join-time defs).
        seq: u64,
        /// Image id.
        id: u32,
        /// Image generation.
        gen: u32,
        /// sha256 of the image bytes.
        sha256: String,
        /// MIME type, e.g. `"image/png"`.
        mime: String,
        /// Image width in pixels.
        w: u32,
        /// Image height in pixels.
        h: u32,
        /// Image width in cells.
        cols: u16,
        /// Image height in cells.
        rows: u16,
        /// Stream position after which this definition applies.
        offset: u64,
    },
    /// Seed marker: buffered bytes from `offset` for `len` bytes.
    Seed {
        /// Absolute offset of the first buffered byte.
        offset: u64,
        /// Number of buffered bytes.
        len: u64,
    },
    /// Grid resize at a stream offset.
    Resize {
        /// Per-epoch marker sequence.
        seq: u64,
        /// Absolute offset of this event.
        offset: u64,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
    },
    /// Grid size change with the giver and offset.
    Sizechanged {
        /// Per-epoch marker sequence.
        seq: u64,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
        /// Who took the grid.
        by: String,
        /// Absolute offset of this event.
        offset: u64,
    },
    /// Graphics reset at a generation.
    Gfxreset {
        /// Per-epoch marker sequence.
        seq: u64,
        /// Image generation.
        gen: u32,
        /// Stream position after which this reset applies.
        offset: u64,
    },
    /// Session exit.
    Exit {
        /// Per-epoch marker sequence.
        seq: u64,
        /// Exit code, if any.
        code: Option<i32>,
        /// Absolute offset of this event.
        offset: u64,
    },
    /// Clipboard write from OSC 52, delivered per-viewer (never replayed).
    Clipboard {
        /// Stream position the write applies at.
        offset: u64,
        /// base64 payload.
        data_b64: String,
    },
    /// Frame-level error.
    Error {
        /// Machine-readable code.
        code: String,
        /// Human-readable message.
        msg: String,
    },
}

/// Client-to-server text frames (tag `t`). Unknown fields are rejected so a
/// typo never silently drops input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase", deny_unknown_fields)]
pub enum ClientFrame {
    /// Resume the stream from an absolute offset.
    Resumefrom {
        /// Absolute offset to resume from.
        offset: u64,
        /// Highest marker seq the client applied (0 if none).
        mseq: u64,
        /// Stream epoch the offset belongs to.
        epoch: String,
    },
    /// Request grid size.
    Takesize {
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
    },
    /// Terminal input bytes as a string.
    Input {
        /// Input data.
        data: String,
    },
    /// Keepalive probe.
    Ping {},
}

/// Largest legal stream offset: offsets are JS-safe integers (< 2^53).
pub const MAX_OFFSET: u64 = 1 << 53;

/// Error decoding a kb-term/1 frame (protocol error "bad-frame").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The frame is shorter than the 8-byte offset header.
    #[error("short binary frame")]
    ShortBinaryFrame,
    /// Malformed JSON or a frame without a string `t` discriminator.
    #[error("bad-frame: malformed frame")]
    MalformedJson,
    /// A header offset at or above 2^53.
    #[error("bad-frame: offset out of range")]
    OffsetOutOfRange,
    /// A malformed tap.sock envelope (zero length, bad kind, bad id).
    #[error("bad-frame: malformed envelope")]
    BadEnvelope,
}

/// Encode a binary frame: an 8-byte big-endian offset, then the payload.
pub fn encode_binary(offset: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + payload.len());
    buf.extend_from_slice(&offset.to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Decode a binary frame into its offset and payload.
///
/// An empty payload is valid; a frame under 8 bytes is an error, as is any
/// header offset at or above 2^53 (the wire protocol error "bad-frame").
pub fn decode_binary(frame: &[u8]) -> Result<(u64, &[u8]), WireError> {
    if frame.len() < 8 {
        return Err(WireError::ShortBinaryFrame);
    }
    let mut header = [0u8; 8];
    header.copy_from_slice(&frame[..8]);
    let offset = u64::from_be_bytes(header);
    if offset >= MAX_OFFSET {
        return Err(WireError::OffsetOutOfRange);
    }
    Ok((offset, &frame[8..]))
}

/// A decoded kb-term/1 text frame, any side, or an unknown `t`.
///
/// The decode is hand-written: parse to `serde_json::Value`, match on `t`,
/// deserialize the known variant. An unknown `t` yields `Unknown` holding
/// the raw input string; re-encode returns that exact string (internally
/// tagged enums cannot express this, and `Value` maps sort keys, so frames
/// are never re-serialised through `Value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFrame {
    /// A server-to-client frame.
    Server(ServerFrame),
    /// A client-to-server frame.
    Client(ClientFrame),
    /// A tap.sock control message (envelope kind 2).
    Control(ControlMessage),
    /// An unknown `t`: the raw input, re-encoded verbatim.
    Unknown(String),
}

/// Reject a text-frame `u64` offset/seq at or above 2^53, matching the TS
/// `reqUint` cap (`Number.MAX_SAFE_INTEGER`): both decoders reject the
/// same frames. The binary header already enforces this bound; text frames
/// must too (offsets are JS-safe integers, brief rule 1). Applies only to
/// known fields; tolerated unknown fields on server frames are ignored by
/// both sides as well.
fn check_js_safe(vals: &[u64]) -> Result<(), WireError> {
    for v in vals {
        if *v >= MAX_OFFSET {
            return Err(WireError::OffsetOutOfRange);
        }
    }
    Ok(())
}

/// Decode one kb-term/1 text frame (any side). Malformed JSON or a missing
/// string `t` is an error; an unknown `t` is `TextFrame::Unknown`. Known
/// frames with a `u64` offset/seq at or above 2^53 are
/// [`WireError::OffsetOutOfRange`], matching the TS side.
pub fn decode_text_frame(raw: &str) -> Result<TextFrame, WireError> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| WireError::MalformedJson)?;
    let t = value
        .get("t")
        .and_then(serde_json::Value::as_str)
        .ok_or(WireError::MalformedJson)?;
    match t {
        "hello" | "profile" | "imagedef" | "seed" | "resize" | "sizechanged" | "gfxreset"
        | "exit" | "clipboard" | "error" => {
            let frame: ServerFrame =
                serde_json::from_str(raw).map_err(|_| WireError::MalformedJson)?;
            check_server_offsets(&frame)?;
            Ok(TextFrame::Server(frame))
        }
        "resumefrom" | "takesize" | "input" | "ping" => {
            let frame: ClientFrame =
                serde_json::from_str(raw).map_err(|_| WireError::MalformedJson)?;
            match &frame {
                ClientFrame::Resumefrom { offset, mseq, .. } => {
                    check_js_safe(&[*offset, *mseq])?;
                }
                _ => {}
            }
            Ok(TextFrame::Client(frame))
        }
        "viewer-open" | "viewer-close" | "typed" | "stats" => {
            let msg: ControlMessage =
                serde_json::from_str(raw).map_err(|_| WireError::MalformedJson)?;
            match &msg {
                ControlMessage::ViewerOpen { resume, .. } => {
                    if let Some(r) = resume {
                        check_js_safe(&[r.offset, r.mseq])?;
                    }
                }
                ControlMessage::Stats {
                    joins,
                    degraded_joins,
                    viewers,
                } => {
                    let mut vals = [0u64; 3];
                    let mut n = 0;
                    if let Some(v) = joins {
                        vals[n] = *v;
                        n += 1;
                    }
                    if let Some(v) = degraded_joins {
                        vals[n] = *v;
                        n += 1;
                    }
                    if let Some(v) = viewers {
                        vals[n] = *v;
                        n += 1;
                    }
                    check_js_safe(&vals[..n])?;
                }
                _ => {}
            }
            Ok(TextFrame::Control(msg))
        }
        _ => Ok(TextFrame::Unknown(raw.to_string())),
    }
}

/// Check every `u64` offset/seq/len on a decoded server frame is JS-safe.
fn check_server_offsets(frame: &ServerFrame) -> Result<(), WireError> {
    match frame {
        ServerFrame::Imagedef { seq, offset, .. } => check_js_safe(&[*seq, *offset]),
        ServerFrame::Seed { offset, len } => check_js_safe(&[*offset, *len]),
        ServerFrame::Resize { seq, offset, .. } => check_js_safe(&[*seq, *offset]),
        ServerFrame::Sizechanged { seq, offset, .. } => check_js_safe(&[*seq, *offset]),
        ServerFrame::Gfxreset { seq, offset, .. } => check_js_safe(&[*seq, *offset]),
        ServerFrame::Exit { seq, offset, .. } => check_js_safe(&[*seq, *offset]),
        ServerFrame::Clipboard { offset, .. } => check_js_safe(&[*offset]),
        ServerFrame::Hello { .. } | ServerFrame::Profile { .. } | ServerFrame::Error { .. } => {
            check_js_safe(&[])
        }
    }
}

/// Resume coordinates in a `viewer-open` control message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeInfo {
    /// Absolute offset to resume from.
    pub offset: u64,
    /// Highest marker seq the viewer applied (0 if none).
    pub mseq: u64,
    /// Stream epoch the offset belongs to.
    pub epoch: String,
}

/// tap.sock control messages (envelope kind 2), relay <-> tap.
/// Unknown fields are rejected so a misspelled field never silently drops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ControlMessage {
    /// Open (or resume) a viewer session on the connection.
    ViewerOpen {
        /// Viewer id (the S->C bodies carry it as a prefix).
        viewer: String,
        /// Requested profile.
        profile: Profile,
        /// Resume coordinates, if any.
        resume: Option<ResumeInfo>,
    },
    /// Close a viewer session.
    ViewerClose {
        /// Viewer id.
        viewer: String,
    },
    /// Keystroke notification (no key content ever crosses here).
    Typed {
        /// Viewer id.
        viewer: String,
    },
    /// Test-mode stats request, or (with fields) the stats response.
    Stats {
        /// Completed joins, if a response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        joins: Option<u64>,
        /// Degraded joins, if a response.
        #[serde(
            default,
            rename = "degradedJoins",
            skip_serializing_if = "Option::is_none"
        )]
        degraded_joins: Option<u64>,
        /// Live viewers, if a response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        viewers: Option<u64>,
    },
}

/// Viewer profile requested at `viewer-open` time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// Desktop profile (50k-line scrollback).
    Desktop,
    /// Phone profile (20k-line scrollback).
    Phone,
}

/// tap.sock envelope kind: text JSON.
pub const ENV_TEXT: u8 = 0;
/// tap.sock envelope kind: binary frame (with its 8-byte header).
pub const ENV_BINARY: u8 = 1;
/// tap.sock envelope kind: control JSON.
pub const ENV_CONTROL: u8 = 2;
/// Largest legal tap.sock envelope length, in bytes; the u32 length counts
/// the kind byte plus the body. The 1 MiB cap was added by round-1 review
/// (brief section 6 sets no cap of its own), raised by 512 in round 3 so the
/// largest deliverable clipboard envelope still fits: the withhold OSC 52
/// budget is 1 MiB of base64 plus the clipboard JSON (~56 bytes), the S->C
/// id prefix (<= 255 bytes) and the kind byte. Both language codecs reject
/// anything over it with [`WireError::BadEnvelope`] (the TS twin applies
/// this identical constant).
pub const MAX_ENVELOPE_LEN: usize = 1_048_576 + 512;

/// One decoded tap.sock envelope: `[u32 BE length][u8 kind][body]`, where
/// `length` counts the kind byte plus the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Envelope kind (0, 1, or 2).
    pub kind: u8,
    /// Envelope body.
    pub body: Vec<u8>,
}

/// Encode one tap.sock envelope.
pub fn encode_envelope(kind: u8, body: &[u8]) -> Result<Vec<u8>, WireError> {
    if kind != ENV_TEXT && kind != ENV_BINARY && kind != ENV_CONTROL {
        return Err(WireError::BadEnvelope);
    }
    if 1 + body.len() > MAX_ENVELOPE_LEN {
        return Err(WireError::BadEnvelope);
    }
    let mut buf = Vec::with_capacity(4 + 1 + body.len());
    buf.extend_from_slice(&((1 + body.len()) as u32).to_be_bytes());
    buf.push(kind);
    buf.extend_from_slice(body);
    Ok(buf)
}

/// S->C body: `[u8 idlen][id utf8][frame]`.
pub fn s2c_body(id: &str, frame: &[u8]) -> Result<Vec<u8>, WireError> {
    let id_bytes = id.as_bytes();
    if id_bytes.len() > 255 {
        return Err(WireError::BadEnvelope);
    }
    let mut buf = Vec::with_capacity(1 + id_bytes.len() + frame.len());
    buf.push(id_bytes.len() as u8);
    buf.extend_from_slice(id_bytes);
    buf.extend_from_slice(frame);
    Ok(buf)
}

/// Parse an S->C body into the viewer id and the frame bytes.
pub fn parse_s2c_body(body: &[u8]) -> Result<(&str, &[u8]), WireError> {
    if body.is_empty() {
        return Err(WireError::BadEnvelope);
    }
    let id_len = usize::from(body[0]);
    if body.len() < 1 + id_len {
        return Err(WireError::BadEnvelope);
    }
    let id = std::str::from_utf8(&body[1..1 + id_len]).map_err(|_| WireError::BadEnvelope)?;
    Ok((id, &body[1 + id_len..]))
}

/// Streaming tap.sock envelope decoder. Feed bytes; get back every complete
/// envelope. A zero length, an oversize length (over
/// [`MAX_ENVELOPE_LEN`]), or a bad kind is `BadEnvelope`.
#[derive(Debug, Default)]
pub struct EnvelopeDecoder {
    buf: Vec<u8>,
}

impl EnvelopeDecoder {
    /// Create an empty decoder.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Push bytes; return every complete envelope now decodable.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Envelope>, WireError> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 4 {
                break;
            }
            // The length can be validated as soon as its 4 bytes are
            // buffered: an oversize (or zero) claim fails here, before the
            // body is waited for (and never buffered).
            let mut hdr = [0u8; 4];
            hdr.copy_from_slice(&self.buf[..4]);
            let len = u32::from_be_bytes(hdr) as usize;
            if len == 0 {
                return Err(WireError::BadEnvelope);
            }
            if len > MAX_ENVELOPE_LEN {
                return Err(WireError::BadEnvelope);
            }
            if self.buf.len() < 4 + len {
                break;
            }
            let kind = self.buf[4];
            if kind != ENV_TEXT && kind != ENV_BINARY && kind != ENV_CONTROL {
                return Err(WireError::BadEnvelope);
            }
            out.push(Envelope {
                kind,
                body: self.buf[5..4 + len].to_vec(),
            });
            self.buf.drain(..4 + len);
        }
        Ok(out)
    }
}

/// Runner RPC request over runner.sock (tag `op`, snake_case).
/// Unknown fields are rejected so a misspelled field never silently drops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// List sessions.
    List {},
    /// Launch a new terminal session.
    Launch {
        /// Harness to run.
        harness: Harness,
        /// Extra argv, if any.
        argv: Option<Vec<String>>,
        /// Resume session id, if any.
        resume_sid: Option<String>,
        /// Working directory.
        cwd: String,
        /// Grid columns (`None` means the runner's default).
        #[serde(default)]
        cols: Option<u16>,
        /// Grid rows (`None` means the runner's default).
        #[serde(default)]
        rows: Option<u16>,
        /// Extra environment for the harness pane.
        #[serde(default)]
        env: Option<std::collections::BTreeMap<String, String>>,
        /// Exit keys used by the kill RPC (`None` means the default sequence).
        #[serde(default)]
        exit_keys: Option<Vec<String>>,
    },
    /// Kill a session.
    Kill {
        /// Session id.
        id: String,
        /// Grace in seconds before the forced path runs (`None` means the default).
        #[serde(default)]
        grace_secs: Option<u64>,
    },
    /// Resize the session grid.
    Resize {
        /// Session id.
        id: String,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
    },
    /// Liveness probe.
    Ping {},
    /// Paste text into a session (through the tmux server, not the client).
    Paste {
        /// Session id.
        id: String,
        /// Viewer the paste is attributed to (must already exist).
        viewer: String,
        /// Text to paste.
        text: String,
    },
    /// Enter on the viewer's hidden client (caller supplies beat state).
    Enter {
        /// Session id.
        id: String,
        /// Viewer.
        viewer: String,
        /// Currently blocked (beat-derived).
        blocked: bool,
        /// The Enter comes from the composer.
        from_composer: bool,
    },
    /// Send input bytes or named keys to a viewer's hidden client.
    Keys {
        /// Session id.
        id: String,
        /// Viewer.
        viewer: String,
        /// Input payload.
        input: KeyInput,
    },
    /// Read session history.
    History {
        /// Session id.
        id: String,
    },
    /// Read a session.
    Read {
        /// Session id.
        id: String,
    },
    /// Take the grid size for a session.
    TakeSize {
        /// Session id.
        id: String,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
        /// Who is taking the grid.
        owner: String,
        /// Override a foreign owner.
        #[serde(default)]
        force: bool,
    },
    /// Release the grid size for a session.
    ReleaseSize {
        /// Session id.
        id: String,
        /// The owner releasing the grid.
        owner: String,
    },
    /// Report the grid size and owner for a session.
    Grid {
        /// Session id.
        id: String,
    },
    /// Open a hidden input client for a viewer.
    ViewerOpen {
        /// Session id.
        id: String,
        /// Viewer id.
        viewer: String,
    },
    /// Close a viewer's hidden client.
    ViewerClose {
        /// Session id.
        id: String,
        /// Viewer id.
        viewer: String,
    },
    /// Fetch media by sha256.
    Media {
        /// sha256 of the media bytes.
        sha256: String,
    },
    /// Open a byte-stream relay to a session's tap (kb-server's
    /// WebSocket upstream). This op never returns as an ordinary RPC:
    /// the server answers the request line with the standard
    /// `{"status":"ok","data":{...}}` envelope, then splices tap.sock
    /// envelopes verbatim on the SAME connection until either side
    /// closes. `from` is the absolute byte offset the caller wants to
    /// resume from (`None` = a full join); the tap decides resume vs
    /// full join.
    Stream {
        /// Session id.
        id: String,
        /// Resume offset, if any.
        #[serde(default)]
        from: Option<u64>,
    },
}

/// Key input for the `keys` RPC (kind-tagged; unknown fields rejected).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum KeyInput {
    /// Raw bytes, base64-encoded (written verbatim).
    Bytes {
        /// Base64 of the bytes (4 KiB decoded cap per call).
        bytes_b64: String,
    },
    /// Named keys from the closed table.
    Named {
        /// Key names.
        named: Vec<String>,
    },
}

/// Runner RPC response over runner.sock (tag `status`, lowercase).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Response {
    /// Success with a payload. `List` puts `SessionInfo` entries in `data`
    /// as an array.
    Ok {
        /// Response payload.
        data: serde_json::Value,
    },
    /// Failure.
    Err {
        /// Machine-readable code.
        code: String,
        /// Human-readable message.
        message: String,
    },
}

/// Success payload for `viewer_open` / `viewer_close` (`{"viewer": ...}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerData {
    /// The viewer id.
    pub viewer: String,
}

/// Success payload for `paste` (`{"paste_only": ..., "bracketed": ...}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteData {
    /// Whether the text starts with `/` or `@`.
    pub paste_only: bool,
    /// Whether bracketed paste was on in the pane.
    pub bracketed: bool,
}

/// Grid result for `take_size` / `grid` (`{"cols", "rows", "owner"}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridData {
    /// Grid columns.
    pub cols: u16,
    /// Grid rows.
    pub rows: u16,
    /// Grid owner, if any.
    pub owner: Option<String>,
}

/// Empty success payload (`{}` for keys / enter / release / resize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyData {}

/// Session facts returned by `Request::List` inside `Response::Ok` `data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// Harness running in the session.
    pub harness: Harness,
    /// Working directory.
    pub cwd: String,
    /// Start time as unix seconds.
    pub started_unix: i64,
    /// Resume session id, if any.
    pub resume_sid: Option<String>,
    /// Pane pid.
    pub pane_pid: u32,
    /// Grid columns.
    pub cols: u16,
    /// Grid rows.
    pub rows: u16,
    /// Whether bracketed paste is enabled in the pane.
    pub bracket_paste_flag: bool,
    /// Whether the pane is dead.
    pub pane_dead: bool,
    /// Exit code, if any.
    pub exit_code: Option<i32>,
    /// Grid owner, if any.
    pub grid_owner: Option<String>,
    /// `kb-term.conf` version the holder loaded.
    #[serde(default)]
    pub conf_version: u32,
    /// The anchor client pid, if any.
    #[serde(default)]
    pub anchor_pid: Option<u32>,
    /// Whether the holder socket did not answer (stale).
    #[serde(default)]
    pub stale: bool,
}
