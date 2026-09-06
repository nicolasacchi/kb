//! `haml/1` — the physical-line lexer: split the source into lines with
//! exact byte ranges, measure indentation, and classify each line's opening
//! SIGIL. Nothing here builds a tree and nothing here reads Ruby; that is
//! [`super::parser`]'s and [`super::extract`]'s job respectively.
//!
//! Two rules earn their own note:
//!
//! * **Offsets are absolute byte offsets into the ORIGINAL input**, never
//!   into a normalised copy. A line's `content_start` points at its first
//!   non-indent byte, so every span the parser mints (a tag name, an
//!   attribute hash, an interpolation's inner Ruby) can be sliced straight
//!   out of the caller's `&[u8]` — which is what makes the offset map back
//!   to HAML positions exact rather than reconstructed.
//! * **A tab in the indentation is a DIAGNOSTIC, never a panic and never a
//!   silent reinterpretation.** HAML's own parser raises on mixed
//!   indentation; this scanner records [`super::DiagnosticKind::TabIndent`]
//!   and keeps going with the byte count it measured, because a read-first
//!   instrument that refuses to show a file is worse than one that shows it
//!   with a caption.

/// A byte range in the ORIGINAL source, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span {
            start: start as u32,
            end: end as u32,
        }
    }

    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// The source bytes this span addresses, or `None` when the span is not
    /// a valid range into `src` (which a correct scanner never mints — the
    /// fallible form exists so a test can ASSERT that, rather than panic).
    pub fn slice<'a>(self, src: &'a str) -> Option<&'a str> {
        src.get(self.start as usize..self.end as usize)
    }
}

/// One physical source line.
#[derive(Debug, Clone, Copy)]
pub struct PhysLine {
    /// 1-based, for every human-facing surface (`Symbol::line_start`,
    /// `FrameworkEdge::src_line`, a diagnostic).
    pub line_no: u32,
    /// Byte offset of the line's first byte (its indentation).
    pub start: u32,
    /// Byte offset just past the line's last byte, EXCLUDING the newline.
    pub end: u32,
    /// Byte offset of the first non-indent byte (== `end` for a blank line).
    pub content_start: u32,
    /// Leading horizontal-whitespace byte count — the indentation unit the
    /// level stack compares. Tabs count as one byte each and are flagged.
    pub indent: u32,
    pub has_tab_indent: bool,
    pub blank: bool,
}

impl PhysLine {
    /// The line's content, indentation stripped, newline excluded.
    pub fn content<'a>(&self, src: &'a str) -> &'a str {
        src.get(self.content_start as usize..self.end as usize)
            .unwrap_or("")
    }
}

/// Split `src` into physical lines. Handles `\n` and `\r\n`; a final line
/// with no trailing newline is still a line.
pub fn split_lines(src: &str) -> Vec<PhysLine> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut line_no = 1u32;
    let mut i = 0usize;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'\n' {
            let mut end = i;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            // A trailing empty segment after the file's final newline is
            // not a line — otherwise every well-formed file would report a
            // phantom blank last line.
            if !(i == bytes.len() && start == i && start > 0) {
                out.push(measure(bytes, start, end, line_no));
                line_no += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    out
}

fn measure(bytes: &[u8], start: usize, end: usize, line_no: u32) -> PhysLine {
    let mut c = start;
    let mut has_tab = false;
    while c < end && (bytes[c] == b' ' || bytes[c] == b'\t') {
        if bytes[c] == b'\t' {
            has_tab = true;
        }
        c += 1;
    }
    PhysLine {
        line_no,
        start: start as u32,
        end: end as u32,
        content_start: c as u32,
        indent: (c - start) as u32,
        has_tab_indent: has_tab,
        blank: c == end,
    }
}

/// The opening sigil of a HAML line — what the first one or two content
/// bytes DECLARE the line to be. Resolved before any of the line's payload
/// is read, exactly as HAML itself does it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sigil {
    /// `!!!`, `!!! 5`, `!!! XML`.
    Doctype,
    /// `%tag`, or the implicit-`div` shorthands `.class` / `#id`.
    Tag,
    /// `=` — an output script.
    Script,
    /// `~` — an output script whose result is whitespace-preserved.
    ScriptPreserve,
    /// `&=` — an output script, escaping forced ON.
    ScriptEscaped,
    /// `!=` — an output script, escaping forced OFF.
    ScriptUnescaped,
    /// `==` — PLAIN TEXT that is always interpolated. Not a script sigil
    /// despite the `=`: `== a #{b}` is the text `a #{b}`, never the Ruby
    /// expression `= a #{b}`.
    PlainInterpolated,
    /// `&` / `!` NOT followed by `=` — plain text with the escaping flag
    /// flipped for this line. Text, like [`Sigil::PlainInterpolated`].
    PlainEscapeToggle,
    /// `-` — a silent script (control flow; renders nothing).
    Silent,
    /// `-#` — a HAML comment. Swallows every deeper line, unparsed.
    HamlComment,
    /// `/` — an HTML comment, optionally conditional (`/[if IE]`).
    HtmlComment,
    /// `:name` — a filter block. Its body is opaque to this scanner unless
    /// the filter's own name says otherwise (`:ruby`).
    Filter,
    /// `\` — the next byte's sigil meaning is escaped; the rest is text.
    Escape,
    /// Anything else: plain text.
    Plain,
}

