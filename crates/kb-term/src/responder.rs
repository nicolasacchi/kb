//! The terminal responder: answers tmux's queries to its outer client.
//!
//! Runs on the pty-master side of the anchor and every hidden input
//! client. A byte-at-a-time state machine over ground / ESC / CSI / OSC /
//! DCS / string-terminator states; only exact query shapes reply, never a
//! set, and never anything about mode 2031. Hold-back caps (CSI 64 B, OSC
//! / DCS 256 B) bound memory: past a cap the sequence is consumed up to
//! its terminator and never parsed as a new one. Behaviour is identical
//! however the input is chunked: no timer flush, end of input leaves the
//! state intact for the next [`Responder::feed`].
//!
//! The pump ([`spawn_pump`]) reads a client's pty master, feeds the
//! responder, and hands replies to one writer task per client (through an
//! mpsc), so a reply can never land inside a key sequence.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;

use crate::runner::RunnerError;

/// DA1 reply: VT220-class + ANSI colour. Attribute 4 (sixel) is never
/// advertised, so no client gets the sixel feature.
const DA1_REPLY: &[u8] = b"\x1b[?62;22c";
/// DA2 reply.
const DA2_REPLY: &[u8] = b"\x1b[>1;10;0c";
/// XTVERSION names kb-term, not a known terminal (a known name would
/// change omp's capability profile).
const XTVERSION_NAME: &str = "kb-term";

/// Responder configuration: fixed profile colours, theme direction.
#[derive(Debug, Clone)]
pub struct ResponderCfg {
    /// Foreground `#RRGGBB` (OSC 10 reply).
    pub fg: String,
    /// Background `#RRGGBB` (OSC 11 reply).
    pub bg: String,
    /// Dark theme (answers `CSI ? 997;1n` when true, `;2n` otherwise).
    pub dark: bool,
}

impl Default for ResponderCfg {
    fn default() -> Self {
        ResponderCfg {
            fg: "#d4d4d4".to_string(),
            bg: "#1e1e1e".to_string(),
            dark: true,
        }
    }
}

impl ResponderCfg {
    /// OSC 10 reply bytes without the terminator (caller appends one).
    fn osc10_head(&self) -> String {
        format!("\x1b]10;rgb:{}", hex16(&self.fg))
    }

    /// OSC 11 reply bytes without the terminator (caller appends one).
    fn osc11_head(&self) -> String {
        format!("\x1b]11;rgb:{}", hex16(&self.bg))
    }
}

/// `#RRGGBB` -> `RRRR/GGGG/BBBB` (each byte doubled, 16 bits per channel).
fn hex16(s: &str) -> String {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() == 6 {
        let parse = |h: &str| u8::from_str_radix(h, 16);
        if let (Ok(a), Ok(b), Ok(c)) = (parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6])) {
            return format!("{a:02x}{a:02x}/{b:02x}{b:02x}/{c:02x}{c:02x}");
        }
    }
    // Malformed config falls back to the default profile colour (grey).
    "d4d4/d4d4/d4d4".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
    Dcs,
    DcsEsc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminator {
    Bel,
    St,
}

/// The responder state machine. [`feed`](Responder::feed) appends reply
/// bytes to `out`; the caller writes them to the master.
pub struct Responder {
    cfg: ResponderCfg,
    state: State,
    buf: Vec<u8>,
    ignoring: bool,
    queries: u64,
}

impl Responder {
    /// Build a responder from the profile configuration.
    pub fn new(cfg: ResponderCfg) -> Self {
        Responder {
            cfg,
            state: State::Ground,
            buf: Vec::new(),
            ignoring: false,
            queries: 0,
        }
    }

    /// Number of queries answered so far (for client stats).
    pub fn queries_answered(&self) -> u64 {
        self.queries
    }

