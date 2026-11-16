//! Optional local pictures for `kb-term attach`: kitty Unicode
//! placeholders drawn by a Ghostty (or kitty) pane over plain ssh.
//!
//! Everything here is a pure, synchronous, byte-in / byte-out type. No
//! module in this file touches the terminal, a tty, a socket or the
//! filesystem: the attach loop passes bytes in and writes the result.
//!
//! STATUS: this is the CI-verifiable core only. The attach loop does not
//! call it yet (no `--graphics` flag exists), and nothing here has ever
//! run against a real Ghostty or kitty. Whether Ghostty redraws
//! placeholder cells that were shown before a later transmit of the same
//! id is UNVERIFIED.
//!
//! Invariants the tests pin:
//! - every APC this module emits carries `q=2` (the terminal never
//!   answers) and every placement is virtual (`U=1`);
//! - it never forwards, creates or reorders a kitty delete (`a=d`);
//! - [`GraphicsFilter::process`] only INSERTS bytes: removing the
//!   inserted APCs gives the input back byte for byte, whatever the
//!   chunking.

use std::collections::{HashMap, HashSet};

use base64::Engine as _;

use crate::wire::{is_valid_sha256_hex, ServerFrame};

/// How picture rows are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureMode {
    /// Dim one-row labels (the default).
    Labels,
    /// Real pictures through kitty graphics.
    Kitty,
}

const MUX_CAPTION: &str =
    "inside a multiplexer; pictures need a pane directly in Ghostty over plain ssh";
const PROBE_CAPTION: &str = "terminal did not confirm kitty graphics (mosh drops them)";
/// Environment variables whose presence means a multiplexer. A fixed
/// list: the `env` lookup cannot enumerate prefixes.
const MUX_ENV: [&str; 4] = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "TMUX"];

fn in_multiplexer(env: &dyn Fn(&str) -> Option<String>) -> bool {
    MUX_ENV.iter().any(|k| env(k).is_some())
}

/// Whether the startup probe may be sent at all (never when graphics
/// were not requested, never inside a multiplexer).
#[must_use]
pub fn should_probe(graphics_flag: bool, env: &dyn Fn(&str) -> Option<String>) -> bool {
    graphics_flag && !in_multiplexer(env)
}

/// Inputs to [`decide_mode`].
pub struct ModeRequest<'a> {
    /// `--graphics` was given.
    pub graphics_flag: bool,
    /// Environment lookup.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The cached startup probe result (`None` = never probed).
    pub probe: Option<&'a ProbeResult>,
    /// Columns of the local pane.
    pub local_cols: u16,
    /// Columns of the session grid.
    pub grid_cols: u16,
}

/// Decide the picture mode, with a caption saying why it is
/// [`PictureMode::Labels`] when graphics were requested but refused.
#[must_use]
pub fn decide_mode(req: &ModeRequest<'_>) -> (PictureMode, Option<String>) {
    if !req.graphics_flag {
        return (PictureMode::Labels, None);
    }
    if in_multiplexer(req.env) {
        return (PictureMode::Labels, Some(MUX_CAPTION.to_string()));
    }
    if !req.probe.is_some_and(ProbeResult::is_positive) {
        return (PictureMode::Labels, Some(PROBE_CAPTION.to_string()));
    }
    if req.local_cols < req.grid_cols {
        return (
            PictureMode::Labels,
            Some(format!(
                "pane ({} cols) is narrower than the session grid ({} cols); placeholder rows would wrap",
                req.local_cols, req.grid_cols
            )),
        );
    }
    (PictureMode::Kitty, None)
}

// ------------------------------------------------------------------ probe

/// The startup probe: XTVERSION, a kitty graphics query, then DA1 as the
/// sentinel every terminal answers.
#[must_use]
pub fn probe_bytes() -> Vec<u8> {
    b"\x1b[>0q\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c".to_vec()
}

/// What the terminal answered to [`probe_bytes`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeResult {
    /// The XTVERSION reply text, if any.
    pub xtversion: Option<String>,
    /// The kitty query was answered `OK`.
    pub kitty_ok: bool,
    /// The DA1 sentinel arrived (the probe is complete).
    pub saw_da1: bool,
    /// Bytes that were not part of a recognised reply, in order,
    /// including everything after the DA1 reply: user input.
    pub leftover: Vec<u8>,
}

impl ProbeResult {
    /// Kitty graphics confirmed AND the terminal names itself ghostty or
    /// kitty. Anything else is negative.
    #[must_use]
    pub fn is_positive(&self) -> bool {
        let named = self.xtversion.as_deref().is_some_and(|v| {
            let v = v.trim().to_ascii_lowercase();
            v.starts_with("ghostty") || v.starts_with("kitty")
        });
        self.kitty_ok && named
    }
}

fn find_st(bytes: &[u8], from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(2)
        .position(|w| w == b"\x1b\\")
        .map(|p| p + from)
}

/// Parse the probe replies. Pure; call it on the bytes read so far.
#[must_use]
pub fn parse_probe_replies(bytes: &[u8]) -> ProbeResult {
    let mut res = ProbeResult::default();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b || i + 1 >= bytes.len() {
            res.leftover.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            b'P' | b'_' => {
                let Some(st) = find_st(bytes, i + 2) else {
                    res.leftover.extend_from_slice(&bytes[i..]);
                    break;
                };
                let body = &bytes[i + 2..st];
                if bytes[i + 1] == b'P' && body.starts_with(b">|") {
                    if res.xtversion.is_none() {
                        res.xtversion =
                            Some(String::from_utf8_lossy(&body[2..]).trim().to_string());
                    }
                } else if bytes[i + 1] == b'_' && body.starts_with(b"G") {
                    if body.windows(7).any(|w| w == b"i=31;OK") {
                        res.kitty_ok = true;
                    }
                } else {
                    res.leftover.extend_from_slice(&bytes[i..st + 2]);
                }
                i = st + 2;
            }
            b'[' => {
                let mut j = i + 2;
                while j < bytes.len() && (0x20..=0x3f).contains(&bytes[j]) {
                    j += 1;
                }
                if j >= bytes.len() || !(0x40..=0x7e).contains(&bytes[j]) {
                    res.leftover.extend_from_slice(&bytes[i..]);
                    break;
                }
                if bytes[j] == b'c' && bytes.get(i + 2) == Some(&b'?') {
                    res.saw_da1 = true;
                    res.leftover.extend_from_slice(&bytes[j + 1..]);
                    break;
                }
                res.leftover.extend_from_slice(&bytes[i..=j]);
                i = j + 1;
            }
            _ => {
                res.leftover.push(bytes[i]);
                i += 1;
            }
        }
    }
    res
}

