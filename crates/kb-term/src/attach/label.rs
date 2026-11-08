//! Stateful, chunk-safe kitty-placeholder label rewriter.
//!
//! Placeholder rows (from omp's kitty protocol: `ESC[38;2;R;G;Bm` +
//! `ESC[58:2::R:G:Bm` + cells of `U+10EEEE` each followed by a row
//! diacritic and a column diacritic, from
//! `crates/kb-term/fixtures/synth_omp_kitty_stream.py`) cannot cross
//! mosh and mean nothing in zellij's native scrollback, so the mirror
//! rewrites each picture as a single dim label `[image WxH, on web]`
//! and every following row of the picture as an empty cleared row.
//! Row counts and LF counts are preserved exactly.
//!
//! Buffering model: the current row (since the last LF) is held from
//! its first byte — the label folds in any text preceding the
//! placeholder, and a trailing `ESC[2K` after visible prefix text
//! would erase it otherwise — and emitted at its LF. A row is a
//! placeholder row iff it contains `U+10EEEE`. The hold is capped at
//! 64 KiB and flushes raw beyond the cap. All state survives chunk
//! boundaries.

use std::collections::HashMap;

use super::tables::diacritic_index;
use super::width::fit;

/// The kitty placeholder character.
pub const PLACEHOLDER: [u8; 4] = [0xF4, 0x8E, 0xBB, 0xAE];
/// Emitted after every rewritten row (clears the 58 underline colour
/// the placeholder rows set).
pub const SGR_RESET: &[u8] = b"\x1b[39;59m";
/// Hold cap per row.
const HOLD_CAP: usize = 64 * 1024;

/// The id colour of a picture: the latest `38;2;R;G;B` (truecolor —
/// packed into the 24-bit image id by the kitty placeholder protocol)
/// or `38;5;n` (256-colour) SGR in effect, carried across rows
/// (sticky: resets never clear it, so a row tail resetting the colour
/// does not split the picture).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fg {
    Rgb(u8, u8, u8),
    Indexed(u8),
}

impl Fg {
    /// The image id the colour names: truecolor packs into the 24-bit
    /// id (the generator's `(R<<16)|(G<<8)|B`), the 256-colour index
    /// is the id.
    fn image_id(self) -> u32 {
        match self {
            Fg::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
            Fg::Indexed(n) => n as u32,
        }
    }
}

fn is_sgr_param_byte(b: u8) -> bool {
    (0x30..=0x39).contains(&b) || b == 0x3B || b == 0x3A
}

fn atoi(b: &[u8]) -> u8 {
    let mut v: u32 = 0;
    let mut any = false;
    for &c in b {
        if c.is_ascii_digit() {
            v = v * 10 + (c - b'0') as u32;
            any = true;
        }
    }
    if any {
        v.min(u32::from(u8::MAX)) as u8
    } else {
        0
    }
}

/// Parse an SGR parameter list and return the `38;2` / `38;5` it sets
/// (the last one, if it sets several). Colon-separated underline
/// colours (`58:2:...`) never match.
fn sgr_foreground(params: &[u8]) -> Option<Fg> {
    let mut found = None;
    let mut toks = params.split(|b| *b == b';');
    while let Some(tok) = toks.next() {
        if tok == b"38" {
            match toks.next() {
                Some(b"2") => {
                    let mut chans = [0u8; 3];
                    for slot in &mut chans {
                        *slot = atoi(toks.next().unwrap_or(b"0"));
                    }
                    found = Some(Fg::Rgb(chans[0], chans[1], chans[2]));
                }
                Some(b"5") => found = Some(Fg::Indexed(atoi(toks.next().unwrap_or(b"0")))),
                _ => {}
            }
        }
    }
    found
}

/// Strip escape sequences from `bytes`, returning the visible text.
/// Handles CSI (`ESC[...letter`), OSC (`ESC]...BEL|ESC\`), string
/// sequences (`ESC P|X|^|_ ... ESC\`) and single-char `ESC<ch>`.
fn strip_escapes(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next == b'[' {
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7E).contains(&bytes[j]) {
                    j += 1;
                }
                i = j.saturating_add(1).min(bytes.len() + 1);
                continue;
            }
            if next == b']' {
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        break;
                    }
                    if bytes[j] == 0x1B && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                        break;
                    }
                    j += 1;
                }
                i = if j < bytes.len() && bytes[j] == 0x07 {
                    j + 1
                } else {
                    (j + 2).min(bytes.len())
                };
                continue;
            }
            if next == b'P' || next == b'X' || next == b'^' || next == b'_' {
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x1B && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                        break;
                    }
                    j += 1;
                }
                i = (j + 2).min(bytes.len());
                continue;
            }
            i += 2;
            continue;
        }
        let rest = &bytes[i..];
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                if good > 0 {
                    out.push_str(std::str::from_utf8(&rest[..good]).unwrap_or(""));
                    i += good;
                } else {
                    i += 1;
                }
            }
        }
    }
    out
}