    /// Test accessor: current buffer length (memory bound).
    #[cfg(test)]
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// Feed raw bytes from the client's pty; append replies to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &byte in bytes {
            self.feed_byte(byte, out);
        }
    }

    fn feed_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Ground => {
                // C1 bytes are ignored so UTF-8 continuations are never
                // misread as C1 starts.
                if byte == 0x1B {
                    self.state = State::Esc;
                    self.buf.clear();
                    self.ignoring = false;
                }
            }
            State::Esc => match byte {
                b'[' => {
                    self.state = State::Csi;
                    self.buf.clear();
                }
                b']' => {
                    self.state = State::Osc;
                    self.buf.clear();
                }
                b'P' => {
                    self.state = State::Dcs;
                    self.buf.clear();
                }
                // A fresh ESC restarts the escape; ST and any other second
                // byte end it with no output.
                0x1B => {}
                _ => {
                    self.state = State::Ground;
                }
            },
            State::Csi => self.feed_csi(byte, out),
            State::Osc | State::Dcs => self.feed_string(byte, out),
            State::OscEsc => {
                if byte == b'\\' {
                    self.finish_string(Terminator::St, out);
                } else {
                    // Abort the string and start a new escape from this
                    // byte (as if it were the second byte after ESC).
                    self.abort_to_escape(byte);
                }
            }
            State::DcsEsc => {
                if byte == b'\\' {
                    self.finish_string(Terminator::St, out);
                } else {
                    self.abort_to_escape(byte);
                }
            }
        }
    }

    fn abort_to_escape(&mut self, byte: u8) {
        // Process `byte` as the second byte of a new escape.
        self.ignoring = false;
        self.buf.clear();
        match byte {
            b'[' => self.state = State::Csi,
            b']' => self.state = State::Osc,
            b'P' => self.state = State::Dcs,
            0x1B => self.state = State::Esc,
            _ => self.state = State::Ground,
        }
    }

    fn feed_csi(&mut self, byte: u8, out: &mut Vec<u8>) {
        if byte == 0x1B {
            self.state = State::Esc;
            self.buf.clear();
            self.ignoring = false;
            return;
        }
        if (0x80..=0x9F).contains(&byte) {
            return;
        }
        if (0x40..=0x7E).contains(&byte) {
            // Final byte: complete the sequence.
            if !self.ignoring {
                self.evaluate_csi(byte, out);
            }
            self.state = State::Ground;
            self.buf.clear();
            self.ignoring = false;
            return;
        }
        // C0 controls abort the sequence; DEL is ignored; everything else
        // is a parameter/intermediate byte.
        if byte < 0x20 {
            self.state = State::Ground;
            self.buf.clear();
            self.ignoring = false;
            return;
        }
        if byte == 0x7F {
            return;
        }
        if !self.ignoring {
            self.buf.push(byte);
            if self.buf.len() > 64 {
                self.ignoring = true;
                self.buf.clear();
            }
        }
    }
    fn feed_string(&mut self, byte: u8, out: &mut Vec<u8>) {
        if byte == 0x07 && matches!(self.state, State::Osc | State::Dcs) {
            self.finish_string(Terminator::Bel, out);
            return;
        }
        if byte == 0x1B {
            // ESC \ terminates; ESC + anything else aborts (handled by
            // the OscEsc/DcsEsc arms on the next byte).
            self.state = if self.state == State::Osc {
                State::OscEsc
            } else {
                State::DcsEsc
            };
            return;
        }
        // C1 bytes (including 0x9C) are ignored so UTF-8 continuations
        // inside a string payload are never misread as terminators.
        if (0x80..=0x9F).contains(&byte) {
            return;
        }
        if !self.ignoring {
            self.buf.push(byte);
            if self.buf.len() > 256 {
                self.ignoring = true;
                self.buf.clear();
            }
        }
    }

    fn finish_string(&mut self, term: Terminator, out: &mut Vec<u8>) {
        if !self.ignoring && matches!(self.state, State::Osc | State::OscEsc) {
            self.evaluate_osc(term, out);
        }
        self.state = State::Ground;
        self.buf.clear();
        self.ignoring = false;
    }

    fn evaluate_csi(&mut self, final_byte: u8, out: &mut Vec<u8>) {
        let params = self.buf.clone();
        // Exact query shapes only; every other final/params is ignored.
        if final_byte == b'c' {
            if params.starts_with(b">") {
                let rest = &params[1..];
                if rest.is_empty() || rest == b"0" {
                    self.answer(out, "da2", DA2_REPLY);
                    return;
                }
            } else if params.is_empty() || params == b"0" {
                self.answer(out, "da1", DA1_REPLY);
                return;
            }
        }
        if final_byte == b'q' && params.starts_with(b">") {
            let rest = &params[1..];
            if rest.is_empty() || rest == b"0" {
                // Single DCS string: ESC P > | <name> <ver> ESC \
                // (8-bit `|` 0x7C marks XTVERSION for tmux; the ST
                // terminates the DCS, so the name is payload).
                let mut reply = b"\x1bP>|".to_vec();
                reply.extend_from_slice(XTVERSION_NAME.as_bytes());
                reply.push(b' ');
                reply.extend_from_slice(env!("CARGO_PKG_VERSION").as_bytes());
                reply.extend_from_slice(b"\x1b\\");
                self.answer(out, "xtversion", &reply);
            }
            return;
        }
        if final_byte == b'n' && params == b"?996" {
            let code = if self.cfg.dark { b"1" } else { b"2" };
            let mut reply = b"\x1b[?997;".to_vec();
            reply.push(code[0]);
            reply.push(b'n');
            self.answer(out, "theme", &reply);
        }
    }

    fn evaluate_osc(&mut self, term: Terminator, out: &mut Vec<u8>) {
        let terminator: &[u8] = match term {
            Terminator::Bel => b"\x07",
            Terminator::St => b"\x1b\\",
        };
        if self.buf == b"10;?" {
            let mut reply = self.cfg.osc10_head().into_bytes();
            reply.extend_from_slice(terminator);
            self.answer(out, "osc10", &reply);
        } else if self.buf == b"11;?" {
            let mut reply = self.cfg.osc11_head().into_bytes();
            reply.extend_from_slice(terminator);
            self.answer(out, "osc11", &reply);
        }
    }

    fn answer(&mut self, out: &mut Vec<u8>, shape: &'static str, reply: &[u8]) {
        self.queries += 1;
        tracing::debug!(shape, "responder answered terminal query");
        out.extend_from_slice(reply);
    }
}

