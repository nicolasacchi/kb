//! `syntax/1` (V72-H1, design D7) — the ONE table that says, for a file
//! type, *what this daemon can do with it*: which tree-sitter grammar (if
//! any) parses it, which EXTRACTION TIER the ingest pipeline runs, whether
//! the type is an injection host, and which extensions / exact filenames /
//! `#!` interpreters address it.
//!
//! Before this module, "which language is this file" lived in one `match`
//! on `Path::extension()` inside [`crate::lang::detect`], and "what do we
//! do with it" lived in five unrelated `const` sets scattered across
//! `lang.rs`, `imports.rs`, `locals.rs`, `hierarchy.rs` and `entities/`.
//! The failure mode that produces is Sourcetrail's: N frontends that rot
//! independently, with no single place a reader can look to find out that
//! `.rake` is Ruby source the instrument silently ignores. The registry is
//! the declaration; the [Parity Grid](parity_grid) is the honest map
//! derived FROM it.
//!
//! # The tier
//!
//! [`Tier`] is the extraction tier, three values:
//!
//! * **`Full`** — highlight spans AND symbol extraction.
//! * **`HighlightOnly`** — highlight spans; symbol extraction is SKIPPED,
//!   deliberately, by ONE short-circuit at the top of the ingest pipeline
//!   ([`SyntaxRow::plan`], consumed in `ingest::index_file`). Never an
//!   aborted walk: the file still gets its `files` row, its highlight
//!   rows, and an explicitly EMPTY symbol set, so a reader is told "no
//!   symbols by tier" rather than being shown an empty list that looks
//!   like a bug.
//! * **`None`** — neither pass runs. Two distinct shapes both land here
//!   and the `grammar` field tells them apart: a type with NO grammar
//!   linked in this build (`sql`, `dockerfile` — the row exists so the
//!   gap is NAMED and the Parity Grid can show it), and a parse-only
//!   grammar (`erb`, registered so `frameworks::rails::views` can walk
//!   tag boundaries, with no tags/highlights query of its own).
//!
//! **`Tier` is NOT `ingest::TIER_*`.** Those four constants
//! (`unknown`/`binary`/`too-large`/`lfs`) are CONTENT skip markers written
//! into `files.lang`; this one is a property of the file TYPE, decided
//! before a byte is read. The wire field `tier` on `GET /api/file` and
//! `GET /api/symbols` carries THIS one, and says only what the type's
//! declared pipeline is — never that a particular blob has been derived
//! (that is what `symbols`/`highlights` themselves report).
//!
//! # Detection order
//!
//! 1. **extension** — exact, case-sensitive, lowercase table (`.rb`,
//!    `.rake`, `.jbuilder`, `.gemspec`, `.ru`, …);
//! 2. **exact filename** — the stem table D7 asks for (`Gemfile`,
//!    `Rakefile`, `Guardfile`, `Capfile`, `Dockerfile`). Exact, so
//!    `Gemfile.lock` (whose extension `lock` matches nothing) is NOT Ruby
//!    — it is a resolver artefact, not Ruby source, and D7 names it as
//!    plain on purpose;
//! 3. **`#!` interpreter** — only for a file with NO extension at all,
//!    exactly as the pre-V72-H1 sniff was gated. The interpreter's
//!    basename is version-stripped (`python3.11` → `python`, `ksh93` →
//!    `ksh`) and looked up in the registry's own `interpreters` lists.
//!
//! [`crate::lang::detect`] is now a thin façade over step 1–3: it returns
//! the matched row's [`crate::lang::LangInfo`], which exists exactly when
//! the row has a grammar. That keeps its contract byte-identical — `Some`
//! has always meant "there is a grammar, and `salt` keys its derived rows"
//! — so a grammar-less registry row (`sql`, `dockerfile`) is invisible to
//! every existing caller and shows up only on the two new read surfaces.
//!
//! # The Parity Grid
//!
//! [`parity_grid`] is `rows × {highlight, symbols, outline, usages, hover,
//! lens}`, every cell DERIVED from this registry plus the predicates that
//! already gate those lanes (`lang::supports_token_level`,
//! `lang::tags_query`, `lang::locals_query`, `extract::CST_OUTLINE_LANG_IDS`)
//! — never hand-typed, so it cannot claim a capability the daemon does not
//! have. A cell is `yes`, `no` or `partial`, and everything except `yes`
//! carries a REASON. The grid is pinned by a checked-in golden
//! (`tests/fixtures/parity.golden.json`), so any capability change is a
//! deliberate golden update, reviewable in the diff that causes it.
//!
//! Nothing here is persisted or cached: both routes are computed per
//! request from compile-time data (root CLAUDE.md invariant #2's posture).

use crate::lang::{self, LangInfo};
use serde::Serialize;

pub const SYNTAX_SCHEMA: &str = "syntax/1";
pub const PARITY_SCHEMA: &str = "parity/1";

