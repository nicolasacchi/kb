//! Local-tty plumbing for the mirror: raw mode, terminal size,
//! exit-restoration bytes, and the stdin terminal-reply filter with
//! the detach key.
//!
//! The mirror never answers terminal queries — the hidden tmux client
//! owns that — so replies the operator's terminal might leak into
//! stdin (its answers to probes the harness or tmux sent) are dropped
//! before they can reach the hidden client.

use std::os::unix::io::RawFd;

/// The detach lead byte: `Ctrl-]`. The sequence is `Ctrl-]` then `.`.
pub const DETACH_LEAD: u8 = 0x1D;

/// The bytes printed on exit (whatever the exit path): SGR reset,
/// sync-output off, cursor visible, bracketed paste off, focus-events
/// off, plus a newline. The mirror sets no DEC modes itself; the
/// streamed content may have enabled any of them.
pub const RESTORE_BYTES: &[u8] = b"\x1b[0m\x1b[?2026l\x1b[?25h\x1b[?2004l\x1b[?1004l\r\n";

/// Raw-mode guard for the operator's tty. The pre-existing termios is
/// restored on drop; the mirror never leaves raw mode set.
///
/// The raw mode is `cfmakeraw` (ICANON/ECHO/ISIG off, so Ctrl-C
/// arrives as a byte and goes to the harness with everything else).
pub struct RawGuard {
    fd: RawFd,
    saved: Option<libc::termios>,
}

impl RawGuard {
    /// Enter raw mode on `fd`. If `fd` is not a tty (or termios calls
    /// fail), this is a no-op — the mirror still runs (so an
    /// offline/`--watch` session never fails opening on a pipe).
    pub fn enter(fd: RawFd) -> Self {
        let mut saved = None;
        unsafe {
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut orig) == 0 {
                let mut raw: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut raw) == 0 {
                    libc::cfmakeraw(&mut raw);
                    if libc::tcsetattr(fd, libc::TCSANOW, &raw) == 0 {
                        saved = Some(orig);
                    }
                }
            }
        }
        Self { fd, saved }
    }

    /// The terminal size (cols, rows) of `fd`, or `None` when it is not
    /// a tty or the size is bogus.
    pub fn size(fd: RawFd) -> Option<(u16, u16)> {
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) != 0 || ws.ws_col == 0 || ws.ws_row == 0 {
                return None;
            }
            Some((ws.ws_col, ws.ws_row))
        }
    }

    /// Restore the original termios (idempotent; drop also restores).
    pub fn restore(&mut self) {
        if let Some(orig) = self.saved.take() {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &orig);
            }
        }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// One stdin read chunk's behaviour after filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinDecision {
    /// Forward these bytes to the hidden client (after the reply filter).
    Forward(Vec<u8>),
    /// The detach key: leave the mirror (restoring the tty).
    Detach,
}

const ESC: u8 = 0x1B;
const BEL: u8 = b'\x07';

/// Drop terminal replies from stdin, holding the detach key state and
/// any incomplete trailing escape sequence across chunks.
///
/// Dropped: DA1 replies (`ESC[?...c`), CPR (`ESC[r;cR` — except the
/// `r == 1 && 2 <= c <= 8` F3-with-modifiers shape, which is kept),
/// DECRPM (`ESC[?...;...$y`), and OSC replies (`ESC]...BEL` /
/// `ESC]...ESC\`).
/// Kept: everything else, including arrows, bare Esc, `ESC[13;2u`
/// (Shift+Enter), `ESC[1;2R` (Shift+F3), and bracketed-paste markers.
/// Incomplete trailing sequences are held verbatim for the next chunk
/// (a partial reply must never reach the hidden client as bytes).
pub struct InputFilter {
    pending: Vec<u8>,
    /// When the held (incomplete) sequence started arriving. Only
    /// meaningful while `pending` is non-empty.
    pending_since: Option<std::time::Instant>,
    pending_lead: bool,
}