// ------------------------------------------------------------ APC encoder

const CHUNK_B64: usize = 4096;

/// Transmit a PNG under image id `id` (`a=t,f=100,q=2`), base64 in
/// chunks of at most 4096 characters.
#[must_use]
pub fn transmit_apc(id: u32, png: &[u8]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let mut out = Vec::with_capacity(b64.len() + 64);
    let chunks: Vec<&[u8]> = if b64.is_empty() {
        vec![&b""[..]]
    } else {
        b64.as_bytes().chunks(CHUNK_B64).collect()
    };
    let last = chunks.len() - 1;
    for (n, chunk) in chunks.iter().enumerate() {
        let more = u8::from(n != last);
        out.extend_from_slice(b"\x1b_G");
        if n == 0 {
            out.extend_from_slice(format!("a=t,f=100,q=2,i={id},m={more}").as_bytes());
        } else {
            out.extend_from_slice(format!("q=2,m={more}").as_bytes());
        }
        out.push(b';');
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// A virtual placement (`U=1`, `p` equal to `i`, as omp emits it).
#[must_use]
pub fn place_apc(id: u32, cols: u16, rows: u16) -> Vec<u8> {
    format!("\x1b_Ga=p,U=1,q=2,i={id},p={id},c={cols},r={rows}\x1b\\").into_bytes()
}

// ------------------------------------------------------------ the filter

/// A picture definition as announced by the tap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDef {
    /// Image id.
    pub id: u32,
    /// Image generation.
    pub gen: u32,
    /// sha256 hex key of the PNG bytes.
    pub sha256: String,
    /// Width in cells.
    pub cols: u16,
    /// Height in cells.
    pub rows: u16,
}

impl ImageDef {
    /// The definition carried by an `Imagedef` frame (`None` for any
    /// other frame). The frame's `mime` is not checked here: a non-PNG
    /// payload is refused when its bytes are fetched.
    #[must_use]
    pub fn from_frame(frame: &ServerFrame) -> Option<Self> {
        match frame {
            ServerFrame::Imagedef {
                id,
                gen,
                sha256,
                cols,
                rows,
                ..
            } if is_valid_sha256_hex(sha256) => Some(Self {
                id: *id,
                gen: *gen,
                sha256: sha256.clone(),
                cols: *cols,
                rows: *rows,
            }),
            _ => None,
        }
    }
}

/// Where PNG bytes come from. The attach loop implements it; this
/// module never does IO.
pub trait ImageSource {
    /// The PNG for a sha256 key, if it is still available. Keys are
    /// pre-validated hex64 (see `is_valid_sha256_hex`): the filter never
    /// passes any other string.
    fn png(&mut self, sha256: &str) -> Option<Vec<u8>>;
}

const PLACEHOLDER: [u8; 4] = [0xF4, 0x8E, 0xBB, 0xAE];
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const MAX_CSI_PARAMS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lex {
    Ground,
    Esc,
    Csi,
    Str,
    StrEsc,
}

/// Inserts kitty transmit + placement APCs in front of the first
/// placeholder glyph of every picture id in the viewer stream.
pub struct GraphicsFilter {
    kitty: bool,
    defs: HashMap<u32, ImageDef>,
    sent: HashSet<u32>,
    /// Ids that could not be drawn (no def, zero geometry, missing or
    /// non-PNG bytes); cleared by a usable `on_image_def`.
    failed: HashSet<u32>,
    skipped: u64,
    lex: Lex,
    params: Vec<u8>,
    params_overflow: bool,
    csi_intermediate: bool,
    /// Matched prefix of a placeholder glyph, not yet emitted.
    held: Vec<u8>,
    fg: Option<u32>,
}

impl GraphicsFilter {
    /// A filter; `mode_kitty == false` makes [`Self::process`] the
    /// identity.
    #[must_use]
    pub fn new(mode_kitty: bool) -> Self {
        Self {
            kitty: mode_kitty,
            defs: HashMap::new(),
            sent: HashSet::new(),
            failed: HashSet::new(),
            skipped: 0,
            lex: Lex::Ground,
            params: Vec::new(),
            params_overflow: false,
            csi_intermediate: false,
            held: Vec::new(),
            fg: None,
        }
    }

    /// Switch kitty mode on or off at runtime (a width change). Returns
    /// the bytes of a placeholder glyph prefix that was held back: the
    /// caller MUST write them before any later output, so no input byte
    /// is ever dropped. While kitty mode is off the lexer does not see
    /// the stream, so on a change the byte-level parser state and the
    /// tracked foreground are reset and the sent set is cleared (an
    /// erase may have happened unobserved and freed the terminal's image
    /// data, so every id is transmitted again on its next row).
    #[must_use]
    pub fn set_kitty(&mut self, on: bool) -> Vec<u8> {
        if self.kitty == on {
            return Vec::new();
        }
        self.kitty = on;
        self.lex = Lex::Ground;
        self.params.clear();
        self.params_overflow = false;
        self.csi_intermediate = false;
        self.fg = None;
        self.sent.clear();
        std::mem::take(&mut self.held)
    }

    /// Whether kitty mode is on.
    #[must_use]
    pub fn is_kitty(&self) -> bool {
        self.kitty
    }

    /// Record an announced picture. An identical re-announcement is a
    /// no-op; a different sha256, a higher generation or new geometry
    /// marks the id unsent again.
    pub fn on_image_def(&mut self, def: &ImageDef) {
        if !is_valid_sha256_hex(&def.sha256) {
            self.failed.insert(def.id);
            self.sent.remove(&def.id);
            self.defs.remove(&def.id);
            return;
        }
        let unchanged = self.defs.get(&def.id).is_some_and(|old| {
            old.sha256 == def.sha256
                && old.gen >= def.gen
                && old.cols == def.cols
                && old.rows == def.rows
        });
        if unchanged {
            return;
        }
        self.sent.remove(&def.id);
        self.failed.remove(&def.id);
        self.defs.insert(def.id, def.clone());
    }

    /// The tap reported a graphics reset (a kitty `a=d` never reaches
    /// the client, so this is the only delete signal): every id is
    /// unsent again; the definitions stay.
    pub fn on_gfx_reset(&mut self) {
        self.sent.clear();
    }

    /// True when rows of this id must keep the attach loop's label
    /// rewriter (the picture could not be drawn).
    #[must_use]
    pub fn wants_label(&self, id: u32) -> bool {
        self.failed.contains(&id)
    }

    /// How many ids were refused so far (counted once per id and def).
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Transform one chunk of the viewer stream. A partial placeholder
    /// glyph at the end of the chunk is held until the next call (or
    /// [`Self::flush`]).
    pub fn process(&mut self, chunk: &[u8], src: &mut dyn ImageSource) -> Vec<u8> {
        if !self.kitty {
            return chunk.to_vec();
        }
        let mut out = Vec::with_capacity(chunk.len());
        for &b in chunk {
            self.step(b, src, &mut out);
        }
        out
    }

    /// Release held bytes (end of stream).
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }

    fn step(&mut self, b: u8, src: &mut dyn ImageSource, out: &mut Vec<u8>) {
        match self.lex {
            Lex::Ground => self.step_ground(b, src, out),
            Lex::Esc => {
                out.push(b);
                self.lex = match b {
                    b'[' => {
                        self.params.clear();
                        self.params_overflow = false;
                        self.csi_intermediate = false;
                        Lex::Csi
                    }
                    b']' | b'P' | b'_' | b'^' | b'X' => Lex::Str,
                    b'c' => {
                        self.sent.clear();
                        Lex::Ground
                    }
                    0x1b => Lex::Esc,
                    _ => Lex::Ground,
                };
            }
            Lex::Csi => {
                out.push(b);
                match b {
                    0x30..=0x3f => {
                        if self.params.len() < MAX_CSI_PARAMS {
                            self.params.push(b);
                        } else {
                            self.params_overflow = true;
                        }
                    }
                    0x20..=0x2f => self.csi_intermediate = true,
                    0x40..=0x7e => {
                        self.csi_final(b);
                        self.lex = Lex::Ground;
                    }
                    0x1b => self.lex = Lex::Esc,
                    _ => {}
                }
            }
            Lex::Str => {
                out.push(b);
                match b {
                    0x07 => self.lex = Lex::Ground,
                    0x1b => self.lex = Lex::StrEsc,
                    _ => {}
                }
            }
            Lex::StrEsc => {
                out.push(b);
                self.lex = match b {
                    b'\\' => Lex::Ground,
                    0x1b => Lex::StrEsc,
                    _ => Lex::Str,
                };
            }
        }
    }

    fn step_ground(&mut self, b: u8, src: &mut dyn ImageSource, out: &mut Vec<u8>) {
        if !self.held.is_empty() {
            if b == PLACEHOLDER[self.held.len()] {
                self.held.push(b);
                if self.held.len() == PLACEHOLDER.len() {
                    self.on_placeholder(src, out);
                    out.append(&mut self.held);
                }
                return;
            }
            out.append(&mut self.held);
        }
        if b == PLACEHOLDER[0] {
            self.held.push(b);
        } else {
            out.push(b);
            if b == 0x1b {
                self.lex = Lex::Esc;
            }
        }
    }

    fn csi_final(&mut self, fin: u8) {
        if self.params_overflow || self.csi_intermediate {
            return;
        }
        match fin {
            b'm' => {
                let params = std::mem::take(&mut self.params);
                apply_sgr(&mut self.fg, &params);
                self.params = params;
            }
            b'J' if self.params == b"2" || self.params == b"3" => self.sent.clear(),
            _ => {}
        }
    }

    /// A placeholder glyph is about to be written: transmit and place
    /// the foreground's image first when it has not been sent.
    fn on_placeholder(&mut self, src: &mut dyn ImageSource, out: &mut Vec<u8>) {
        let Some(id) = self.fg.filter(|&id| id != 0) else {
            return;
        };
        if self.sent.contains(&id) || self.failed.contains(&id) {
            return;
        }
        let usable = self
            .defs
            .get(&id)
            .filter(|d| d.cols != 0 && d.rows != 0)
            .cloned();
        let png = usable
            .as_ref()
            .and_then(|d| src.png(&d.sha256))
            .filter(|p| p.starts_with(&PNG_MAGIC));
        match (usable, png) {
            (Some(def), Some(png)) => {
                out.extend_from_slice(&transmit_apc(id, &png));
                out.extend_from_slice(&place_apc(id, def.cols, def.rows));
                self.sent.insert(id);
            }
            _ => {
                self.failed.insert(id);
                self.skipped += 1;
            }
        }
    }
}

