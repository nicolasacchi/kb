//! `kb-core::sidecar_spool` — bounded-memory capture of one sidecar's text.
//!
//! The sidecar-text tail block (`sessions::render_sidecar_text_block`) can
//! only ever RENDER a HEAD slice and a TAIL slice of each agent's scrubbed
//! text (`SIDECAR_TEXT_AGENT_CAP_BYTES`, further bounded by the 8 MiB total),
//! plus the exact scrubbed byte LENGTH (which drives the budget allocation
//! and the `truncated N bytes` marker). A [`SidecarSpool`] therefore keeps
//! exactly those three things — never the whole text — so a capture over 160
//! MB of sidecars holds a few MB, not 160.
//!
//! [`spool_sidecar_file`] is the streaming producer: ONE pass over a sidecar
//! file, line by line, feeding (a) the subagent-digest parse and (b) the
//! secrets scrub ([`SecretScrubStream`]) into the spool. Every output the
//! render fn produces from a spool is byte-identical to what the old
//! read-whole → scrub-whole → truncate pipeline produced; the golden tests in
//! `sessions.rs` pin that against a verbatim copy of the old pipeline.

use std::io::{self, BufRead};
use std::path::Path;

use crate::session_scrub::SecretScrubStream;
use crate::sessions::{
    parse_subagent_lines, sidecar_head_tail_split, SubagentDigest, SIDECAR_TEXT_AGENT_CAP_BYTES,
};

/// Head bytes retained: the head share of the largest possible budget, plus
/// 4 bytes so the char-boundary snap at the cut can always inspect the byte
/// AT the cut (a UTF-8 scalar is at most 4 bytes).
fn head_retain() -> usize {
    sidecar_head_tail_split(SIDECAR_TEXT_AGENT_CAP_BYTES).0 + 4
}

/// Tail bytes retained: the tail share of the largest possible budget.
fn tail_retain() -> usize {
    sidecar_head_tail_split(SIDECAR_TEXT_AGENT_CAP_BYTES).1
}

/// One sidecar's (already scrubbed) text, reduced to what the render can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarSpool {
    /// The block's per-source label (`agent_id`, `workflow:<id>`, …).
    pub id: String,
    len: usize,
    head: Vec<u8>,
    tail: Vec<u8>,
}

#[inline]
fn is_cont(b: u8) -> bool {
    b & 0xC0 == 0x80
}

impl SidecarSpool {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            len: 0,
            head: Vec::new(),
            tail: Vec::new(),
        }
    }

    /// Spool a whole in-memory text (extras, tests, the legacy
    /// `(id, text)` render entry point). No scrubbing.
    pub fn from_text(id: impl Into<String>, text: &str) -> Self {
        let mut s = Self::new(id);
        s.push(text);
        s
    }

    /// Total byte length of the text this spool saw (the scrubbed length).
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append the next piece of (scrubbed) text.
    pub fn push(&mut self, s: &str) {
        let b = s.as_bytes();
        self.len += b.len();
        let want = head_retain().saturating_sub(self.head.len());
        if want > 0 {
            self.head.extend_from_slice(&b[..want.min(b.len())]);
        }
        let tr = tail_retain();
        self.tail.extend_from_slice(b);
        if self.tail.len() > tr * 2 {
            let cut = self.tail.len() - tr;
            self.tail.drain(..cut);
        }
    }

    /// The last `min(len, retain)` bytes of the text.
    fn logical_tail(&self) -> &[u8] {
        &self.tail[self.tail.len() - self.tail.len().min(tail_retain())..]
    }

    /// Absolute byte at `abs` (must lie in the retained head or tail).
    fn byte_at(&self, abs: usize) -> u8 {
        if abs < self.head.len() {
            return self.head[abs];
        }
        let lt = self.logical_tail();
        lt[abs - (self.len - lt.len())]
    }

    /// The text [`crate::sessions::render_sidecar_text_block`] would embed
    /// (pre-escape) for this agent under `budget` bytes: the whole text when
    /// it fits, else HEAD 60% + marker + TAIL 40%, both ends snapped to UTF-8
    /// char boundaries — identical, byte for byte, to the old
    /// `truncate_and_escape_agent_raw` minus its final escape.
    pub(crate) fn kept_for_budget(&self, budget: usize) -> String {
        let logical_tail = self.logical_tail();
        let tail_abs_start = self.len - logical_tail.len();
        if self.len <= budget {
            let mut out = Vec::with_capacity(self.len);
            if self.len <= self.head.len() {
                out.extend_from_slice(&self.head[..self.len]);
            } else {
                out.extend_from_slice(&self.head);
                out.extend_from_slice(&logical_tail[self.head.len() - tail_abs_start..]);
            }
            return String::from_utf8(out).expect("spooled text is valid UTF-8");
        }
        let (head_len, tail_len) = sidecar_head_tail_split(budget);
        let mut head_end = head_len;
        while head_end > 0 && is_cont(self.head[head_end]) {
            head_end -= 1;
        }
        let mut tail_start = self.len - tail_len;
        while tail_start < self.len && is_cont(self.byte_at(tail_start)) {
            tail_start += 1;
        }
        let tail_start = tail_start.max(head_end);
        let dropped = tail_start - head_end;
        let marker = crate::sessions::sidecar_truncation_marker(dropped);
        let mut out = Vec::with_capacity(head_end + marker.len() + (self.len - tail_start));
        out.extend_from_slice(&self.head[..head_end]);
        out.extend_from_slice(marker.as_bytes());
        out.extend_from_slice(&logical_tail[tail_start - tail_abs_start..]);
        String::from_utf8(out).expect("char-boundary-snapped slices are valid UTF-8")
    }
}