impl InputFilter {
    /// Create a fresh filter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            pending_since: None,
            pending_lead: false,
        }
    }

    /// Filter one stdin read chunk: detach first (a `Ctrl-]` + `.`
    /// pair detaches; `Ctrl-]` followed by anything else forwards
    /// both bytes), then the reply filter.
    pub fn decide(&mut self, bytes: &[u8]) -> StdinDecision {
        let mut all = std::mem::take(&mut self.pending);
        self.pending_since = None;
        all.extend_from_slice(bytes);
        let mut out = Vec::with_capacity(all.len());
        let mut i = 0;
        while i < all.len() {
            let b = all[i];
            if self.pending_lead {
                self.pending_lead = false;
                if b == b'.' {
                    return StdinDecision::Detach;
                }
                out.push(DETACH_LEAD);
            }
            if b != ESC {
                if b == DETACH_LEAD {
                    self.pending_lead = true;
                } else {
                    out.push(b);
                }
                i += 1;
                continue;
            }
            // ESC [ / ESC ] / ESC <other>
            if i + 1 >= all.len() {
                self.pending.extend_from_slice(&all[i..]);
                self.pending_since = Some(std::time::Instant::now());
                return StdinDecision::Forward(out);
            }
            match all[i + 1] {
                b'[' => {
                    let mut j = i + 2;
                    while j < all.len() && !(0x40..=0x7E).contains(&all[j]) {
                        j += 1;
                    }
                    if j >= all.len() {
                        self.pending.extend_from_slice(&all[i..]);
                        self.pending_since = Some(std::time::Instant::now());
                        return StdinDecision::Forward(out);
                    }
                    let params = &all[i + 2..j];
                    if is_terminal_reply_csi(params, all[j]) {
                        i = j + 1;
                        continue;
                    }
                    out.extend_from_slice(&all[i..=j]);
                    i = j + 1;
                    continue;
                }
                b']' => {
                    let mut j = i + 2;
                    let mut end: Option<usize> = None;
                    while j < all.len() {
                        if all[j] == BEL {
                            end = Some(j + 1);
                            break;
                        }
                        if all[j] == ESC && j + 1 < all.len() && all[j + 1] == b'\\' {
                            end = Some(j + 2);
                            break;
                        }
                        j += 1;
                    }
                    if let Some(stop) = end {
                        i = stop;
                        continue;
                    }
                    self.pending.extend_from_slice(&all[i..]);
                    self.pending_since = Some(std::time::Instant::now());
                    return StdinDecision::Forward(out);
                }
                _ => {
                    out.push(ESC);
                    out.push(all[i + 1]);
                    i += 2;
                    continue;
                }
            }
        }
        StdinDecision::Forward(out)
    }

    /// Whether an incomplete escape sequence is currently held.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Flush a held bare `ESC` once it has been idle for at least
    /// `idle`. A lone Esc is a valid key and must not wait forever for
    /// bytes that never arrive; a longer incomplete sequence still
    /// waits (it may yet complete). Returns the flushed bytes (forward
    /// these to the hidden client), or `None` when nothing expired.
    pub fn take_expired_esc(&mut self, idle: std::time::Duration) -> Option<Vec<u8>> {
        let is_bare_esc = self.pending == [ESC];
        let expired = self
            .pending_since
            .map(|t| t.elapsed() >= idle)
            .unwrap_or(false);
        if is_bare_esc && expired {
            self.pending.clear();
            self.pending_since = None;
            Some(vec![ESC])
        } else {
            None
        }
    }
}

impl Default for InputFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a CSI is a terminal reply to drop.
fn is_terminal_reply_csi(params: &[u8], finalb: u8) -> bool {
    match finalb {
        // DA1: `ESC[?...c`.
        b'c' => params.first() == Some(&b'?'),
        // CPR: `ESC[r;cR` — except r=1, 2<=c<=8 (Shift/Ctrl/Alt+F3).
        b'R' => {
            let nums = parse_nums(params);
            nums.len() == 2 && !(nums[0] == 1 && (2..=8).contains(&nums[1]))
        }
        // DECRPM: `ESC[?..;..$y`.
        b'y' => params.first() == Some(&b'?') && params.last() == Some(&b'$'),
        _ => false,
    }
}