/// Apply one SGR parameter string (`38;2;R;G;B`, `38:2::R:G:B`,
/// `38;5;n`, `0`, empty, `39`) to the tracked foreground. The id is the
/// 24-bit colour (`R<<16|G<<8|B`) or the palette index.
fn apply_sgr(fg: &mut Option<u32>, params: &[u8]) {
    if params
        .iter()
        .any(|b| matches!(b, b'<' | b'=' | b'>' | b'?'))
    {
        return;
    }
    let text = String::from_utf8_lossy(params);
    let groups: Vec<Vec<Option<u32>>> = text
        .split(';')
        .map(|g| {
            g.split(':')
                .map(|s| s.parse::<u32>().ok().map(|v| v.min(100_000)))
                .collect()
        })
        .collect();
    let first = |i: usize| groups.get(i).and_then(|g| g.first().copied().flatten());
    let rgb = |r: Option<u32>, g: Option<u32>, b: Option<u32>| match (r, g, b) {
        (Some(r), Some(g), Some(b)) if r <= 255 && g <= 255 && b <= 255 => {
            Some((r << 16) | (g << 8) | b)
        }
        _ => None,
    };
    let mut i = 0;
    while i < groups.len() {
        let g = &groups[i];
        let code = g[0].unwrap_or(0);
        match code {
            0 | 39 | 30..=37 | 90..=97 => {
                *fg = None;
                i += 1;
            }
            38 | 48 | 58 => {
                let is_fg = code == 38;
                if g.len() > 1 {
                    // Colon form: [cs] is optional; with four values the
                    // first is the (usually empty) colour-space slot.
                    let v = match g[1] {
                        Some(2) => {
                            let vals = &g[2..];
                            match vals.len() {
                                n if n >= 4 => rgb(vals[n - 3], vals[n - 2], vals[n - 1]),
                                3 => rgb(vals[0], vals[1], vals[2]),
                                _ => None,
                            }
                        }
                        Some(5) => g.get(2).copied().flatten().filter(|&n| n <= 255),
                        _ => None,
                    };
                    if is_fg {
                        *fg = v;
                    }
                    i += 1;
                } else {
                    match first(i + 1) {
                        Some(2) => {
                            if is_fg {
                                *fg = rgb(first(i + 2), first(i + 3), first(i + 4));
                            }
                            i += 5;
                        }
                        Some(5) => {
                            if is_fg {
                                *fg = first(i + 2).filter(|&n| n <= 255);
                            }
                            i += 3;
                        }
                        _ => i += 1,
                    }
                }
            }
            _ => i += 1,
        }
    }
}