/// Shared pump state (stats + activity clock).
#[derive(Debug)]
struct PumpShared {
    bytes_drained: AtomicU64,
    queries_answered: AtomicU64,
    last_output: std::sync::Mutex<Option<Instant>>,
    closed: AtomicBool,
    /// Why the read loop exited (0 = still open). Test-visible reason codes:
    /// 1 = `readable()` failed, 2 = EOF (read returned 0), 3 = a read error
    /// (see `closed_errno`).
    closed_reason: AtomicU32,
    /// The `raw_os_error` of the `readable()`/read failure, if any.
    closed_errno: AtomicI32,
}

/// One per-client pump handle: feed replies / keys / items to the hidden
/// client's single writer task.
#[derive(Debug, Clone)]
pub struct PumpHandle {
    tx: mpsc::Sender<Vec<u8>>,
    shared: Arc<PumpShared>,
}

impl PumpHandle {
    /// Queue one item to the writer (whole, never split). Bounded at 2 s:
    /// a stuck client answers `busy-pty` instead of hanging the RPC.
    pub async fn write(&self, item: Vec<u8>) -> Result<(), RunnerError> {
        tokio::time::timeout(Duration::from_secs(2), self.tx.send(item))
            .await
            .map_err(|_| RunnerError::BusyPty("client writer busy".to_string()))?
            .map_err(|_| RunnerError::BusyPty("client pty closed".to_string()))?;
        Ok(())
    }

    /// Bytes read (and discarded) from the master so far.
    pub fn bytes_drained(&self) -> u64 {
        self.shared.bytes_drained.load(Ordering::Relaxed)
    }

    /// Queries the responder has answered so far.
    pub fn queries_answered(&self) -> u64 {
        self.shared.queries_answered.load(Ordering::Relaxed)
    }

    /// When the master last produced bytes (the quiet clock).
    pub fn last_output(&self) -> Option<Instant> {
        *self.shared.last_output.lock().unwrap()
    }