/// What the ingest pipeline runs for a file type. See the module doc —
/// and note this is a different axis from `ingest::TIER_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Highlight spans + symbol extraction.
    Full,
    /// Highlight spans only; symbols skipped by one short-circuit.
    HighlightOnly,
    /// Neither pass runs (no grammar, or a parse-only grammar).
    None,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Full => "full",
            Tier::HighlightOnly => "highlight_only",
            Tier::None => "none",
        }
    }
}

/// What [`crate::ingest::index_file`] derives for a row — the ONE
/// short-circuit the HIGHLIGHT_ONLY tier is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub highlight: bool,
    pub symbols: bool,
}

/// One file type. Rows are `'static` data; the table below is the source
/// of truth and every other language-shaped list in this crate is either
/// derived from it or pinned against it by a test.
#[derive(Debug, Clone, Copy)]
pub struct SyntaxRow {
    /// The language id — the same string `files.lang`, `lang::for_id` and
    /// every salt use.
    pub lang: &'static str,
    /// `Some` exactly when a tree-sitter grammar is linked for this row;
    /// carries the cache salt. `None` means the type is NAMED but has no
    /// grammar in this build.
    pub info: Option<LangInfo>,
    /// The grammar crate, for the `syntax/1` wire. `Some`/`None` in
    /// lock-step with `info` (test-pinned).
    pub grammar: Option<&'static str>,
    pub tier: Tier,
    /// This type embeds other languages (ERB hosts Ruby + HTML). Declared
    /// for H2a's injection-aware pipeline, which is what will consume it;
    /// NOTHING reads it today beyond the `syntax/1` wire, and saying so
    /// here is cheaper than letting a reader assume injections work.
    pub injection_host: bool,
    /// Extensions, without the dot, lowercase, matched exactly.
    pub extensions: &'static [&'static str],
    /// Exact file names (D7's stem table).
    pub filenames: &'static [&'static str],
    /// `#!` interpreter basenames, version-stripped (see the module doc).
    pub interpreters: &'static [&'static str],
    /// Why the tier is not `Full`. `None` for a `Full` row (test-pinned).
    pub note: Option<&'static str>,
}

impl SyntaxRow {
    /// The extraction plan — the HIGHLIGHT_ONLY short-circuit, as data.
    pub fn plan(&self) -> Plan {
        match self.tier {
            Tier::Full => Plan {
                highlight: true,
                symbols: true,
            },
            Tier::HighlightOnly => Plan {
                highlight: true,
                symbols: false,
            },
            Tier::None => Plan {
                highlight: false,
                symbols: false,
            },
        }
    }
}

const NO_GRAMMAR: &str =
    "no tree-sitter grammar linked in this build — the type is named so the gap is visible";