/// What [`spool_sidecar_file`] produced for one `agent-*.jsonl`.
pub struct SpooledSidecar {
    /// Digest parsed from the RAW (unscrubbed) lines — same input the old
    /// `parse_subagent_jsonl(read_to_string(..))` saw.
    pub digest: SubagentDigest,
    /// Scrubbed text spool, labelled with the digest's resolved `agent_id`.
    pub spool: SidecarSpool,
    /// Secrets redacted in this sidecar's text.
    pub redactions: u32,
}

struct LineTee<R: BufRead> {
    reader: R,
    scrub: SecretScrubStream,
    spool: SidecarSpool,
    err: Option<io::Error>,
    buf: Vec<u8>,
}

impl<R: BufRead> Iterator for LineTee<R> {
    type Item = String;
    fn next(&mut self) -> Option<String> {
        if self.err.is_some() {
            return None;
        }
        self.buf.clear();
        match self.reader.read_until(b'\n', &mut self.buf) {
            Ok(0) => None,
            Ok(_) => {
                // A UTF-8 scalar never spans a `\n`, so validating line by
                // line is equivalent to `read_to_string`'s whole-file check.
                let line = match std::str::from_utf8(&self.buf) {
                    Ok(l) => l,
                    Err(e) => {
                        self.err = Some(io::Error::new(io::ErrorKind::InvalidData, e));
                        return None;
                    }
                };
                let spool = &mut self.spool;
                self.scrub.push_line(line, &mut |chunk| spool.push(chunk));
                Some(line.to_string())
            }
            Err(e) => {
                self.err = Some(e);
                None
            }
        }
    }
}