// ------------------------------------------------------------ stdin filter

const MAX_HELD: usize = 256;

enum Classified {
    /// A terminal reply of this many bytes.
    Reply(usize),
    /// Not a reply: the ESC is ordinary input.
    Input,
    /// Could still become a reply with more bytes.
    Incomplete,
}

/// Removes terminal-originated replies (kitty APC, XTVERSION, DA1/DA2,
/// OSC 10/11 colours) from stdin so they never reach the hidden tmux
/// client. Keys, including CSI `R` (`ESC[1;2R` is the F3 key), pass.
#[derive(Debug, Default)]
pub struct StdinFilter {
    held: Vec<u8>,
}

fn classify(buf: &[u8]) -> Classified {
    use Classified::{Incomplete, Input, Reply};
    // buf[0] is ESC.
    let Some(&kind) = buf.get(1) else {
        return Incomplete;
    };
    let st = |from: usize, bel: bool| -> Classified {
        let mut j = from;
        while j < buf.len() {
            if bel && buf[j] == 0x07 {
                return Reply(j + 1);
            }
            if buf[j] == 0x1b {
                return match buf.get(j + 1) {
                    Some(b'\\') => Reply(j + 2),
                    Some(_) => Input,
                    None => Incomplete,
                };
            }
            j += 1;
        }
        Incomplete
    };
    match kind {
        b'_' => match buf.get(2) {
            None => Incomplete,
            Some(b'G') => st(3, false),
            Some(_) => Input,
        },
        b'P' => match (buf.get(2), buf.get(3)) {
            (None, _) | (Some(b'>'), None) => Incomplete,
            (Some(b'>'), Some(b'|')) => st(4, false),
            _ => Input,
        },
        b'[' => match buf.get(2) {
            None => Incomplete,
            Some(b'?' | b'>') => {
                let mut j = 3;
                while j < buf.len() && (0x20..=0x3f).contains(&buf[j]) {
                    j += 1;
                }
                match buf.get(j) {
                    None => Incomplete,
                    Some(b'c') => Reply(j + 1),
                    Some(_) => Input,
                }
            }
            Some(_) => Input,
        },
        b']' => {
            let prefix_ok = |p: &[u8]| {
                let n = buf.len().min(p.len() + 2);
                buf[2..n] == p[..n - 2]
            };
            if buf.len() < 5 {
                if prefix_ok(b"10;") || prefix_ok(b"11;") {
                    Incomplete
                } else {
                    Input
                }
            } else if &buf[2..5] == b"10;" || &buf[2..5] == b"11;" {
                st(5, true)
            } else {
                Input
            }
        }
        _ => Input,
    }
}

impl StdinFilter {
    /// A new filter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed stdin bytes; returns the bytes to forward. A sequence prefix
    /// split across reads is held until it resolves, or until
    /// [`Self::flush`].
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut buf = std::mem::take(&mut self.held);
        buf.extend_from_slice(bytes);
        let mut out = Vec::with_capacity(buf.len());
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                out.push(buf[i]);
                i += 1;
                continue;
            }
            match classify(&buf[i..]) {
                Classified::Reply(n) => i += n,
                Classified::Input => {
                    out.push(0x1b);
                    i += 1;
                }
                Classified::Incomplete if buf.len() - i > MAX_HELD => {
                    out.push(0x1b);
                    i += 1;
                }
                Classified::Incomplete => {
                    self.held = buf[i..].to_vec();
                    break;
                }
            }
        }
        out
    }

    /// Release held bytes (the caller does this after 25 ms of silence,
    /// so a lone Escape keypress is never swallowed).
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }

    /// Whether bytes are being held.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.held.is_empty()
    }
}