/// The registry. Order is the wire order and the Parity Grid's row order:
/// the eleven grammar languages in `lang.rs`'s own declaration order,
/// then the parse-only grammar, then the named-but-grammar-less types.
pub const REGISTRY: &[SyntaxRow] = &[
    SyntaxRow {
        lang: "rust",
        info: Some(lang::RUST),
        grammar: Some("tree-sitter-rust"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["rs"],
        filenames: &[],
        interpreters: &[],
        note: None,
    },
    SyntaxRow {
        lang: "python",
        info: Some(lang::PYTHON),
        grammar: Some("tree-sitter-python"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["py"],
        filenames: &[],
        // V72-H1 widens the extensionless sniff past bash-family: an
        // extensionless `#!/usr/bin/env python3` script is Python source
        // and was previously indexed as `unknown`.
        interpreters: &["python"],
        note: None,
    },
    SyntaxRow {
        lang: "ruby",
        info: Some(lang::RUBY),
        grammar: Some("tree-sitter-ruby"),
        tier: Tier::Full,
        injection_host: false,
        // D7's Ruby-by-another-name set. `.ru` is `config.ru` (Rack),
        // `.jbuilder` is a Rails view that is plain Ruby, `.gemspec` and
        // `.rake` are Ruby DSL files. All were `unknown` before V72-H1.
        extensions: &["rb", "rake", "jbuilder", "gemspec", "ru"],
        // Exact names only. `Gemfile.lock` deliberately absent — it is a
        // resolver artefact in its own format, not Ruby (D7: "plain").
        filenames: &["Gemfile", "Rakefile", "Guardfile", "Capfile"],
        interpreters: &["ruby"],
        note: None,
    },
    SyntaxRow {
        lang: "typescript",
        info: Some(lang::TYPESCRIPT),
        grammar: Some("tree-sitter-typescript"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["ts", "mts", "cts"],
        filenames: &[],
        interpreters: &[],
        note: None,
    },
    SyntaxRow {
        lang: "tsx",
        info: Some(lang::TSX),
        grammar: Some("tree-sitter-typescript"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["tsx"],
        filenames: &[],
        interpreters: &[],
        note: None,
    },
    SyntaxRow {
        lang: "javascript",
        info: Some(lang::JAVASCRIPT),
        grammar: Some("tree-sitter-javascript"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["js", "jsx", "mjs", "cjs"],
        filenames: &[],
        interpreters: &[],
        note: None,
    },
    SyntaxRow {
        lang: "bash",
        info: Some(lang::BASH),
        grammar: Some("tree-sitter-bash"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["sh", "bash"],
        filenames: &[],
        interpreters: &["bash", "sh", "dash", "ksh"],
        note: None,
    },
    SyntaxRow {
        lang: "yaml",
        info: Some(lang::YAML),
        grammar: Some("tree-sitter-yaml"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["yml", "yaml"],
        filenames: &[],
        interpreters: &[],
        note: None,
    },
    SyntaxRow {
        lang: "go",
        info: Some(lang::GO),
        grammar: Some("tree-sitter-go"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["go"],
        filenames: &[],
        interpreters: &[],
        note: None,
    },
    SyntaxRow {
        lang: "toml",
        info: Some(lang::TOML),
        grammar: Some("tree-sitter-toml-ng"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["toml"],
        filenames: &[],
        interpreters: &[],
        note: None,
    },
    SyntaxRow {
        lang: "json",
        info: Some(lang::JSON),
        grammar: Some("tree-sitter-json"),
        tier: Tier::Full,
        injection_host: false,
        extensions: &["json"],
        filenames: &[],
        interpreters: &[],
        note: None,
    },
    SyntaxRow {
        lang: "erb",
        info: Some(lang::ERB),
        grammar: Some("tree-sitter-embedded-template"),
        tier: Tier::None,
        // ERB is the injection host this crate already parses (Ruby
        // inside HTML); the pipeline that would USE that is H2a's.
        injection_host: true,
        extensions: &["erb"],
        filenames: &[],
        interpreters: &[],
        note: Some(
            "parse-only grammar: registered so the Rails view scanner can walk tag \
             boundaries; no tags or highlights query in this build",
        ),
    },
    SyntaxRow {
        lang: "dockerfile",
        info: None,
        grammar: None,
        tier: Tier::None,
        injection_host: false,
        extensions: &[],
        filenames: &["Dockerfile"],
        interpreters: &[],
        note: Some(NO_GRAMMAR),
    },
    SyntaxRow {
        lang: "sql",
        info: None,
        grammar: None,
        tier: Tier::None,
        injection_host: false,
        extensions: &["sql"],
        filenames: &[],
        interpreters: &[],
        note: Some(NO_GRAMMAR),
    },
];

/// The registry row for `lang`, or `None`.
pub fn row_for_lang(lang: &str) -> Option<&'static SyntaxRow> {
    REGISTRY.iter().find(|r| r.lang == lang)
}

/// The registry row addressed by `path` (and, for an extensionless file
/// with a shebang, `content`) — the detection ladder in the module doc.
///
/// Returns a row even when that row has NO grammar, which is the whole
/// point of naming `sql`/`dockerfile`: the file read wire can say "tier
/// none, and here is why" instead of an undifferentiated silence.
pub fn row_for_path(path: &str, content: Option<&[u8]>) -> Option<&'static SyntaxRow> {
    let p = std::path::Path::new(path);
    // (1) extension. Case-sensitive, exactly as the pre-V72-H1 `match`
    // was — an uppercase `.RB` was never Ruby here and widening that is a
    // separate, testable decision, not a refactor side effect.
    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
        if let Some(row) = REGISTRY.iter().find(|r| r.extensions.contains(&ext)) {
            return Some(row);
        }
        // An extension that matches nothing stops here: the shebang sniff
        // has always been gated on "no extension at all".
        return None;
    }
    // (2) exact filename (D7's stem table).
    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
        if let Some(row) = REGISTRY.iter().find(|r| r.filenames.contains(&name)) {
            return Some(row);
        }
    }
    // (3) `#!` interpreter.
    let interpreter = content.and_then(shebang_interpreter)?;
    REGISTRY
        .iter()
        .find(|r| r.interpreters.contains(&interpreter.as_str()))
}

/// The version-stripped basename of `bytes`' `#!` interpreter, if the
/// bytes start with a shebang.
///
/// `#!/usr/bin/env bash` indirection is followed (the real interpreter is
/// the next word); a trailing version suffix is stripped so `python3.11`,
/// `ruby3.2` and `ksh93` all reduce to the registry's own version-free
/// interpreter names. Everything else about this is the pre-V72-H1
/// `shebang_is_bash_family` walk, byte for byte — only the final lookup
/// changed from a hardcoded bash-family `matches!` to the registry.
fn shebang_interpreter(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"#!") {
        return None;
    }
    let line_end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let line = std::str::from_utf8(&bytes[2..line_end]).ok()?;
    let mut parts = line.split_whitespace();
    let first = parts.next()?;
    let interpreter = if first == "env" || first.ends_with("/env") {
        parts.next().unwrap_or("")
    } else {
        first
    };
    let name = interpreter.rsplit('/').next().unwrap_or(interpreter);
    let stripped = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if stripped.is_empty() {
        return None;
    }
    Some(stripped.to_string())
}

/// The declared extraction tier for `path`, plus the reason it is not
/// `Full`. An unregistered file type is an honest `(None, reason)`, not
/// silence.
pub fn tier_for_path(path: &str, content: Option<&[u8]>) -> (Tier, Option<&'static str>) {
    match row_for_path(path, content) {
        Some(row) => (row.tier, row.note),
        None => (
            Tier::None,
            Some("no syntax/1 registry row for this file type"),
        ),
    }
}

// ── the `syntax/1` wire ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SyntaxRowOut {
    pub lang: &'static str,
    pub tier: &'static str,
    /// The tree-sitter grammar crate, or `null` when none is linked.
    pub grammar: Option<&'static str>,
    /// The derived-row cache salt, `null` in lock-step with `grammar`.
    pub salt: Option<&'static str>,
    pub injection_host: bool,
    pub extensions: &'static [&'static str],
    pub filenames: &'static [&'static str],
    pub interpreters: &'static [&'static str],
    /// Why the tier is not `full`.
    pub note: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyntaxOut {
    pub schema: &'static str,
    pub rows: Vec<SyntaxRowOut>,
    /// True total. The registry is served whole, so `truncated` is always
    /// false — it is on the wire because every list this daemon serves
    /// reports its own cap, and a reader should not have to remember
    /// which ones are exempt.
    pub total: usize,
    pub truncated: bool,
}

/// The registry, as `syntax/1`.
pub fn syntax_registry() -> SyntaxOut {
    let rows: Vec<SyntaxRowOut> = REGISTRY
        .iter()
        .map(|r| SyntaxRowOut {
            lang: r.lang,
            tier: r.tier.as_str(),
            grammar: r.grammar,
            salt: r.info.map(|i| i.salt),
            injection_host: r.injection_host,
            extensions: r.extensions,
            filenames: r.filenames,
            interpreters: r.interpreters,
            note: r.note,
        })
        .collect();
    SyntaxOut {
        schema: SYNTAX_SCHEMA,
        total: rows.len(),
        truncated: false,
        rows,
    }
}

// ── the Parity Grid ──────────────────────────────────────────────────────

/// The grid's columns, in wire order.
pub const CAPABILITIES: &[&str] = &["highlight", "symbols", "outline", "usages", "hover", "lens"];

pub const STATE_YES: &str = "yes";
pub const STATE_NO: &str = "no";
pub const STATE_PARTIAL: &str = "partial";

#[derive(Debug, Clone, Serialize)]
pub struct ParityCell {
    pub capability: &'static str,
    /// `yes` | `no` | `partial`.
    pub state: &'static str,
    /// Present for every state except `yes`. A `no` explains itself the
    /// same way a `partial` does: this grid exists to show where the
    /// instrument is weak, and an unexplained gap is not a map.
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParityRow {
    pub lang: &'static str,
    pub tier: &'static str,
    pub cells: Vec<ParityCell>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParityOut {
    pub schema: &'static str,
    pub capabilities: &'static [&'static str],
    pub rows: Vec<ParityRow>,
    pub total: usize,
    pub truncated: bool,
}

fn cell(capability: &'static str, state: &'static str, reason: Option<&'static str>) -> ParityCell {
    ParityCell {
        capability,
        state,
        reason,
    }
}

/// Why this row runs no pass at all — the row's own note, or the generic
/// grammar-less reason.
fn no_pass_reason(row: &SyntaxRow) -> Option<&'static str> {
    Some(row.note.unwrap_or(NO_GRAMMAR))
}

fn cell_highlight(row: &SyntaxRow) -> ParityCell {
    if row.plan().highlight {
        cell("highlight", STATE_YES, None)
    } else {
        cell("highlight", STATE_NO, no_pass_reason(row))
    }
}

fn cell_symbols(row: &SyntaxRow) -> ParityCell {
    if !row.plan().symbols {
        let reason = match row.tier {
            Tier::HighlightOnly => {
                Some("tier highlight_only — symbol extraction is skipped by design, not missing")
            }
            _ => no_pass_reason(row),
        };
        return cell("symbols", STATE_NO, reason);
    }
    if lang::tags_query(row.lang).is_some() {
        return cell("symbols", STATE_YES, None);
    }
    if crate::extract::CST_OUTLINE_LANG_IDS.contains(&row.lang) {
        return cell(
            "symbols",
            STATE_PARTIAL,
            Some("key-path outline rows (kind \"key\") — a data outline, not code definitions"),
        );
    }
    cell(
        "symbols",
        STATE_NO,
        Some("no tags query and no CST outline for this language"),
    )
}

fn cell_outline(row: &SyntaxRow) -> ParityCell {
    let symbols = cell_symbols(row);
    if symbols.state == STATE_NO {
        return cell("outline", STATE_NO, symbols.reason);
    }
    // Every language that HAS symbols also has an outline today — the
    // reader's structure popup is a render of `GET /api/symbols`. What
    // does not exist yet is the universal `outline/1` contract (one
    // `{kind,name,range,badges}` shape per file type, D7), so no row can
    // honestly claim a full `yes` here until H2a ships it.
    cell(
        "outline",
        STATE_PARTIAL,
        Some("rendered from the symbols table; the universal outline/1 contract is not built yet"),
    )
}

fn cell_usages(row: &SyntaxRow) -> ParityCell {
    if !lang::supports_token_level(row.lang) {
        return cell(
            "usages",
            STATE_NO,
            Some("no occurrences index — every usages row is occurrence-backed"),
        );
    }
    // An `exact` row needs a scope proof: the four ingest-stamped locals
    // languages, or Ruby's read-time STRICT lane — which is exactly the
    // set with a vendored `*-locals.scm`.
    if lang::locals_query(row.lang).is_some() {
        return cell("usages", STATE_YES, None);
    }
    cell(
        "usages",
        STATE_PARTIAL,
        Some("likely/candidate only — no locals query, so the static ladder has no exact tier"),
    )
}

fn cell_hover(row: &SyntaxRow) -> ParityCell {
    if lang::supports_token_level(row.lang) {
        return cell("hover", STATE_YES, None);
    }
    if row.plan().symbols {
        return cell(
            "hover",
            STATE_PARTIAL,
            Some("word-scan resolve over key-path rows — no occurrences index to bind a position"),
        );
    }
    cell(
        "hover",
        STATE_NO,
        Some("no symbols and no occurrences — the resolve ladder has nothing to bind"),
    )
}

fn cell_lens(row: &SyntaxRow) -> ParityCell {
    if lang::supports_token_level(row.lang) {
        return cell("lens", STATE_YES, None);
    }
    cell(
        "lens",
        STATE_NO,
        Some(
            "lens rows are callable/type declarations with occurrence-backed usage counts; \
             this language has neither",
        ),
    )
}

/// The Parity Grid — every registry row against every capability, each
/// cell derived from the predicate that actually gates that lane.
pub fn parity_grid() -> ParityOut {
    let rows: Vec<ParityRow> = REGISTRY
        .iter()
        .map(|r| ParityRow {
            lang: r.lang,
            tier: r.tier.as_str(),
            cells: vec![
                cell_highlight(r),
                cell_symbols(r),
                cell_outline(r),
                cell_usages(r),
                cell_hover(r),
                cell_lens(r),
            ],
        })
        .collect();
    ParityOut {
        schema: PARITY_SCHEMA,
        capabilities: CAPABILITIES,
        total: rows.len(),
        truncated: false,
        rows,
    }
}

// ── routes ───────────────────────────────────────────────────────────────

use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;

/// `GET /api/syntax` — the registry. Build-time data, no repo content, no
/// params; the same ordinary `auth_bearer` read as `/api/themes`.
pub async fn syntax_route() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(syntax_registry()),
    )
}