/// Stream one sidecar file ONCE: constant memory per file (one line + at most
/// one scrub chunk + the spool). Any I/O or invalid-UTF-8 error fails the
/// whole file — the same outcome as the old `read_to_string` (the caller
/// skips the sidecar).
pub fn spool_sidecar_file(path: &Path, filename_id: &str) -> io::Result<SpooledSidecar> {
    let file = std::fs::File::open(path)?;
    let reader = io::BufReader::with_capacity(64 * 1024, file);
    let mut tee = LineTee {
        reader,
        scrub: SecretScrubStream::new(),
        spool: SidecarSpool::new(String::new()),
        err: None,
        buf: Vec::new(),
    };
    let digest = parse_subagent_lines(&mut tee, filename_id);
    // The digest parse consumes every line; drain defensively so the scrub
    // tail is always flushed even if that ever changes.
    for _ in tee.by_ref() {}
    let LineTee {
        mut scrub,
        mut spool,
        err,
        ..
    } = tee;
    if let Some(e) = err {
        return Err(e);
    }
    scrub.finish(&mut |chunk| spool.push(chunk));
    spool.id = digest.agent_id.clone();
    Ok(SpooledSidecar {
        digest,
        spool,
        redactions: scrub.redactions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_scrub::{scrub_transcript, ScrubOptions};
    use crate::sessions::{
        parse_subagent_jsonl, render_sidecar_text_block, render_sidecar_text_block_spooled,
        SIDECAR_TEXT_AGENT_CAP_BYTES,
    };

    /// The OLD pipeline for one file: read whole, parse whole, scrub whole.
    /// Returns (digest, scrubbed text, redaction count).
    fn legacy(path: &Path, filename_id: &str) -> (SubagentDigest, String, u32) {
        let raw = std::fs::read_to_string(path).unwrap();
        let digest = parse_subagent_jsonl(&raw, filename_id);
        let (scrubbed, report) = scrub_transcript(&raw, &ScrubOptions::secrets_only());
        (digest, scrubbed, report.total)
    }

    fn assert_file_matches_legacy(path: &Path) -> (String, SidecarSpool) {
        let got = spool_sidecar_file(path, "fallback").unwrap();
        let (digest, scrubbed, total) = legacy(path, "fallback");
        assert_eq!(got.digest, digest, "digest must be unchanged");
        assert_eq!(got.redactions, total, "redaction count must be unchanged");
        assert_eq!(got.spool.len(), scrubbed.len());
        let old = render_sidecar_text_block(&[(digest.agent_id.clone(), scrubbed.clone())]);
        let new = render_sidecar_text_block_spooled(std::slice::from_ref(&got.spool));
        assert_eq!(new, old, "rendered block must be byte-identical");
        (scrubbed, got.spool)
    }

    fn jsonl_fixture(lines: usize) -> String {
        let mut s = String::new();
        s.push_str("{\"type\":\"user\",\"agentId\":\"agent-xyz\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n");
        for i in 0..lines {
            s.push_str(&format!(
                "{{\"type\":\"assistant\",\"n\":{i},\"message\":{{\"role\":\"assistant\",\"usage\":{{\"input_tokens\":3,\"output_tokens\":4}},\"content\":[{{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{{\"file_path\":\"/w/f{}.rs\"}}}}]}},\"pad\":\"é&<> lorem ipsum dolor sit amet consectetur\"}}\n",
                i % 40
            ));
            if i % 997 == 0 {
                s.push_str(
                    "{\"note\":\"api_key: abcdEFGH12345678 and sk-ABCDEFGHIJKLMNOPQRSTUVWX\"}\n",
                );
            }
            if i % 1500 == 0 {
                // A newline-spanning bearer match: must stay in ONE chunk.
                s.push_str("Authorization: Bearer\n");
                s.push_str("abcdefghijklmnopqrstuvwxyz0123456789\n");
            }
        }
        s
    }

    #[test]
    fn streamed_sidecar_file_equals_the_legacy_read_whole_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-abc.jsonl");
        // > 2 scrub chunks and > the 2 MiB per-agent cap, multibyte + entities.
        let text = jsonl_fixture(14_000);
        assert!(text.len() > 2 * SIDECAR_TEXT_AGENT_CAP_BYTES);
        std::fs::write(&path, &text).unwrap();
        let (scrubbed, spool) = assert_file_matches_legacy(&path);
        assert!(scrubbed.contains("[redacted:api-key]"));
        assert!(!scrubbed.contains("ABCDEFGHIJKLMNOPQRSTUVWX"));
        assert_eq!(spool.id, "agent-xyz", "label is the resolved agentId");
        // A small (under-budget) file round-trips whole too.
        let small = dir.path().join("agent-small.jsonl");
        std::fs::write(&small, jsonl_fixture(20)).unwrap();
        assert_file_matches_legacy(&small);
        // And an empty / newline-less / non-JSON file.
        let odd = dir.path().join("agent-odd.jsonl");
        std::fs::write(&odd, "no newline at all, token=abcdefgh12345678").unwrap();
        assert_file_matches_legacy(&odd);
        let empty = dir.path().join("agent-empty.jsonl");
        std::fs::write(&empty, "").unwrap();
        assert_file_matches_legacy(&empty);
    }

    /// SECURITY — a secret straddling the HEAD cut or the TAIL cut must not
    /// survive as a fragment. The scrub runs on the full (chunk-safe) stream
    /// BEFORE the head/tail slices are taken, so the slices are cut from
    /// already-redacted text; this pins that no raw fragment of the secret
    /// reaches the rendered block, and that the output still equals the old
    /// pipeline byte for byte.
    #[test]
    fn a_secret_straddling_the_cut_leaves_no_fragment() {
        let secret = "sk-QWERTYUIOPASDFGHJKLZXCVBNM1234567890";
        let cap = SIDECAR_TEXT_AGENT_CAP_BYTES;
        let (head, tail) = sidecar_head_tail_split(cap);
        let dir = tempfile::tempdir().unwrap();

        // Head straddle: the secret starts 10 bytes before the head cut.
        let prefix_len = head - 10 - "{\"k\":\"".len();
        let mut text = format!("{{\"p\":\"{}\"}}\n", "a".repeat(prefix_len - 8));
        text.push_str(&format!("{{\"k\":\"{secret}\"}}\n"));
        // Push well past the cap so the cut is really taken.
        text.push_str(&jsonl_fixture(12_000));
        let p1 = dir.path().join("agent-head.jsonl");
        std::fs::write(&p1, &text).unwrap();
        let (scrubbed1, spool1) = assert_file_matches_legacy(&p1);
        let marker = "[redacted:api-key]";
        let m1 = scrubbed1.find(marker).unwrap();
        assert!(
            m1 < head && head < m1 + marker.len(),
            "fixture must put the head cut INSIDE the redaction ({m1} vs {head})"
        );

        // Tail straddle: the secret line ends ~12 bytes before the kept tail.
        let mut text2 = jsonl_fixture(12_000);
        text2.push_str(&format!("{{\"k\":\"{secret}\"}}\n"));
        text2.push_str(&format!("{{\"p\":\"{}\"}}\n", "b".repeat(tail - 12 - 8)));
        let p2 = dir.path().join("agent-tail.jsonl");
        std::fs::write(&p2, &text2).unwrap();
        let (scrubbed2, spool2) = assert_file_matches_legacy(&p2);
        let m2 = scrubbed2.rfind(marker).unwrap();
        let tail_cut = scrubbed2.len() - tail;
        assert!(
            m2 < tail_cut && tail_cut < m2 + marker.len(),
            "fixture must put the tail cut INSIDE the redaction ({m2} vs {tail_cut})"
        );

        for spool in [spool1, spool2] {
            let block = render_sidecar_text_block_spooled(&[spool]).unwrap();
            for w in secret.as_bytes().windows(8) {
                let frag = std::str::from_utf8(w).unwrap();
                assert!(!block.contains(frag), "fragment {frag:?} leaked");
            }
        }
    }

    #[test]
    fn invalid_utf8_sidecar_fails_the_whole_file_like_read_to_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-bad.jsonl");
        let mut bytes = b"{\"a\":1}\n".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        std::fs::write(&path, bytes).unwrap();
        assert!(std::fs::read_to_string(&path).is_err());
        assert!(spool_sidecar_file(&path, "bad").is_err());
    }

    /// Memory bound: after spooling many MB the spool retains only the
    /// head+tail windows, never the text.
    #[test]
    fn spool_retention_is_bounded_regardless_of_input_size() {
        let mut spool = SidecarSpool::new("big");
        let chunk = "x".repeat(1 << 20);
        for _ in 0..64 {
            spool.push(&chunk);
        }
        assert_eq!(spool.len(), 64 << 20);
        assert!(spool.head.len() <= head_retain());
        assert!(spool.tail.len() <= tail_retain() * 2);
    }
}