impl Sigil {
    /// How many bytes the sigil itself occupies (never includes the space
    /// after it).
    pub fn width(self) -> usize {
        match self {
            Sigil::Doctype => 3,
            Sigil::HamlComment
            | Sigil::ScriptEscaped
            | Sigil::ScriptUnescaped
            | Sigil::PlainInterpolated => 2,
            Sigil::Tag => 0, // the tag parser reads `%`/`.`/`#` itself
            Sigil::Script
            | Sigil::ScriptPreserve
            | Sigil::PlainEscapeToggle
            | Sigil::Silent
            | Sigil::HtmlComment
            | Sigil::Filter
            | Sigil::Escape => 1,
            Sigil::Plain => 0,
        }
    }

    /// True for the four sigils whose payload is Ruby to be EVALUATED.
    /// `==`/`&`/`!` are deliberately excluded — they are text (see their
    /// own docs), and treating them as Ruby would feed the Rails lens a
    /// fragment that never existed.
    pub fn is_output_script(self) -> bool {
        matches!(
            self,
            Sigil::Script | Sigil::ScriptPreserve | Sigil::ScriptEscaped | Sigil::ScriptUnescaped
        )
    }

    /// The literal source form, for highlight spans and the module's own
    /// tests. `Tag`/`Plain` have no sigil text of their own.
    pub fn as_str(self) -> &'static str {
        match self {
            Sigil::Doctype => "!!!",
            Sigil::Tag => "",
            Sigil::Script => "=",
            Sigil::ScriptPreserve => "~",
            Sigil::ScriptEscaped => "&=",
            Sigil::ScriptUnescaped => "!=",
            Sigil::PlainInterpolated => "==",
            Sigil::PlainEscapeToggle => "&",
            Sigil::Silent => "-",
            Sigil::HamlComment => "-#",
            Sigil::HtmlComment => "/",
            Sigil::Filter => ":",
            Sigil::Escape => "\\",
            Sigil::Plain => "",
        }
    }
}