/// `GET /api/parity` — the Parity Grid, derived per request from the
/// registry and this binary's own capability predicates. Nothing is
/// persisted (root CLAUDE.md invariant #2).
pub async fn parity_route() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Json(parity_grid()))
}

// ── the declaration↔handler walk (crate invariant 15) ────────────────────

/// Neither route takes a param, so there is nothing a request can omit.
/// The contract still ships so both dead-surface walks — this crate's
/// against `router.rs`, kb-code-cli's against the verbs — cover them.
fn no_params_accept_without(_omit: &str) -> bool {
    true
}

pub const SYNTAX_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/syntax",
    handler: "syntax::syntax_route",
    required_params: &[],
    params_accept_without: no_params_accept_without,
};

pub const PARITY_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/parity",
    handler: "syntax::parity_route",
    required_params: &[],
    params_accept_without: no_params_accept_without,
};

/// Every route V72-H1 adds. Walked from BOTH sides exactly as
/// `entities::V71_G0_ROUTES` is — see that list's doc.
pub const V72_H1_ROUTES: &[crate::entities::RouteContract] = &[SYNTAX_ROUTE, PARITY_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("router.rs");
    /// The CI golden. Lives under `tests/` (where a reviewer looks for a
    /// fixture) and is read from here (where the only test that can
    /// regenerate it lives).
    const PARITY_GOLDEN: &str = include_str!("../tests/fixtures/parity.golden.json");

    // ── detection: the pre-V72-H1 table, byte for byte ───────────────────

    /// Every `(path, expected lang)` pair the pre-V72-H1 `lang::detect`
    /// `match` produced, asserted against the registry-backed one. This is
    /// the "pin it before you refactor" test: the registry may only ADD
    /// detections, never move or drop one.
    #[test]
    fn detection_is_byte_identical_for_every_pre_v72_h1_extension() {
        let table: &[(&str, Option<&str>)] = &[
            ("src/lib.rs", Some("rust")),
            ("pkg/module.py", Some("python")),
            ("app/model.rb", Some("ruby")),
            ("src/app.ts", Some("typescript")),
            ("src/app.mts", Some("typescript")),
            ("src/app.cts", Some("typescript")),
            ("src/Widget.tsx", Some("tsx")),
            ("src/app.js", Some("javascript")),
            ("src/app.jsx", Some("javascript")),
            ("src/app.mjs", Some("javascript")),
            ("src/app.cjs", Some("javascript")),
            ("bin/run.sh", Some("bash")),
            ("bin/run.bash", Some("bash")),
            ("k8s/deploy.yml", Some("yaml")),
            ("k8s/deploy.yaml", Some("yaml")),
            ("cmd/main.go", Some("go")),
            ("Cargo.toml", Some("toml")),
            ("package.json", Some("json")),
            ("app/views/show.html.erb", Some("erb")),
            ("app/views/show.turbo_stream.erb", Some("erb")),
            // Not detected before, and STILL not detected: an extension
            // that matches nothing never falls through to the stem table
            // or the shebang sniff.
            ("README.md", None),
            ("noextension", None),
            ("Gemfile.lock", None),
            ("src/lib.RS", None),
        ];
        for (path, expected) in table {
            assert_eq!(
                lang::detect(path, None).map(|l| l.id),
                *expected,
                "detect({path:?})"
            );
        }
    }

    #[test]
    fn stem_table_resolves_d7s_ruby_by_another_name_set() {
        for path in [
            "Gemfile",
            "Rakefile",
            "Guardfile",
            "Capfile",
            "config.ru",
            "lib/tasks/db.rake",
            "app/views/orders/show.json.jbuilder",
            "kb.gemspec",
            "vendor/nested/Gemfile",
        ] {
            assert_eq!(
                lang::detect(path, None).map(|l| l.id),
                Some("ruby"),
                "detect({path:?})"
            );
        }
        // D7 names `Gemfile.lock` plain on purpose: it is a resolver
        // artefact in its own format, not Ruby source. Exact-name
        // matching is what keeps it out.
        assert_eq!(lang::detect("Gemfile.lock", None), None);
        // A stem row with no grammar is a registry row but not a `detect`
        // hit — `Some` has always meant "there is a grammar".
        assert_eq!(lang::detect("Dockerfile", None), None);
        assert_eq!(
            row_for_path("Dockerfile", None).map(|r| r.lang),
            Some("dockerfile")
        );
        assert_eq!(lang::detect("db/migrate.sql", None), None);
        assert_eq!(
            row_for_path("db/migrate.sql", None).map(|r| r.lang),
            Some("sql")
        );
    }

    #[test]
    fn shebang_sniff_covers_the_registry_interpreters_only() {
        let table: &[(&[u8], Option<&str>)] = &[
            (b"#!/bin/bash\necho hi\n", Some("bash")),
            (b"#!/bin/sh\necho hi\n", Some("bash")),
            (b"#!/usr/bin/env bash\necho hi\n", Some("bash")),
            (b"#!/usr/bin/env dash\n", Some("bash")),
            (b"#!/bin/ksh93\n", Some("bash")),
            // V72-H1 widens the sniff: these two were `None` before.
            (b"#!/usr/bin/env ruby\n", Some("ruby")),
            (b"#!/usr/bin/env python3\n", Some("python")),
            (b"#!/usr/bin/python3.11\n", Some("python")),
            // Still not sniffed — no registry row claims them.
            (b"#!/usr/bin/perl\n", None),
            (b"#!/usr/bin/env node\n", None),
            (b"just some text\n", None),
        ];
        for (bytes, expected) in table {
            assert_eq!(
                lang::detect("bin/tool", Some(*bytes)).map(|l| l.id),
                *expected,
                "detect(bin/tool, {:?})",
                String::from_utf8_lossy(bytes),
            );
        }
        // No content, no sniff.
        assert_eq!(lang::detect("bin/tool", None), None);
        // An extension always wins over content — no sniff attempted.
        assert_eq!(
            lang::detect("bin/tool.py", Some(b"#!/bin/bash\n")).map(|l| l.id),
            Some("python")
        );
    }

    /// The wire values `GET /api/file` and per-file `GET /api/symbols`
    /// carry, for each of the four shapes a caller can hit.
    #[test]
    fn tier_for_path_answers_for_every_shape() {
        // Full: nothing to explain.
        assert_eq!(tier_for_path("app/model.rb", None), (Tier::Full, None));
        // A parse-only grammar: `none`, and it says which kind of none.
        let (tier, reason) = tier_for_path("app/views/show.html.erb", None);
        assert_eq!(tier, Tier::None);
        assert!(reason.expect("erb explains itself").contains("parse-only"));
        // A named type with no grammar linked.
        let (tier, reason) = tier_for_path("Dockerfile", None);
        assert_eq!(tier, Tier::None);
        assert!(reason
            .expect("dockerfile explains itself")
            .contains("no tree-sitter grammar"));
        // Not in the registry at all — distinguishable from the above.
        let (tier, reason) = tier_for_path("README.md", None);
        assert_eq!(tier, Tier::None);
        assert_eq!(reason, Some("no syntax/1 registry row for this file type"));
    }

    // ── registry consistency ─────────────────────────────────────────────

    #[test]
    fn grammar_salt_and_info_are_in_lock_step_and_agree_with_lang() {
        for r in REGISTRY {
            assert_eq!(
                r.info.is_some(),
                r.grammar.is_some(),
                "{}: info and grammar must be Some/None together",
                r.lang
            );
            assert_eq!(
                r.info,
                lang::for_id(r.lang),
                "{}: registry row and lang::for_id disagree",
                r.lang
            );
            if let Some(info) = r.info {
                assert_eq!(
                    info.id, r.lang,
                    "{}: LangInfo id must be the row's lang",
                    r.lang
                );
            }
        }
        // Every `lang::for_id` entry has a row — the registry is the
        // complete set, not a subset.
        for info in lang::ALL_LANGS {
            assert!(
                REGISTRY.iter().any(|r| r.info == Some(*info)),
                "{}: in ALL_LANGS but missing from the syntax/1 registry",
                info.id
            );
        }
    }

    #[test]
    fn every_key_is_claimed_by_exactly_one_row() {
        for (kind, keys) in [
            (
                "extension",
                REGISTRY
                    .iter()
                    .flat_map(|r| r.extensions)
                    .collect::<Vec<_>>(),
            ),
            (
                "filename",
                REGISTRY
                    .iter()
                    .flat_map(|r| r.filenames)
                    .collect::<Vec<_>>(),
            ),
        ] {
            let mut sorted: Vec<&&str> = keys.clone();
            sorted.sort_unstable();
            let before = sorted.len();
            sorted.dedup();
            assert_eq!(
                before,
                sorted.len(),
                "a {kind} is claimed by two rows: {keys:?}"
            );
        }
        // Interpreters may not collide either — two rows claiming `sh`
        // would make the sniff order-dependent.
        let mut interps: Vec<&&str> = REGISTRY.iter().flat_map(|r| r.interpreters).collect();
        let before = interps.len();
        interps.sort_unstable();
        interps.dedup();
        assert_eq!(
            before,
            interps.len(),
            "an interpreter is claimed by two rows"
        );
        // Lang ids are unique.
        let mut langs: Vec<&str> = REGISTRY.iter().map(|r| r.lang).collect();
        let before = langs.len();
        langs.sort_unstable();
        langs.dedup();
        assert_eq!(before, langs.len(), "a lang id appears twice");
    }

    /// The tier is a CLAIM about what the pipeline does; these are the
    /// facts that must back it. A `Full` row that has no highlights query
    /// (or no way to make a symbol) would ship a tier that lies.
    #[test]
    fn every_tier_claim_is_backed_by_the_thing_that_implements_it() {
        for r in REGISTRY {
            let plan = r.plan();
            if plan.highlight {
                assert!(
                    lang::highlights_query(r.lang).is_some(),
                    "{}: tier {} promises highlight spans but has no highlights query",
                    r.lang,
                    r.tier.as_str()
                );
            }
            if plan.symbols {
                assert!(
                    lang::tags_query(r.lang).is_some()
                        || crate::extract::CST_OUTLINE_LANG_IDS.contains(&r.lang),
                    "{}: tier full promises symbols but has neither a tags query nor a CST \
                     outline",
                    r.lang
                );
            }
            assert_eq!(
                r.note.is_none(),
                r.tier == Tier::Full,
                "{}: a non-full tier must say why, and a full tier has nothing to explain",
                r.lang
            );
        }
    }

    /// The structural guarantee behind "one short-circuit, never an
    /// aborted walk": every DOWNSTREAM ingest pass is gated on its own
    /// id-keyed predicate, and no non-`Full` language may be in any of
    /// them. Break this and a HIGHLIGHT_ONLY file would reach a pass that
    /// needs the symbols the tier just skipped.
    #[test]
    fn no_non_full_language_is_in_any_downstream_pass() {
        for r in REGISTRY {
            if r.tier == Tier::Full {
                continue;
            }
            let id = r.lang;
            assert!(!lang::supports_token_level(id), "{id}: token-level");
            assert!(!crate::imports::supports(id), "{id}: imports");
            assert!(!crate::locals::supports(id), "{id}: locals");
            assert!(!crate::hierarchy::supports_hierarchy(id), "{id}: hierarchy");
            assert!(!crate::entities::indexes_lang(id), "{id}: entities");
        }
    }

    /// HIGHLIGHT_ONLY ships as a MECHANISM with no production row yet —
    /// recorded here, with the reason, rather than left to be discovered.
    /// The first rows arrive with H2a's SCSS/CSS/Markdown grammars; this
    /// test is the one that will fail then, which is the point.
    #[test]
    fn highlight_only_has_no_production_row_yet() {
        let rows: Vec<&str> = REGISTRY
            .iter()
            .filter(|r| r.tier == Tier::HighlightOnly)
            .map(|r| r.lang)
            .collect();
        assert!(
            rows.is_empty(),
            "HIGHLIGHT_ONLY now has production rows ({rows:?}) — update this test AND the \
             parity golden in the same commit"
        );
    }

    /// The tier short-circuit itself, exercised over a synthetic row (the
    /// mechanism is what is under test, not any particular language).
    #[test]
    fn the_tier_short_circuit_is_the_plan() {
        let full = Plan {
            highlight: true,
            symbols: true,
        };
        let hl_only = Plan {
            highlight: true,
            symbols: false,
        };
        let none = Plan {
            highlight: false,
            symbols: false,
        };
        let mut row = REGISTRY
            .iter()
            .find(|r| r.lang == "ruby")
            .copied()
            .expect("ruby row");
        assert_eq!(row.plan(), full);
        row.tier = Tier::HighlightOnly;
        assert_eq!(row.plan(), hl_only);
        row.tier = Tier::None;
        assert_eq!(row.plan(), none);
    }

    // ── the Parity Grid ──────────────────────────────────────────────────

    #[test]
    fn every_row_reports_every_capability_in_column_order() {
        let grid = parity_grid();
        assert_eq!(grid.total, REGISTRY.len());
        assert!(!grid.truncated);
        for row in &grid.rows {
            let names: Vec<&str> = row.cells.iter().map(|c| c.capability).collect();
            assert_eq!(names, CAPABILITIES, "{}: column order", row.lang);
            for c in &row.cells {
                assert!(
                    matches!(c.state, STATE_YES | STATE_NO | STATE_PARTIAL),
                    "{}/{}: unknown state {:?}",
                    row.lang,
                    c.capability,
                    c.state
                );
                assert_eq!(
                    c.reason.is_none(),
                    c.state == STATE_YES,
                    "{}/{}: every non-yes cell explains itself, and a yes has nothing to \
                     explain",
                    row.lang,
                    c.capability
                );
            }
        }
    }

    /// Spot-checks that the grid says what the daemon actually does — the
    /// three shapes a reader must be able to tell apart.
    #[test]
    fn the_grid_reports_the_three_real_shapes() {
        let grid = parity_grid();
        let find = |lang: &str, cap: &str| -> ParityCell {
            grid.rows
                .iter()
                .find(|r| r.lang == lang)
                .unwrap_or_else(|| panic!("{lang} row"))
                .cells
                .iter()
                .find(|c| c.capability == cap)
                .unwrap_or_else(|| panic!("{lang}/{cap}"))
                .clone()
        };
        // A full-tier, locals-proven language: yes everywhere except the
        // outline contract that does not exist yet.
        assert_eq!(find("rust", "highlight").state, STATE_YES);
        assert_eq!(find("rust", "symbols").state, STATE_YES);
        assert_eq!(find("rust", "outline").state, STATE_PARTIAL);
        assert_eq!(find("rust", "usages").state, STATE_YES);
        // Token-level without a locals query: no exact tier, and it says so.
        assert_eq!(find("go", "usages").state, STATE_PARTIAL);
        assert_eq!(find("bash", "usages").state, STATE_PARTIAL);
        // Ruby's exact comes from the read-time STRICT lane, which is why
        // it has a vendored locals query.
        assert_eq!(find("ruby", "usages").state, STATE_YES);
        // A CST-outline data language: highlighted, key rows, no code lanes.
        assert_eq!(find("yaml", "highlight").state, STATE_YES);
        assert_eq!(find("yaml", "symbols").state, STATE_PARTIAL);
        assert_eq!(find("yaml", "usages").state, STATE_NO);
        assert_eq!(find("yaml", "lens").state, STATE_NO);
        // A parse-only grammar, and a type with no grammar at all: every
        // cell `no`, every `no` explained.
        for lang in ["erb", "sql", "dockerfile"] {
            for cap in CAPABILITIES {
                let c = find(lang, cap);
                assert_eq!(c.state, STATE_NO, "{lang}/{cap}");
                assert!(c.reason.is_some(), "{lang}/{cap} must say why");
            }
        }
    }

    /// The CI gate. A capability change is a golden change — reviewable in
    /// the diff that causes it, never a silent widening or rot.
    #[test]
    fn the_parity_grid_matches_the_checked_in_golden() {
        let actual = serde_json::to_string_pretty(&parity_grid()).expect("serialize");
        assert_eq!(
            actual.trim_end(),
            PARITY_GOLDEN.trim_end(),
            "the derived Parity Grid differs from tests/fixtures/parity.golden.json.\n\
             If the capability change is intended, replace the golden with the JSON \
             below (this is exactly `kb-code parity --json`):\n{actual}"
        );
    }

    // ── the declaration↔handler walk ─────────────────────────────────────

    #[test]
    fn every_declared_v72_h1_route_is_registered_and_requires_its_params() {
        assert!(!V72_H1_ROUTES.is_empty());
        for c in V72_H1_ROUTES {
            let nested = c
                .path
                .strip_prefix("/api")
                .expect("every route path is /api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: declared but never registered in router.rs",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{}: registered path but no {} handler named in router.rs",
                c.path,
                c.handler
            );
            assert!((c.params_accept_without)(""));
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: declares {p:?} required but its params struct accepts a request \
                     without it",
                    c.path
                );
            }
        }
    }
}