fn parse_nums(params: &[u8]) -> Vec<u64> {
    params
        .split(|b| *b == b';')
        .map(|t| {
            let mut v: u64 = 0;
            for &c in t {
                if c.is_ascii_digit() {
                    v = v * 10 + (c - b'0') as u64;
                }
            }
            v
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(input: &[u8]) -> Vec<u8> {
        match InputFilter::new().decide(input) {
            StdinDecision::Forward(v) => v,
            StdinDecision::Detach => Vec::new(),
        }
    }

    #[test]
    fn reply_filter_drops_terminal_replies_keeps_keys() {
        // Dropped: DA1 replies (with and without extra params).
        assert!(filtered(b"\x1b[?62;22c").is_empty(), "DA1 dropped");
        assert!(filtered(b"\x1b[?62c").is_empty(), "DA1 bare dropped");
        // Dropped: CPR (with and without modifier byte).
        assert!(filtered(b"\x1b[12;40R").is_empty(), "CPR dropped");
        assert!(filtered(b"\x1b[24;80R").is_empty(), "CPR dropped");
        // Dropped: OSC replies (BEL- and ST-terminated).
        assert!(
            filtered(b"\x1b]11;rgb:0000/0000/0000\x07").is_empty(),
            "OSC BEL"
        );
        assert!(
            filtered(b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\").is_empty(),
            "OSC ST"
        );
        // Dropped: DECRPM reports.
        assert!(filtered(b"\x1b[?25;1$y").is_empty(), "DECRPM dropped");
        assert!(filtered(b"\x1b[?1049;2$y").is_empty(), "DECRPM dropped");
        // Kept: arrows.
        assert_eq!(filtered(b"\x1b[A"), b"\x1b[A");
        assert_eq!(filtered(b"\x1b[B"), b"\x1b[B");
        assert_eq!(filtered(b"\x1b[D"), b"\x1b[D");
        // Kept: bare Esc is held (it may still become a sequence), then
        // flushed once idle: asserted below in
        // `lone_esc_flushes_after_idle`.
        // Kept: Shift+Enter kitty form.
        assert_eq!(filtered(b"\x1b[13;2u"), b"\x1b[13;2u");
        // Kept: the F3 exception (r=1, 2<=c<=8).
        assert_eq!(filtered(b"\x1b[1;2R"), b"\x1b[1;2R");
        assert_eq!(filtered(b"\x1b[1;5R"), b"\x1b[1;5R");
        assert_eq!(filtered(b"\x1b[1;8R"), b"\x1b[1;8R");
        // Dropped: CPR-shaped but r=1, c=9 (not F3).
        assert!(filtered(b"\x1b[1;9R").is_empty(), "r=1 c=9 is CPR");
        // Kept: plain text and paste markers.
        assert_eq!(filtered(b"hello"), b"hello");
        assert_eq!(
            filtered(b"\x1b[200~paste\x1b[201~"),
            b"\x1b[200~paste\x1b[201~"
        );
        // Mixed: text, reply, key.
        assert_eq!(filtered(b"a\x1b[?62;22cb\x1b[A"), b"ab\x1b[A");
        // A reply split across chunks is still dropped.
        let mut f = InputFilter::new();
        assert_eq!(f.decide(b"\x1b"), StdinDecision::Forward(Vec::new()));
        assert_eq!(f.decide(b"[?62;22c"), StdinDecision::Forward(Vec::new()));
        // A key split across chunks is reassembled verbatim.
        let mut f = InputFilter::new();
        assert_eq!(f.decide(b"\x1b"), StdinDecision::Forward(Vec::new()));
        assert_eq!(f.decide(b"[A"), StdinDecision::Forward(b"\x1b[A".to_vec()));
    }

    #[test]
    fn lone_esc_flushes_after_idle() {
        use std::time::Duration;
        // A lone Esc is held immediately (nothing forwarded yet), and
        // does NOT flush before the idle threshold.
        let mut f = InputFilter::new();
        assert_eq!(f.decide(b"\x1b"), StdinDecision::Forward(Vec::new()));
        assert!(f.has_pending());
        assert_eq!(f.take_expired_esc(Duration::from_secs(10)), None);
        assert!(f.has_pending());
        // Once idle past the threshold it flushes as the Esc key.
        assert_eq!(
            f.take_expired_esc(Duration::ZERO),
            Some(vec![0x1B]),
            "lone Esc forwards after idle"
        );
        assert!(!f.has_pending());
        // A longer incomplete sequence (CSI prefix) never idle-flushes.
        let mut f = InputFilter::new();
        assert_eq!(f.decide(b"\x1b["), StdinDecision::Forward(Vec::new()));
        assert_eq!(f.take_expired_esc(Duration::ZERO), None, "CSI waits");
        assert!(f.has_pending());
        // Nothing pending: no flush.
        let mut f = InputFilter::new();
        assert_eq!(f.decide(b"hi"), StdinDecision::Forward(b"hi".to_vec()));
        assert_eq!(f.take_expired_esc(Duration::ZERO), None);
    }

    #[test]
    fn stdin_detach_parsing() {
        // Ctrl-] then '.' -> detach.
        let mut p = InputFilter::new();
        assert_eq!(p.decide(b"\x1d."), StdinDecision::Detach);

        // Ctrl-] then 'x' -> both bytes forwarded.
        let mut p = InputFilter::new();
        assert_eq!(
            p.decide(b"\x1dx"),
            StdinDecision::Forward(b"\x1dx".to_vec())
        );

        // Ctrl-] split across chunks.
        let mut p = InputFilter::new();
        assert_eq!(p.decide(b"hi\x1d"), StdinDecision::Forward(b"hi".to_vec()));
        assert_eq!(p.decide(b"."), StdinDecision::Detach);

        // Ctrl-] split, followed by a non-dot byte in the next chunk.
        let mut p = InputFilter::new();
        assert_eq!(p.decide(b"hi\x1d"), StdinDecision::Forward(b"hi".to_vec()));
        assert_eq!(p.decide(b"x"), StdinDecision::Forward(b"\x1dx".to_vec()));

        // The detach sequence after a kept key.
        let mut p = InputFilter::new();
        assert_eq!(p.decide(b"\x1b[A\x1d."), StdinDecision::Detach);
    }

    #[test]
    fn restore_bytes_are_the_documented_sequence() {
        assert_eq!(
            RESTORE_BYTES,
            b"\x1b[0m\x1b[?2026l\x1b[?25h\x1b[?2004l\x1b[?1004l\r\n"
        );
    }
}
