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
    },
    /// A stored image definition.
    Imagedef {
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
        /// Absolute offset of this event.
        offset: u64,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
    },
    /// Grid size change with the giver and offset.
    Sizechanged {
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
        /// Image generation.
        gen: u32,
    },
    /// Session exit.
    Exit {
        /// Exit code, if any.
        code: Option<i32>,
        /// Absolute offset of this event.
        offset: u64,
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

/// Error decoding a binary frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The frame is shorter than the 8-byte offset header.
    #[error("short binary frame")]
    ShortBinaryFrame,
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
/// An empty payload is valid; a frame under 8 bytes is an error.
pub fn decode_binary(frame: &[u8]) -> Result<(u64, &[u8]), WireError> {
    if frame.len() < 8 {
        return Err(WireError::ShortBinaryFrame);
    }
    let mut header = [0u8; 8];
    header.copy_from_slice(&frame[..8]);
    Ok((u64::from_be_bytes(header), &frame[8..]))
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