/// The local cell size in pixels from a window size, when the terminal
/// reports pixels (`TIOCGWINSZ`). `None` when any input is zero.
#[must_use]
pub fn cell_px_from_winsize(cols: u16, rows: u16, xpixel: u16, ypixel: u16) -> Option<(u16, u16)> {
    if cols == 0 || rows == 0 || xpixel == 0 || ypixel == 0 {
        return None;
    }
    Some((xpixel / cols, ypixel / rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLYPH: [u8; 4] = [0xF4, 0x8E, 0xBB, 0xAE];

    fn png(tag: u8) -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend_from_slice(&[tag; 24]);
        v
    }

    struct Store(HashMap<String, Vec<u8>>);
    impl ImageSource for Store {
        fn png(&mut self, sha256: &str) -> Option<Vec<u8>> {
            self.0.get(sha256).cloned()
        }
    }

    fn sha(n: u32) -> String {
        format!("{n:064x}")
    }

    fn def(id: u32, gen: u32, cols: u16, rows: u16) -> ImageDef {
        ImageDef {
            id,
            gen,
            sha256: sha(id),
            cols,
            rows,
        }
    }

    fn store(ids: &[u32]) -> Store {
        Store(ids.iter().map(|&i| (sha(i), png(i as u8))).collect())
    }

    /// An omp-shaped picture row for `id` (3 cells).
    fn row(id: u32) -> Vec<u8> {
        let (r, g, b) = ((id >> 16) & 255, (id >> 8) & 255, id & 255);
        let mut v = format!("\x1b[38;2;{r};{g};{b}m\x1b[58:2::{r}:{g}:{b}m").into_bytes();
        for _ in 0..3 {
            v.extend_from_slice(&GLYPH);
            v.extend_from_slice(&[0xCC, 0x85, 0xCC, 0x8D]);
        }
        v.extend_from_slice(b"\x1b[39;59m\r\r\n\x1b[2K");
        v
    }

    fn filter_with(ids: &[u32]) -> GraphicsFilter {
        let mut f = GraphicsFilter::new(true);
        for &i in ids {
            f.on_image_def(&def(i, 1, 90, 19));
        }
        f
    }

    fn run(f: &mut GraphicsFilter, s: &mut Store, input: &[u8]) -> Vec<u8> {
        let mut out = f.process(input, s);
        out.extend(f.flush());
        out
    }

    /// Every APC body (between `ESC _ G` and `ESC \`).
    fn apcs(bytes: &[u8]) -> Vec<String> {
        let mut v = Vec::new();
        let mut i = 0;
        while i + 2 < bytes.len() {
            if &bytes[i..i + 3] == b"\x1b_G" {
                let end = find_st(bytes, i + 3).expect("terminated APC");
                v.push(String::from_utf8_lossy(&bytes[i + 3..end]).into_owned());
                i = end + 2;
            } else {
                i += 1;
            }
        }
        v
    }

    fn transmits(bytes: &[u8]) -> usize {
        apcs(bytes).iter().filter(|a| a.starts_with("a=t,")).count()
    }

    fn strip(bytes: &[u8], id: u32, cols: u16, rows: u16, p: &[u8]) -> Vec<u8> {
        let mut pair = transmit_apc(id, p);
        pair.extend(place_apc(id, cols, rows));
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(&pair) {
                i += pair.len();
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn base64_encoder_matches_rfc4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, enc) in cases {
            let apc = transmit_apc(1, plain.as_bytes());
            let body = String::from_utf8(apc).unwrap();
            let payload = body
                .strip_prefix("\x1b_Ga=t,f=100,q=2,i=1,m=0;")
                .and_then(|s| s.strip_suffix("\x1b\\"))
                .expect("one chunk");
            assert_eq!(payload, enc, "vector {plain:?}");
        }
    }

    #[test]
    fn transmit_chunks_a_64134_byte_png_into_21_apc_chunks() {
        let data: Vec<u8> = (0..64_134u32).map(|i| (i * 31 % 251) as u8).collect();
        let one_shot = base64::engine::general_purpose::STANDARD.encode(&data);
        assert_eq!(one_shot.len(), 85_512);
        let apc = transmit_apc(7, &data);
        let chunks = apcs(&apc);
        assert_eq!(chunks.len(), 21);
        let mut joined = String::new();
        for (n, c) in chunks.iter().enumerate() {
            let (ctrl, payload) = c.split_once(';').unwrap();
            assert!(ctrl.contains("q=2"));
            match n {
                0 => assert_eq!(ctrl, "a=t,f=100,q=2,i=7,m=1"),
                1..=19 => assert_eq!(ctrl, "q=2,m=1"),
                _ => {
                    assert_eq!(ctrl, "q=2,m=0");
                    assert_eq!(payload.len(), 3592);
                }
            }
            if n < 20 {
                assert_eq!(payload.len(), 4096);
            }
            joined.push_str(payload);
        }
        assert_eq!(joined, one_shot);
    }

    #[test]
    fn a_single_chunk_image_ends_with_m0_and_a_placement_is_virtual_and_q2() {
        let apc = transmit_apc(5, &[9u8; 100]);
        let chunks = apcs(&apc);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].starts_with("a=t,f=100,q=2,i=5,m=0;"));
        assert_eq!(
            place_apc(0x66CF38, 90, 19),
            b"\x1b_Ga=p,U=1,q=2,i=6737720,p=6737720,c=90,r=19\x1b\\".to_vec()
        );
    }

    #[test]
    fn every_emitted_apc_carries_q2_and_none_is_a_delete() {
        let mut f = filter_with(&[1, 2, 3]);
        let mut s = store(&[1, 2, 3]);
        let mut stream = Vec::new();
        for id in [1, 2, 3] {
            stream.extend(row(id));
        }
        stream.extend_from_slice(b"\x1b[H\x1b[2J\x1b[3J");
        stream.extend(row(1));
        f.on_gfx_reset();
        stream.extend(row(2));
        stream.extend_from_slice(b"\x1bc");
        stream.extend(row(3));
        let out = run(&mut f, &mut s, &stream);
        let all = apcs(&out);
        assert!(all.len() >= 12);
        for a in &all {
            let ctrl = a.split_once(';').map_or(a.as_str(), |(c, _)| c);
            assert!(ctrl.contains("q=2"), "{ctrl}");
            assert!(!ctrl.contains("a=d"), "{ctrl}");
            assert!(!ctrl.contains("a=T"), "{ctrl}");
        }
        for a in all.iter().filter(|a| a.starts_with("a=p")) {
            assert!(a.contains("U=1"));
        }
    }

    #[test]
    fn first_placeholder_row_per_id_transmits_before_the_glyph_and_only_once() {
        let mut f = filter_with(&[10, 20]);
        let mut s = store(&[10, 20]);
        let mut input = Vec::new();
        for _ in 0..3 {
            input.extend(row(10));
        }
        for _ in 0..2 {
            input.extend(row(20));
        }
        let out = run(&mut f, &mut s, &input);
        assert_eq!(transmits(&out), 2);
        assert_eq!(
            apcs(&out).iter().filter(|a| a.starts_with("a=p")).count(),
            2
        );
        for id in [10u32, 20] {
            let mut pair = transmit_apc(id, &png(id as u8));
            pair.extend(place_apc(id, 90, 19));
            let mut with_glyph = pair.clone();
            with_glyph.extend_from_slice(&GLYPH);
            let at = out
                .windows(with_glyph.len())
                .position(|w| w == with_glyph.as_slice())
                .expect("APC sits immediately before a glyph");
            // Nothing but the pair precedes the glyph it was inserted for.
            assert_eq!(&out[at + pair.len()..at + pair.len() + 4], &GLYPH[..]);
        }
        let mut back = strip(&out, 10, 90, 19, &png(10));
        back = strip(&back, 20, 90, 19, &png(20));
        assert_eq!(back, input);
    }

    #[test]
    fn ed2_ed3_and_ris_make_ids_unsent_again() {
        let cases: [(&[u8], bool); 7] = [
            (b"\x1b[H\x1b[2J\x1b[3J", true),
            (b"\x1b[2J", true),
            (b"\x1b[3J", true),
            (b"\x1bc", true),
            (b"\x1b[1J", false),
            (b"\x1b[0J", false),
            (b"\x1b[J", false),
        ];
        for (erase, resets) in cases {
            let mut f = filter_with(&[4]);
            let mut s = store(&[4]);
            let mut input = row(4);
            input.extend_from_slice(erase);
            input.extend(row(4));
            let out = run(&mut f, &mut s, &input);
            assert_eq!(transmits(&out), if resets { 2 } else { 1 }, "{erase:?}");
        }
    }

    #[test]
    fn gfx_reset_frame_clears_the_sent_set_but_not_the_defs() {
        let mut f = filter_with(&[4]);
        let mut s = store(&[4]);
        let first = run(&mut f, &mut s, &row(4));
        assert_eq!(transmits(&first), 1);
        f.on_gfx_reset();
        let second = run(&mut f, &mut s, &row(4));
        assert_eq!(transmits(&second), 1);
        assert!(!f.wants_label(4));
    }

    #[test]
    fn a_changed_sha_for_the_same_id_is_retransmitted_without_an_erase() {
        let mut f = filter_with(&[4]);
        let mut s = store(&[4]);
        assert_eq!(transmits(&run(&mut f, &mut s, &row(4))), 1);
        f.on_image_def(&def(4, 1, 90, 19));
        assert_eq!(transmits(&run(&mut f, &mut s, &row(4))), 0, "identical");
        let mut changed = def(4, 1, 90, 19);
        changed.sha256 = sha(999);
        s.0.insert(sha(999), png(99));
        f.on_image_def(&changed);
        let out = run(&mut f, &mut s, &row(4));
        assert_eq!(transmits(&out), 1);
        assert!(out
            .windows(transmit_apc(4, &png(99)).len())
            .any(|w| w == transmit_apc(4, &png(99)).as_slice()));
        assert_eq!(transmits(&run(&mut f, &mut s, &row(4))), 0, "sent once");
        let mut again = changed.clone();
        again.gen = 2;
        f.on_image_def(&again);
        assert_eq!(transmits(&run(&mut f, &mut s, &row(4))), 1, "higher gen");
    }

    #[test]
    fn seed_starting_with_ed3_gets_its_images() {
        let mut f = filter_with(&[6]);
        let mut s = store(&[6]);
        assert_eq!(transmits(&run(&mut f, &mut s, &row(6))), 1);
        let mut seed = b"\x1b[3J".to_vec();
        seed.extend(row(6));
        let out = run(&mut f, &mut s, &seed);
        assert_eq!(transmits(&out), 1);
        let at = out.windows(3).position(|w| w == b"\x1b_G").unwrap();
        let ed3 = out.windows(4).position(|w| w == b"\x1b[3J").unwrap();
        assert!(ed3 < at);
    }

    fn fixture() -> Vec<u8> {
        let mut v = b"hello \xe2\x82\xac ".to_vec();
        v.extend_from_slice(b"\x1b[38;2;0;0;7m");
        v.extend_from_slice(&GLYPH);
        v.extend_from_slice(b"\x1b[0m\x1b[3J");
        v.extend_from_slice(b"\x1b[38:2::0:0:9m");
        v.extend_from_slice(&GLYPH);
        v.extend_from_slice(&GLYPH);
        v.extend_from_slice(b"\x1b[38;5;11m");
        v.extend_from_slice(&GLYPH);
        v.extend_from_slice(b"\x1b[2J\x1b]0;title\x07\x1b[38;2;0;0;7m");
        v.extend_from_slice(&GLYPH);
        v.extend_from_slice(&[0xF4, 0x8E, b'x', b'\n']);
        v
    }

    #[test]
    fn process_output_is_independent_of_chunking() {
        let input = fixture();
        let mk = || (filter_with(&[7, 9, 11]), store(&[7, 9, 11]));
        let (mut f, mut s) = mk();
        let whole = run(&mut f, &mut s, &input);
        assert!(transmits(&whole) >= 4);

        let (mut f, mut s) = mk();
        let mut bytewise = Vec::new();
        for b in &input {
            bytewise.extend(f.process(&[*b], &mut s));
        }
        bytewise.extend(f.flush());
        assert_eq!(bytewise, whole);

        for cut in 0..=input.len() {
            let (mut f, mut s) = mk();
            let mut out = f.process(&input[..cut], &mut s);
            out.extend(f.process(&input[cut..], &mut s));
            out.extend(f.flush());
            assert_eq!(out, whole, "cut {cut}");
        }
    }

    #[test]
    fn palette_and_truecolour_foregrounds_name_the_same_id_space() {
        let drawn = |sgr: &[u8], ids: &[u32]| {
            let mut f = filter_with(ids);
            let mut s = store(ids);
            let mut input = sgr.to_vec();
            input.extend_from_slice(&GLYPH);
            apcs(&run(&mut f, &mut s, &input))
        };
        assert!(drawn(b"\x1b[38;5;77m", &[77])[0].contains("i=77"));
        assert!(drawn(b"\x1b[38;2;1;2;3m", &[66051])[0].contains("i=66051"));
        for reset in [&b"\x1b[39m"[..], b"\x1b[0m", b"\x1b[m"] {
            let mut sgr = b"\x1b[38;5;77m".to_vec();
            sgr.extend_from_slice(reset);
            assert!(drawn(&sgr, &[77]).is_empty(), "{reset:?}");
        }
    }

    #[test]
    fn unknown_id_passes_through_asks_for_a_label_and_a_later_def_draws_next_row() {
        let mut f = GraphicsFilter::new(true);
        let mut s = store(&[8]);
        let first = run(&mut f, &mut s, &row(8));
        assert_eq!(first, row(8));
        assert!(f.wants_label(8));
        assert_eq!(f.skipped(), 1);
        f.on_image_def(&def(8, 1, 90, 19));
        assert!(!f.wants_label(8));
        let next = run(&mut f, &mut s, &row(8));
        assert_eq!(transmits(&next), 1);
        assert!(!f.wants_label(8));
    }

    #[test]
    fn zero_geometry_or_missing_png_emits_nothing_and_asks_for_a_label() {
        let mut f = GraphicsFilter::new(true);
        f.on_image_def(&def(3, 1, 0, 19));
        f.on_image_def(&def(4, 1, 90, 0));
        f.on_image_def(&def(5, 1, 90, 19));
        f.on_image_def(&def(6, 1, 90, 19));
        let mut s = store(&[3, 4]);
        s.0.insert(sha(6), b"not a png".to_vec());
        for id in [3, 4, 5, 6] {
            let out = run(&mut f, &mut s, &row(id));
            assert_eq!(out, row(id));
            assert!(apcs(&out).is_empty());
            assert!(f.wants_label(id), "id {id}");
        }
        assert_eq!(f.skipped(), 4);
    }

    #[test]
    fn labels_mode_filter_is_the_identity() {
        let mut f = filter_with(&[7, 9, 11]);
        assert!(f.set_kitty(false).is_empty());
        let mut s = store(&[7, 9, 11]);
        let input = fixture();
        assert_eq!(f.process(&input, &mut s), input);
        assert!(f.flush().is_empty());
        let mut g = GraphicsFilter::new(false);
        assert_eq!(g.process(&input, &mut s), input);
    }

    #[test]
    fn toggling_mode_with_a_held_glyph_prefix_drops_no_byte() {
        // kitty -> labels: a chunk ends inside a glyph; the prefix comes
        // back from set_kitty and is written first.
        let mut f = filter_with(&[7]);
        let mut s = store(&[7]);
        let mut input = row(7);
        input.splice(0..0, b"ab".iter().copied());
        let cut = input.iter().position(|&b| b == 0xF4).unwrap() + 2;
        let mut got = f.process(&input[..cut], &mut s);
        got.extend(f.set_kitty(false));
        got.extend(f.process(&input[cut..], &mut s));
        got.extend(f.flush());
        assert_eq!(strip(&got, 7, 90, 19, &png(7)), input);
        // labels -> kitty mid-row: nothing is lost either.
        let mut g = GraphicsFilter::new(false);
        let mut out = g.process(&input[..cut], &mut s);
        out.extend(g.set_kitty(true));
        out.extend(g.process(&input[cut..], &mut s));
        out.extend(g.flush());
        assert_eq!(out, input);
    }

    #[test]
    fn kitty_labels_erase_kitty_retransmits() {
        let mut f = filter_with(&[7]);
        let mut s = store(&[7]);
        let first = run(&mut f, &mut s, &row(7));
        assert_eq!(transmits(&first), 1);
        assert!(f.set_kitty(false).is_empty());
        assert_eq!(f.process(b"\x1b[2J", &mut s), b"\x1b[2J");
        assert!(f.set_kitty(true).is_empty());
        let again = run(&mut f, &mut s, &row(7));
        assert_eq!(transmits(&again), 1);
    }

    #[test]
    fn imagedef_with_an_invalid_key_is_refused() {
        let frame = |sha: &str| ServerFrame::Imagedef {
            seq: 0,
            id: 7,
            gen: 1,
            sha256: sha.to_string(),
            mime: "image/png".to_string(),
            w: 10,
            h: 10,
            cols: 5,
            rows: 2,
            offset: 0,
        };
        assert!(ImageDef::from_frame(&frame(&sha(7))).is_some());
        let (g64, a64) = ("g".repeat(64), "A".repeat(64));
        for bad in ["", "abc", "../etc/passwd", g64.as_str(), a64.as_str()] {
            assert!(ImageDef::from_frame(&frame(bad)).is_none(), "{bad}");
        }
        struct Never;
        impl ImageSource for Never {
            fn png(&mut self, _: &str) -> Option<Vec<u8>> {
                panic!("fetcher called with an unvalidated key");
            }
        }
        let mut f = GraphicsFilter::new(true);
        f.on_image_def(&ImageDef {
            id: 7,
            gen: 1,
            sha256: "../x".to_string(),
            cols: 5,
            rows: 2,
        });
        let out = f.process(&row(7), &mut Never);
        assert_eq!(transmits(&out), 0);
        assert_eq!(out, row(7));
    }

    #[test]
    fn decide_mode_table() {
        let none = |_: &str| -> Option<String> { None };
        let ok = ProbeResult {
            xtversion: Some("ghostty 1.2.0".into()),
            kitty_ok: true,
            saw_da1: true,
            leftover: vec![],
        };
        let req = |flag, env: &dyn Fn(&str) -> Option<String>, probe, local, grid| {
            decide_mode(&ModeRequest {
                graphics_flag: flag,
                env,
                probe,
                local_cols: local,
                grid_cols: grid,
            })
        };
        assert_eq!(
            req(false, &none, Some(&ok), 200, 100),
            (PictureMode::Labels, None)
        );
        for var in ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "TMUX"] {
            let env = move |k: &str| (k == var).then(|| "x".to_string());
            let (m, cap) = req(true, &env, Some(&ok), 200, 100);
            assert_eq!(m, PictureMode::Labels);
            assert!(cap.unwrap().contains("inside a multiplexer"));
            assert!(!should_probe(true, &env));
        }
        assert!(should_probe(true, &none));
        assert!(!should_probe(false, &none));
        let empty = ProbeResult::default();
        for probe in [None, Some(&empty)] {
            let (m, cap) = req(true, &none, probe, 200, 100);
            assert_eq!(m, PictureMode::Labels);
            assert!(cap.unwrap().contains("mosh"));
        }
        let (m, cap) = req(true, &none, Some(&ok), 80, 120);
        assert_eq!(m, PictureMode::Labels);
        let cap = cap.unwrap();
        assert!(
            cap.contains("(80 cols)") && cap.contains("(120 cols)"),
            "{cap}"
        );
        assert_eq!(
            req(true, &none, Some(&ok), 120, 120),
            (PictureMode::Kitty, None)
        );
        assert_eq!(
            req(true, &none, Some(&ok), 200, 120),
            (PictureMode::Kitty, None)
        );
    }

    #[test]
    fn probe_bytes_are_exact_and_parse_replies() {
        assert_eq!(
            probe_bytes(),
            b"\x1b[>0q\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c".to_vec()
        );
        let reply = b"\x1bP>|ghostty 1.2.0\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[?62;c";
        let mut with_tail = reply.to_vec();
        with_tail.extend_from_slice(b"abc");
        let r = parse_probe_replies(&with_tail);
        assert_eq!(r.xtversion.as_deref(), Some("ghostty 1.2.0"));
        assert!(r.kitty_ok && r.saw_da1);
        assert_eq!(r.leftover, b"abc");
        assert!(r.is_positive());

        let wez = parse_probe_replies(b"\x1bP>|WezTerm 2024\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[?62;c");
        assert!(wez.kitty_ok && !wez.is_positive());
        let no_kitty = parse_probe_replies(b"\x1bP>|ghostty 1.2.0\x1b\\\x1b[?62;c");
        assert!(!no_kitty.kitty_ok && !no_kitty.is_positive());
        let kitty = parse_probe_replies(b"\x1bP>|kitty(0.40)\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[?62;c");
        assert!(kitty.is_positive());
        let early = parse_probe_replies(b"x\x1bP>|ghostty 1\x1b\\");
        assert!(!early.saw_da1);
        assert_eq!(early.leftover, b"x");
    }

    #[test]
    fn stdin_filter_drops_terminal_replies_and_keeps_keys() {
        let replies: [&[u8]; 7] = [
            b"\x1b_Gi=31;OK\x1b\\",
            b"\x1bP>|ghostty 1.2.0\x1b\\",
            b"\x1b[?62;4c",
            b"\x1b[>1;10;0c",
            b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07",
            b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\",
            b"\x1b]11;rgb:0000/0000/0000\x1b\\",
        ];
        for reply in replies {
            let mut whole = StdinFilter::new();
            let mut input = b"a".to_vec();
            input.extend_from_slice(reply);
            input.push(b'b');
            let mut out = whole.feed(&input);
            out.extend(whole.flush());
            assert_eq!(out, b"ab", "{reply:?}");
            for cut in 0..=input.len() {
                let mut f = StdinFilter::new();
                let mut out = f.feed(&input[..cut]);
                out.extend(f.feed(&input[cut..]));
                out.extend(f.flush());
                assert_eq!(out, b"ab", "{reply:?} cut {cut}");
            }
        }
        let keys: &[u8] = b"x\x1b[A\x1b[1;2R\x1bOP\x1b[200~hi\x1b[201~y";
        for cut in 0..=keys.len() {
            let mut f = StdinFilter::new();
            let mut out = f.feed(&keys[..cut]);
            out.extend(f.feed(&keys[cut..]));
            out.extend(f.flush());
            assert_eq!(out, keys, "cut {cut}");
        }
        let mut f = StdinFilter::new();
        assert!(f.feed(b"\x1b").is_empty());
        assert!(f.has_pending());
        assert_eq!(f.flush(), b"\x1b");
        assert!(!f.has_pending());
        let mut f = StdinFilter::new();
        let mut long = b"\x1b_G".to_vec();
        long.extend(std::iter::repeat_n(b'z', 300));
        let out = f.feed(&long);
        assert!(
            !out.is_empty(),
            "an unterminated hold is released at the bound"
        );
    }

    #[test]
    fn cell_px_from_winsize_divides_and_refuses_zero() {
        assert_eq!(cell_px_from_winsize(200, 50, 2000, 1500), Some((10, 30)));
        assert_eq!(cell_px_from_winsize(0, 50, 2000, 1500), None);
        assert_eq!(cell_px_from_winsize(200, 0, 2000, 1500), None);
        assert_eq!(cell_px_from_winsize(200, 50, 0, 1500), None);
        assert_eq!(cell_px_from_winsize(200, 50, 2000, 0), None);
    }
}