fn find_placeholder(content: &[u8]) -> Option<usize> {
    content
        .windows(PLACEHOLDER.len())
        .position(|w| w == PLACEHOLDER)
}

/// The row diacritic codepoint of the first placeholder cell (the char
/// immediately after the first `U+10EEEE`).
fn first_comment_diacs(content: &[u8], at: usize) -> Option<u32> {
    let tail = &content[at + PLACEHOLDER.len()..];
    let s = std::str::from_utf8(tail).ok()?;
    s.chars().next().map(|c| c as u32)
}

/// Result of scanning one held row for placeholder characteristics.
struct RowScan {
    /// Position (in content) of the first `U+10EEEE`.
    placeholder_at: Option<usize>,
    /// Row diacritic codepoint of the first placeholder cell, if the
    /// row decodes as UTF-8 there.
    row_diacritic: Option<u32>,
    /// The id colour in effect at the first placeholder.
    fg_at_placeholder: Option<Fg>,
    /// The id colour after the whole row (for carrying into the next).
    fg_after_row: Option<Fg>,
    /// Visible text before the first placeholder (escape sequences
    /// stripped; only meaningful when a placeholder is present).
    visible_prefix: String,
}

fn scan_row(content: &[u8], fg_in: Option<Fg>) -> RowScan {
    let placeholder_at = find_placeholder(content);
    let visible_prefix = match placeholder_at {
        Some(at) => strip_escapes(&content[..at]),
        None => String::new(),
    };
    // Walk every CSI in the row to find 38;2/38;5. The colour before
    // the placeholder position is "in effect" there; the colour after
    // the whole row carries into the next row.
    let pre = placeholder_at.unwrap_or(usize::MAX);
    let mut fg = fg_in;
    let mut fg_at = fg_in;
    let mut i = 0;
    while i + 2 <= content.len() {
        if content[i] == 0x1B && content[i + 1] == b'[' {
            let mut j = i + 2;
            let mut params = Vec::new();
            while j < content.len() && is_sgr_param_byte(content[j]) {
                params.push(content[j]);
                j += 1;
            }
            if j < content.len() && content[j] == b'm' {
                if let Some(new_fg) = sgr_foreground(&params) {
                    fg = Some(new_fg);
                    if i < pre {
                        fg_at = fg;
                    }
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let row_diacritic = placeholder_at.and_then(|at| first_comment_diacs(content, at));
    RowScan {
        placeholder_at,
        row_diacritic,
        fg_at_placeholder: fg_at,
        fg_after_row: fg,
        visible_prefix,
    }
}

/// The stateful label rewriter.
pub struct LabelRewriter {
    held: Vec<u8>,
    flushing_raw: bool,
    cols: u16,
    dim: bool,
    /// Latest id colour in effect (sticky across rows).
    last_fg: Option<Fg>,
    /// The id colour of the picture currently open, if any.
    open_picture_fg: Option<Fg>,
    /// Pixel dims of defined images by id (`w == 0 || h == 0` = stub).
    defs: HashMap<u32, (u32, u32)>,
}

impl LabelRewriter {
    /// Create a rewriter for a `cols`-wide grid. `dim` renders labels
    /// dim (`ESC[2m`); `--no-labels-dim` leaves them plain.
    #[must_use]
    pub fn new(cols: u16, dim: bool) -> Self {
        Self {
            held: Vec::new(),
            flushing_raw: false,
            cols,
            dim,
            last_fg: None,
            open_picture_fg: None,
            defs: HashMap::new(),
        }
    }

    /// Update the current grid width (on `Profile` / `Resize`).
    pub fn set_cols(&mut self, cols: u16) {
        self.cols = cols;
    }

    /// Record an `Imagedef` (id -> pixel dims; a stub arrives as
    /// `w == 0 || h == 0`).
    pub fn add_image(&mut self, id: u32, w: u32, h: u32) {
        self.defs.insert(id, (w, h));
    }

    /// Drop all image definitions (`Gfxreset`).
    pub fn clear_images(&mut self) {
        self.defs.clear();
    }

    /// Reset stream state (on a fresh join / new epoch).
    pub fn reset(&mut self) {
        self.held.clear();
        self.flushing_raw = false;
        self.last_fg = None;
        self.open_picture_fg = None;
        self.defs.clear();
    }

    /// Flush any held row raw (end of stream).
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.held);
        self.held.clear();
        self.flushing_raw = false;
    }

    /// Feed bytes; append the rewritten output for every completed row.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.flushing_raw {
                // Stream raw until the row's LF.
                match rest.iter().position(|&b| b == b'\n') {
                    Some(p) => {
                        out.extend_from_slice(&rest[..=p]);
                        rest = &rest[p + 1..];
                        self.flushing_raw = false;
                    }
                    None => {
                        out.extend_from_slice(rest);
                        rest = &rest[rest.len()..];
                    }
                }
                continue;
            }
            match rest.iter().position(|&b| b == b'\n') {
                Some(p) => {
                    let seg = &rest[..=p];
                    if self.held.len() + seg.len() > HOLD_CAP {
                        out.extend_from_slice(&self.held);
                        out.extend_from_slice(seg);
                        self.held.clear();
                    } else {
                        self.held.extend_from_slice(seg);
                        let row = std::mem::take(&mut self.held);
                        self.emit_row(&row, out);
                    }
                    rest = &rest[p + 1..];
                }
                None => {
                    if self.held.len() + rest.len() > HOLD_CAP {
                        out.extend_from_slice(&self.held);
                        out.extend_from_slice(rest);
                        self.held.clear();
                        self.flushing_raw = true;
                    } else {
                        self.held.extend_from_slice(rest);
                    }
                    rest = &rest[rest.len()..];
                }
            }
        }
    }

    fn emit_row(&mut self, row: &[u8], out: &mut Vec<u8>) {
        // Split off the terminator (the CR run immediately before LF).
        let nl = row.len() - 1; // `row` always ends with b'\n'
        let mut term_start = nl;
        while term_start > 0 && row[term_start - 1] == b'\r' {
            term_start -= 1;
        }
        let content = &row[..term_start];
        let terminator = &row[term_start..];
        let scan = scan_row(content, self.last_fg);
        self.last_fg = scan.fg_after_row;
        if let Some(at) = scan.placeholder_at {
            let _ = at;
            self.emit_placeholder_row(content, terminator, scan, out);
        } else {
            out.extend_from_slice(content);
            out.extend_from_slice(terminator);
        }
    }

    fn emit_placeholder_row(
        &mut self,
        content: &[u8],
        terminator: &[u8],
        scan: RowScan,
        out: &mut Vec<u8>,
    ) {
        // Whether this row starts a new picture: a row diacritic index
        // of 0 starts a new picture even when the colour is equal.
        let starts_new = match (
            scan.row_diacritic.and_then(diacritic_index),
            scan.fg_at_placeholder,
        ) {
            (Some(0), _) => true,
            (_, None) => true,
            (_, fg) => fg != self.open_picture_fg,
        };
        if starts_new {
            let label = match scan
                .fg_at_placeholder
                .and_then(|fg| self.defs.get(&fg.image_id()))
            {
                Some((w, h)) if *w > 0 && *h > 0 => format!("[image {w}x{h}, on web]"),
                _ => "[image, on web]".to_string(),
            };
            let total = format!("{}{}", scan.visible_prefix, label);
            let fitted = fit(&total, self.cols);
            out.extend_from_slice(b"\x1b[2K");
            if self.dim {
                out.extend_from_slice(b"\x1b[2m");
            }
            out.extend_from_slice(fitted.as_bytes());
            if self.dim {
                out.extend_from_slice(b"\x1b[0m");
            }
            out.extend_from_slice(SGR_RESET);
            self.open_picture_fg = scan.fg_at_placeholder;
        } else {
            out.extend_from_slice(b"\x1b[2K");
            out.extend_from_slice(SGR_RESET);
        }
        let _ = content;
        out.extend_from_slice(terminator);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- fixture builders -------------------------------------------------

    /// One placeholder cell (U+10EEEE + row + col diacritics), following
    /// `synth_omp_kitty_stream.py`: the row diacritic is `DIAC[r]`,
    /// the column diacritic is `DIAC[c]`.
    fn cell(row_idx: usize, col_idx: usize) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&PLACEHOLDER);
        let mut buf = [0u8; 4];
        let rd = super::super::tables::DIACRITICS[row_idx % 297];
        v.extend_from_slice(char::from_u32(rd).unwrap().encode_utf8(&mut buf).as_bytes());
        let cd = super::super::tables::DIACRITICS[col_idx % 297];
        v.extend_from_slice(char::from_u32(cd).unwrap().encode_utf8(&mut buf).as_bytes());
        v
    }

    /// An omp placeholder row: SGR fg 38;2, SGR 58:2, `cols` placeholder
    /// cells, SGR 39;59, terminator.
    fn picture_row(rgb: (u8, u8, u8), row_idx: u8, cols: usize, terminator: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2).as_bytes());
        v.extend_from_slice(format!("\x1b[58:2::{}:{}:{}m", rgb.0, rgb.1, rgb.2).as_bytes());
        for c in 0..cols {
            v.extend_from_slice(&cell(row_idx as usize, c));
        }
        v.extend_from_slice(b"\x1b[39;59m");
        v.extend_from_slice(terminator);
        v
    }

    fn feed_all(rw: &mut LabelRewriter, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        rw.feed(input, &mut out);
        out
    }

    fn count_occurrences(out: &[u8], needle: &[u8]) -> usize {
        out.windows(needle.len()).filter(|w| *w == needle).count()
    }

    #[test]
    fn label_rewrites_picture_rows_once_and_keeps_row_count() {
        let mut rw = LabelRewriter::new(201, true);
        rw.add_image(0x66CF38, 360, 152);
        // One 3-row picture, text lines around it.
        let mut input = Vec::new();
        input.extend_from_slice(b"text one\r\n");
        input.extend_from_slice(b"text two\r\n");
        input.extend_from_slice(&picture_row((0x66, 0xCF, 0x38), 0, 5, b"\r\r\n"));
        input.extend_from_slice(&picture_row((0x66, 0xCF, 0x38), 1, 5, b"\r\r\n"));
        input.extend_from_slice(&picture_row((0x66, 0xCF, 0x38), 2, 5, b"\r\r\n"));
        input.extend_from_slice(b"text three\r\n");

        let out = feed_all(&mut rw, &input);
        assert!(
            out.windows(PLACEHOLDER.len()).all(|w| w != PLACEHOLDER),
            "no U+10EEEE may survive"
        );
        assert_eq!(
            count_occurrences(&out, b"[image 360x152, on web]"),
            1,
            "the label is emitted once"
        );
        assert_eq!(
            input.iter().filter(|&&b| b == b'\n').count(),
            6,
            "6 input rows"
        );
        assert_eq!(
            out.iter().filter(|&&b| b == b'\n').count(),
            6,
            "row count is preserved"
        );
        assert!(out.windows(8).any(|w| w == b"text one"));
        assert!(out.windows(10).any(|w| w == b"text three"));
        assert!(!out.windows(4).any(|w| w == b"\x1b[58"), "no ESC[58");
        assert_eq!(
            count_occurrences(&out, SGR_RESET),
            3,
            "one ESC[39;59m per rewritten row"
        );
    }

    #[test]
    fn label_adjacent_same_colour_pictures_split_on_row_diacritic_zero() {
        let mut rw = LabelRewriter::new(201, true);
        rw.add_image(0x010203, 100, 50);
        // Picture 1: rows with diacritics 0,1. Picture 2: rows 0,1 again
        // with the SAME colour: the row-0 diacritic starts a new
        // picture even though the colour is equal.
        let mut input = Vec::new();
        input.extend_from_slice(&picture_row((1, 2, 3), 0, 3, b"\r\n"));
        input.extend_from_slice(&picture_row((1, 2, 3), 1, 3, b"\r\n"));
        input.extend_from_slice(&picture_row((1, 2, 3), 0, 3, b"\r\n"));
        input.extend_from_slice(&picture_row((1, 2, 3), 1, 3, b"\r\n"));
        // A row-0 placeholder with NO fg SGR (only 58): the colour is
        // carried across rows, so it is the same colour again — and
        // the diacritic index of 0 still starts a new picture.
        let mut third = Vec::new();
        third.extend_from_slice(b"\x1b[58:2::1:2:3m");
        for c in 0..3 {
            third.extend_from_slice(&cell(0, c));
        }
        third.extend_from_slice(b"\x1b[39;59m\r\n");
        input.extend_from_slice(&third);

        let out = feed_all(&mut rw, &input);
        assert_eq!(
            count_occurrences(&out, b"[image 100x50, on web]"),
            3,
            "three picture starts: rows 0, 0 again (same colour), and the SGR-less row 0"
        );
        assert_eq!(out.iter().filter(|&&b| b == b'\n').count(), 5);
    }

    #[test]
    fn label_is_width_safe_with_wide_chars() {
        // Wide chars: the visible label content stays <= cols-1 cells.
        let mut rw = LabelRewriter::new(201, true);
        let input = picture_row((9, 9, 9), 0, 60, b"\r\n");
        let out = feed_all(&mut rw, &input);
        let text = strip_escapes(&out);
        assert!(
            super::super::width::str_width(&text) <= 200,
            "visible width fits cols-1: {text:?}"
        );

        // cols=20 with WIDE prefix text: label + prefix truncated to 19 cells.
        let mut rw = LabelRewriter::new(20, true);
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[38;2;1;1;1m");
        input.extend_from_slice("日本語あいうえお".as_bytes());
        input.extend_from_slice(b"\x1b[58:2::1:1:1m");
        input.extend_from_slice(&cell(0, 0));
        input.extend_from_slice(&cell(0, 1));
        input.extend_from_slice(b"\x1b[39;59m\r\n");
        let out = feed_all(&mut rw, &input);
        let text = strip_escapes(&out);
        assert!(
            super::super::width::str_width(&text) <= 19,
            "cols=20 truncates to 19 cells, got {}: {text:?}",
            super::super::width::str_width(&text)
        );
        assert!(
            text.contains('日') || text.contains('['),
            "prefix survives: {text}"
        );
        assert_eq!(out.iter().filter(|&&b| b == b'\n').count(), 1);
    }

    #[test]
    fn label_never_leaks_placeholder_or_sgr58() {
        let mut rw = LabelRewriter::new(120, true);
        let mut input = Vec::new();
        input.extend_from_slice(b"hello world\r\n");
        input.extend_from_slice(b"\x1b[58:2::0:0:0m");
        input.extend_from_slice(&picture_row((0, 0, 0), 0, 10, b"\r\n"));
        input.extend_from_slice(&picture_row((0, 0, 0), 1, 10, b"\r\n"));
        input.extend_from_slice(&picture_row((0, 0, 0), 2, 10, b"\n"));
        let out = feed_all(&mut rw, &input);
        assert!(
            out.windows(PLACEHOLDER.len()).all(|w| w != PLACEHOLDER),
            "zero U+10EEEE"
        );
        assert!(!out.windows(4).any(|w| w == b"\x1b[58"), "zero ESC[58");
        assert_eq!(
            count_occurrences(&out, SGR_RESET),
            3,
            "ESC[39;59m after each rewrite"
        );
        assert!(out.windows(11).any(|w| w == b"hello world"));
    }

    #[test]
    fn label_chunking_is_irrelevant() {
        // A 5-picture fixture: one chunk must equal every 2-cut.
        let mut input = Vec::new();
        for p in 0u8..5 {
            let rgb = (1 + p, 2, 3);
            input.extend_from_slice(&picture_row(rgb, 0, 6, b"\r\n"));
            input.extend_from_slice(&picture_row(rgb, 1, 6, b"\r\n"));
            input.extend_from_slice(&picture_row(rgb, 2, 6, b"\r\n"));
        }
        input.extend_from_slice(b"done\r\n");
        fn seeded() -> LabelRewriter {
            let mut rw = LabelRewriter::new(201, true);
            rw.add_image(0x010203, 10, 10);
            rw.add_image(0x020203, 20, 20);
            rw.add_image(0x030203, 30, 30);
            rw.add_image(0x040203, 40, 40);
            rw.add_image(0x050203, 50, 50);
            rw
        }
        let whole_out = feed_all(&mut seeded(), &input);
        let mut failed = 0;
        for cut in 1..input.len() {
            let mut rw = seeded();
            let mut out = Vec::new();
            rw.feed(&input[..cut], &mut out);
            rw.feed(&input[cut..], &mut out);
            if out != whole_out {
                failed += 1;
                if failed <= 2 {
                    eprintln!("2-cut at byte {cut} diverged");
                }
            }
        }
        assert_eq!(failed, 0, "every 2-cut equals one chunk");
        // A cut inside F4 8E BB AE specifically.
        let f4 = input
            .windows(PLACEHOLDER.len())
            .position(|w| w == PLACEHOLDER)
            .expect("fixture contains a placeholder");
        for inside in 1..PLACEHOLDER.len() {
            let cut = f4 + inside;
            let mut rw = seeded();
            let mut out = Vec::new();
            rw.feed(&input[..cut], &mut out);
            rw.feed(&input[cut..], &mut out);
            assert_eq!(out, whole_out, "cut inside U+10EEEE at {inside}");
        }
    }

    #[test]
    fn label_hold_cap_flushes_raw() {
        // A row far beyond the 64 KiB cap, no placeholder: the output
        // equals the raw input (flushed raw, not lost).
        let mut rw = LabelRewriter::new(201, true);
        let mut input = Vec::new();
        input.resize(70_000, b'a');
        input.extend_from_slice(b"\r\n");
        let mut out = Vec::new();
        rw.feed(&input[..30_000], &mut out);
        rw.feed(&input[30_000..], &mut out);
        assert_eq!(out, input, "cap flush is byte-identical raw");
    }

    #[test]
    fn label_unknown_id_and_stub_fall_back() {
        // Unknown id (no ImageDef) -> `[image, on web]`.
        let mut rw = LabelRewriter::new(201, true);
        let input = picture_row((0xDE, 0xAD, 0xBE), 0, 4, b"\r\n");
        let out = feed_all(&mut rw, &input);
        assert_eq!(count_occurrences(&out, b"[image, on web]"), 1);
        // Stub (zero pixel dims) -> `[image, on web]`.
        let mut rw = LabelRewriter::new(201, true);
        rw.add_image(0xADBEEF, 0, 0);
        let input = picture_row((0xAD, 0xBE, 0xEF), 0, 4, b"\r\n");
        let out = feed_all(&mut rw, &input);
        assert_eq!(count_occurrences(&out, b"[image, on web]"), 1);
        // 38;5 indexed colour resolves by index.
        let mut rw = LabelRewriter::new(201, true);
        rw.add_image(196, 12, 34);
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[38;5;196m");
        input.extend_from_slice(&cell(0, 0));
        input.extend_from_slice(&cell(0, 1));
        input.extend_from_slice(b"\x1b[39;59m\r\n");
        let out = feed_all(&mut rw, &input);
        assert_eq!(count_occurrences(&out, b"[image 12x34, on web]"), 1);
    }

    #[test]
    fn label_no_dim_flag_omits_dim_sgr() {
        let mut rw = LabelRewriter::new(201, false);
        rw.add_image(0x010101, 8, 8);
        let input = picture_row((1, 1, 1), 0, 3, b"\r\n");
        let out = feed_all(&mut rw, &input);
        assert_eq!(count_occurrences(&out, b"[image 8x8, on web]"), 1);
        assert!(!out.windows(4).any(|w| w == b"\x1b[2m"), "no dim");
        assert!(!out.windows(4).any(|w| w == b"\x1b[0m"), "no reset");
    }

    #[test]
    fn ed3_and_sync_frames_pass_through_unsplit() {
        // ED3, sync frames and the rebuild pass through verbatim and in
        // order, and every line of the rebuild survives.
        let mut rw = LabelRewriter::new(201, true);
        let mut input = Vec::new();
        input.extend_from_slice(b"alpha\r\n");
        input.extend_from_slice(b"\x1b[?2026h");
        input.extend_from_slice(b"\x1b[H");
        input.extend_from_slice(b"\x1b[2J");
        input.extend_from_slice(b"\x1b[3J");
        input.extend_from_slice(b"beta\r\n");
        input.extend_from_slice(b"\x1b[?2026l");
        input.extend_from_slice(b"gamma\r\n");
        let out = feed_all(&mut rw, &input);
        assert_eq!(out, input, "frames and lines pass through unsplit");
        // The single ED3 is intact.
        assert_eq!(
            count_occurrences(&out, b"\x1b[3J"),
            1,
            "ED3 count preserved"
        );
    }
}