    /// Whether the master reported EOF/EIO.
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Relaxed)
    }

    /// Whether both handles point at the same pump (same shared state).
    pub fn is_same(&self, other: &PumpHandle) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// Why the read loop exited (0 = still open; see `PumpShared`).
    pub fn closed_reason(&self) -> u32 {
        self.shared.closed_reason.load(Ordering::Relaxed)
    }

    /// The `raw_os_error` behind the close, if the failure carried one.
    pub fn closed_errno(&self) -> i32 {
        self.shared.closed_errno.load(Ordering::Relaxed)
    }

    /// How long the master has been quiet (`None` if never active).
    pub fn quiet_for(&self) -> Option<Duration> {
        self.last_output().map(|t| t.elapsed())
    }
}

/// Start the pump: a read loop feeding `master` through the responder and
/// a single writer task owning `writer`. `master` and `writer` are two
/// fds for the same pty (dups).
pub fn spawn_pump(
    master: AsyncFd<std::fs::File>,
    writer: AsyncFd<std::fs::File>,
    cfg: ResponderCfg,
) -> PumpHandle {
    let shared = Arc::new(PumpShared {
        bytes_drained: AtomicU64::new(0),
        queries_answered: AtomicU64::new(0),
        last_output: std::sync::Mutex::new(None),
        closed: AtomicBool::new(false),
        closed_reason: AtomicU32::new(0),
        closed_errno: AtomicI32::new(0),
    });
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    // Writer task: one per client, whole items only.
    tokio::spawn(async move {
        while let Some(item) = rx.recv().await {
            if write_all_async(&writer, &item).await.is_err() {
                break;
            }
        }
    });
    let write_tx = tx.clone();
    let read_shared = shared.clone();
    tokio::spawn(async move {
        let mut responder = Responder::new(cfg);
        let mut buf = vec![0u8; 65536];
        loop {
            let mut readable = match master.readable().await {
                Ok(g) => g,
                Err(e) => {
                    read_shared.closed_reason.store(1, Ordering::Relaxed);
                    read_shared
                        .closed_errno
                        .store(e.raw_os_error().unwrap_or(0), Ordering::Relaxed);
                    break;
                }
            };
            let n = readable.try_io(|inner: &AsyncFd<std::fs::File>| {
                use std::io::Read;
                inner.get_ref().read(&mut buf)
            });
            match n {
                Ok(Ok(0)) => {
                    read_shared.closed_reason.store(2, Ordering::Relaxed);
                    break;
                }
                Ok(Ok(n)) => {
                    read_shared
                        .bytes_drained
                        .fetch_add(n as u64, Ordering::Relaxed);
                    *read_shared.last_output.lock().unwrap() = Some(Instant::now());
                    let mut out = Vec::new();
                    let before = responder.queries_answered();
                    responder.feed(&buf[..n], &mut out);
                    let added = responder.queries_answered() - before;
                    if added > 0 {
                        // Replies queue into the same writer as keys.
                        // Await the send (2 s bound, like
                        // PumpHandle::write) so a reply is never
                        // silently dropped while still counted; the
                        // counter only moves for replies actually queued.
                        let sent = tokio::time::timeout(Duration::from_secs(2), write_tx.send(out))
                            .await
                            .is_ok_and(|r| r.is_ok());
                        if sent {
                            read_shared
                                .queries_answered
                                .fetch_add(added, Ordering::Relaxed);
                        }
                    }
                }
                // `try_io` maps a `WouldBlock` read to `Err(TryIoError)`
                // (and clears readiness there), so that arm must re-wait,
                // never mark the pump closed: a freshly registered AsyncFd
                // starts "ready", so the first read usually WouldBlocks
                // before tmux writes anything.
                Err(_) => continue,
                Ok(Err(e)) => {
                    read_shared.closed_reason.store(3, Ordering::Relaxed);
                    read_shared
                        .closed_errno
                        .store(e.raw_os_error().unwrap_or(0), Ordering::Relaxed);
                    break;
                }
            }
        }
        read_shared.closed.store(true, Ordering::Relaxed);
    });
    PumpHandle { tx, shared }
}