/// True for a byte that may start (or continue) a `.class`/`#id` shorthand
/// token. Deliberately NOT `is_alphanumeric` over `char`: HAML's shorthand
/// grammar is ASCII, and a UTF-8 continuation byte must never be mistaken
/// for a name byte.
pub fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Classify a line's content. `content` is the line with its indentation
/// already stripped; a blank line has no sigil at all.
pub fn classify(content: &str) -> Option<Sigil> {
    let b = content.as_bytes();
    if b.is_empty() {
        return None;
    }
    let at = |i: usize| -> u8 { b.get(i).copied().unwrap_or(0) };
    // `!!!` must be tested before the `!=`/`!` arms below, or a doctype
    // would lex as an escape-toggled plain-text line.
    if b.starts_with(b"!!!") {
        return Some(Sigil::Doctype);
    }
    Some(match at(0) {
        b'-' if at(1) == b'#' => Sigil::HamlComment,
        b'-' => Sigil::Silent,
        b'=' if at(1) == b'=' => Sigil::PlainInterpolated,
        b'=' => Sigil::Script,
        b'~' => Sigil::ScriptPreserve,
        b'&' if at(1) == b'=' => Sigil::ScriptEscaped,
        b'!' if at(1) == b'=' => Sigil::ScriptUnescaped,
        b'&' | b'!' => Sigil::PlainEscapeToggle,
        b'%' => Sigil::Tag,
        // `.`/`#` are the implicit-div shorthands ONLY when a name byte
        // follows. `#{x}` at line start is interpolated TEXT, and a bare
        // `.` is a full stop — both would become a `%div` under a laxer
        // test, which is the classic HAML-scanner bug.
        b'.' if is_name_byte(at(1)) => Sigil::Tag,
        b'#' if is_name_byte(at(1)) => Sigil::Tag,
        b'/' => Sigil::HtmlComment,
        b':' if at(1).is_ascii_alphabetic() || at(1) == b'_' => Sigil::Filter,
        b'\\' => Sigil::Escape,
        _ => Sigil::Plain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_exact_byte_ranges_and_indentation() {
        let src = "%a\n  %b\n\n    %c\n";
        let lines = split_lines(src);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].line_no, 1);
        assert_eq!(lines[0].content(src), "%a");
        assert_eq!(lines[0].indent, 0);
        assert_eq!(lines[1].content(src), "%b");
        assert_eq!(lines[1].indent, 2);
        assert!(lines[2].blank);
        assert_eq!(lines[3].indent, 4);
        assert_eq!(lines[3].content(src), "%c");
        // Ranges address the ORIGINAL bytes.
        for l in &lines {
            assert_eq!(
                &src[l.start as usize..l.end as usize],
                src.lines().nth(l.line_no as usize - 1).unwrap()
            );
        }
    }

    #[test]
    fn crlf_and_a_missing_final_newline_are_both_lines() {
        let lines = split_lines("%a\r\n%b");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].content("%a\r\n%b"), "%a");
        assert_eq!(lines[1].content("%a\r\n%b"), "%b");
        // A file ending in a newline has no phantom trailing line.
        assert_eq!(split_lines("%a\n").len(), 1);
        assert_eq!(split_lines("").len(), 1);
    }

    #[test]
    fn tabs_in_the_indentation_are_flagged_not_reinterpreted() {
        let lines = split_lines("%a\n\t%b\n");
        assert!(!lines[0].has_tab_indent);
        assert!(lines[1].has_tab_indent);
        assert_eq!(lines[1].indent, 1, "a tab is one indentation BYTE");
    }

    #[test]
    fn every_sigil_is_classified_and_the_near_misses_are_not() {
        let table: &[(&str, Option<Sigil>)] = &[
            ("!!! 5", Some(Sigil::Doctype)),
            ("%p hi", Some(Sigil::Tag)),
            (".card", Some(Sigil::Tag)),
            ("#hero", Some(Sigil::Tag)),
            ("= foo", Some(Sigil::Script)),
            ("== a #{b}", Some(Sigil::PlainInterpolated)),
            ("~ foo", Some(Sigil::ScriptPreserve)),
            ("&= foo", Some(Sigil::ScriptEscaped)),
            ("!= foo", Some(Sigil::ScriptUnescaped)),
            ("& text", Some(Sigil::PlainEscapeToggle)),
            ("! text", Some(Sigil::PlainEscapeToggle)),
            ("- if x", Some(Sigil::Silent)),
            ("-# note", Some(Sigil::HamlComment)),
            ("/ comment", Some(Sigil::HtmlComment)),
            (":javascript", Some(Sigil::Filter)),
            ("\\= literal", Some(Sigil::Escape)),
            // The near misses: none of these is a tag or a filter.
            ("#{foo} bar", Some(Sigil::Plain)),
            (". a full stop", Some(Sigil::Plain)),
            ("#", Some(Sigil::Plain)),
            (": not a filter", Some(Sigil::Plain)),
            ("just text", Some(Sigil::Plain)),
            ("", None),
        ];
        for (line, expected) in table {
            assert_eq!(classify(line), *expected, "classify({line:?})");
        }
    }

    #[test]
    fn doctype_wins_over_the_escape_toggle_sigil() {
        // `!!!` starts with `!`; without the explicit ordering in
        // `classify` it would lex as escape-toggled plain text.
        assert_eq!(classify("!!!"), Some(Sigil::Doctype));
        assert_eq!(classify("!!"), Some(Sigil::PlainEscapeToggle));
    }
}