async fn write_all_async(fd: &AsyncFd<std::fs::File>, item: &[u8]) -> Result<(), ()> {
    if item.is_empty() {
        return Ok(());
    }
    let mut pos = 0usize;
    while pos < item.len() {
        let mut writable = fd.writable().await.map_err(|_| ())?;
        let n = writable.try_io(|inner: &AsyncFd<std::fs::File>| {
            use std::io::Write;
            let mut f = inner.get_ref().try_clone()?;
            f.write(&item[pos..])
        });
        match n {
            Ok(Ok(k)) => pos += k,
            // As in the read loop, `Err(TryIoError)` is the WouldBlock
            // signal: readiness was already cleared by `try_io`, so retry
            // instead of killing the writer.
            Err(_) => continue,
            Ok(Err(_)) => return Err(()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(cfg: ResponderCfg, input: &[u8]) -> Vec<u8> {
        let mut r = Responder::new(cfg);
        let mut out = Vec::new();
        r.feed(input, &mut out);
        out
    }

    #[test]
    fn responder_replies_are_pinned_goldens() {
        let dark = ResponderCfg::default();
        let ver = env!("CARGO_PKG_VERSION");
        // Single DCS string ESC P > | kb-term <ver> ESC \ : the `|`
        // (0x7C) marks XTVERSION for tmux; the ST terminates the DCS.
        let xt = format!("\x1bP>|{XTVERSION_NAME} {ver}\x1b\\");
        assert_eq!(feed_all(dark.clone(), b"\x1b[>q"), xt.as_bytes());
        assert_eq!(feed_all(dark.clone(), b"\x1b[>0q"), xt.as_bytes());
        assert_eq!(feed_all(dark.clone(), b"\x1b[?996n"), b"\x1b[?997;1n");
        let light = ResponderCfg {
            dark: false,
            ..dark.clone()
        };
        assert_eq!(feed_all(light, b"\x1b[?996n"), b"\x1b[?997;2n");
        // OSC 10/11 with BEL and with ST terminators (mirror the query).
        assert_eq!(
            feed_all(dark.clone(), b"\x1b]10;?\x07"),
            b"\x1b]10;rgb:d4d4/d4d4/d4d4\x07"
        );
        assert_eq!(
            feed_all(dark.clone(), b"\x1b]10;?\x1b\\"),
            b"\x1b]10;rgb:d4d4/d4d4/d4d4\x1b\\"
        );
        assert_eq!(
            feed_all(dark.clone(), b"\x1b]11;?\x07"),
            b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07"
        );
        assert_eq!(
            feed_all(dark.clone(), b"\x1b]11;?\x1b\\"),
            b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\"
        );
    }

    #[test]
    fn da1_reply_never_advertises_sixel() {
        for dark in [true, false] {
            for fg in ["#d4d4d4", "#000000", "#ffffff"] {
                let cfg = ResponderCfg {
                    fg: fg.to_string(),
                    dark,
                    ..ResponderCfg::default()
                };
                let reply = feed_all(cfg, b"\x1b[c");
                assert_eq!(reply, DA1_REPLY);
                // Parse the CSI parameter string properly: strip the
                // ESC [ ? prefix and c suffix, split on `;`, assert
                // every attribute (a leading `4` would be missed by
                // slicing a fixed span).
                let text = std::str::from_utf8(&reply).unwrap();
                let params = text
                    .strip_prefix("\x1b[?")
                    .and_then(|s| s.strip_suffix('c'))
                    .expect("DA1 reply must be CSI ? ... c");
                let attrs: Vec<&str> = params.split(';').collect();
                assert_eq!(attrs, ["62", "22"], "DA1 attributes pinned");
                assert!(!attrs.contains(&"4"), "DA1 must not contain attribute 4");
            }
        }
    }

    fn sample_stream() -> Vec<u8> {
        let mut s = Vec::new();
        s.extend_from_slice(b"\x1b[c");
        s.extend_from_slice(b"noise bytes\x07\x00\x01");
        s.extend_from_slice(b"\x1b[>0c");
        s.extend_from_slice(b"\x1b[>q");
        s.extend_from_slice(b"\x1b[?996n");
        s.extend_from_slice(b"\x1b]11;?\x07");
        s.extend_from_slice(b"\x1b]10;?\x1b\\");
        s.extend_from_slice(b"\x1b[16t");
        s.extend_from_slice(b"\x1b]11;rgb:00/00/00\x07");
        s.extend_from_slice(b"\x1b[>4;2m");
        s.extend_from_slice(&[0x80, 0x9f, 0xC3, 0xA9]);
        s.extend_from_slice(b"\x1bP+q1234\x1b\\");
        s.extend_from_slice(b"\x1b[0m");
        s
    }

    fn feed_split(cfg: ResponderCfg, stream: &[u8], cuts: &[usize]) -> Vec<u8> {
        let mut r = Responder::new(cfg);
        let mut out = Vec::new();
        let mut prev = 0;
        for &cut in cuts {
            r.feed(&stream[prev..cut], &mut out);
            prev = cut;
        }
        r.feed(&stream[prev..], &mut out);
        out
    }

    #[test]
    fn responder_is_chunking_invariant() {
        let cfg = ResponderCfg::default();
        let stream = sample_stream();
        let whole = feed_split(cfg.clone(), &stream, &[]);
        // Every 1-cut.
        for cut in 0..stream.len() {
            assert_eq!(
                feed_split(cfg.clone(), &stream, &[cut]),
                whole,
                "1-cut at {cut}"
            );
        }
        // Every pair of cuts.
        for a in 0..stream.len() {
            for b in a..stream.len() {
                assert_eq!(
                    feed_split(cfg.clone(), &stream, &[a, b]),
                    whole,
                    "2-cuts at {a},{b}"
                );
            }
        }
    }

    #[test]
    fn responder_ignores_sets_and_unknown_and_never_answers_2031() {
        let mut r = Responder::new(ResponderCfg::default());
        let mut out = Vec::new();
        for seq in [
            &b"\x1b]11;rgb:00/00/00\x07"[..],
            b"\x1b[>4;2m",
            b"\x1b[?1;2$r",
            b"\x1b[16t",
            b"\x1b[?u",
            b"\x1b[?2004$p",
            b"\x1b[31m",
            b"\x1b[2J",
            b"\x1b[1;2H",
            b"\x1b]52;c;?\x07",
            b"\x1b[?2031h",
            b"\x1b[?2031l",
            b"\x1b[?2031$p",
        ] {
            r.feed(seq, &mut out);
        }
        assert!(out.is_empty(), "sets/unknown must emit nothing: {out:?}");
        assert!(
            !out.windows(4).any(|w| w == b"2031"),
            "no output may mention 2031"
        );
    }

    #[test]
    fn responder_overlong_sequences_are_dropped_not_answered_and_memory_stays_bounded() {
        // DA1 padded past 64 B: no reply, but a normal DA1 afterwards answers.
        let mut overlong = b"\x1b[".to_vec();
        overlong.extend([b'9'; 100]);
        overlong.extend_from_slice(b"c");
        let mut r = Responder::new(ResponderCfg::default());
        let mut out = Vec::new();
        r.feed(&overlong, &mut out);
        assert!(out.is_empty(), "overlong CSI must not reply: {out:?}");
        assert!(
            r.buffered_len() <= 64,
            "CSI buffer bounded at 64, got {}",
            r.buffered_len()
        );
        r.feed(b"\x1b[c", &mut out);
        assert_eq!(&out[..], DA1_REPLY);
        // OSC 11 query padded past 256 B: no reply; next query answers.
        let mut osc = b"\x1b]11;?".to_vec();
        osc.extend([b' '; 300]);
        osc.extend_from_slice(b"\x07");
        let mut r = Responder::new(ResponderCfg::default());
        let mut out = Vec::new();
        r.feed(&osc, &mut out);
        assert!(out.is_empty(), "overlong OSC must not reply: {out:?}");
        assert!(
            r.buffered_len() <= 256,
            "OSC buffer bounded at 256, got {}",
            r.buffered_len()
        );
        r.feed(b"\x1b]11;?\x07", &mut out);
        assert_eq!(&out[..], b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07");
    }

    #[test]
    fn responder_survives_c1_and_garbage() {
        // Seeded PRNG over random bytes: never panics, and the state
        // returns to a usable ground after `ESC \` + a fresh DA1.
        let mut seed: u64 = 0x243F6A8885A308D3;
        let mut noise = [0u8; 8192];
        for slot in noise.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *slot = (seed >> 32) as u8;
        }
        // Sprinkle C1 bytes and escape introducers through the noise.
        for (i, b) in noise.iter_mut().enumerate() {
            if i % 97 == 0 {
                *b = 0x93;
            } else if i % 131 == 0 {
                *b = 0x1B;
            }
        }
        let mut r = Responder::new(ResponderCfg::default());
        let mut out = Vec::new();
        r.feed(&noise, &mut out);
        // Reset with ST, then a DA1 must answer: proves ground recovery.
        r.feed(b"\x1b\\", &mut out);
        out.clear();
        r.feed(b"\x1b[c", &mut out);
        assert_eq!(&out[..], DA1_REPLY);
        // Random C1/UTF-8 soup in ground never answers spuriously.
        let mut c1 = Vec::new();
        for b in 0x80u8..=0x9F {
            c1.push(b);
        }
        c1.extend_from_slice("héllo→".as_bytes());
        let mut out2 = Vec::new();
        r.feed(&c1, &mut out2);
        assert!(out2.is_empty());
    }
    #[test]
    fn responder_dcs_bel_terminates_then_da1_answers() {
        // A DCS string closed by BEL must not swallow the next query.
        let mut r = Responder::new(ResponderCfg::default());
        let mut out = Vec::new();
        r.feed(b"\x1bP1$r\x07", &mut out);
        assert!(out.is_empty(), "DCS content answers nothing: {out:?}");
        r.feed(b"\x1b[c", &mut out);
        assert_eq!(
            &out[..],
            DA1_REPLY,
            "DA1 after BEL-terminated DCS must answer"
        );
    }

    #[test]
    fn responder_c1_in_string_is_not_a_terminator() {
        // 0x9C inside an OSC payload is a UTF-8 continuation, not ST:
        // the string stays open, the DA1 inside it answers nothing,
        // and chunking never changes the outcome.
        let mut stream = b"\x1b]11;?".to_vec();
        // A payload byte that would look like a C1 ST if misread.
        stream.push(0x9C);
        stream.extend_from_slice(b"\x07");
        stream.extend_from_slice(b"\x1b[c");
        let whole = feed_all(ResponderCfg::default(), &stream);
        // The 0x9C is ignored, so the payload stays exactly `11;?`
        // and the BEL terminates it: the OSC 11 reply (BEL-mirrored)
        // plus the trailing DA1. Under the old C1-ST reading the
        // terminator would have been ST and this would fail.
        let mut want = b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07".to_vec();
        want.extend_from_slice(DA1_REPLY);
        assert_eq!(&whole[..], &want[..]);
        for cut in 0..stream.len() {
            assert_eq!(
                feed_split(ResponderCfg::default(), &stream, &[cut]),
                whole,
                "1-cut at {cut}"
            );
        }
    }

    #[test]
    fn responder_queries_answered_counts_every_query_in_a_batch() {
        // Two queries in one feed batch count as two (the pump's
        // stats delta the shared counter moves by).
        let mut r = Responder::new(ResponderCfg::default());
        let mut out = Vec::new();
        let before = r.queries_answered();
        r.feed(b"\x1b[c\x1b[>q", &mut out);
        assert_eq!(r.queries_answered() - before, 2);
        assert_eq!(r.queries_answered(), 2);
    }

    #[tokio::test]
    async fn pump_counts_every_query_over_a_pty_pair() {
        // The shared counter the pump moves per batch: feed two queries
        // in ONE write to the slave side and wait for the handle to
        // report both answered (fails if the pump drops/miscounts the
        // shared fetch_add).
        let (master, slave) = crate::pty::open_pty(80, 24, 720, 432).unwrap();
        let reader = crate::pty::async_fd_dup(&master).unwrap();
        let writer = crate::pty::async_fd_dup(&master).unwrap();
        let handle = spawn_pump(reader, writer, ResponderCfg::default());
        use std::io::Write as _;
        let mut slave: std::fs::File = slave.into();
        slave.write_all(b"\x1b[c\x1b[>q").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), async {
            while handle.queries_answered() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        assert!(got, "pump must reach 2 answered queries");
        assert_eq!(handle.queries_answered(), 2);
        assert!(handle.bytes_drained() > 0);
    }
}
