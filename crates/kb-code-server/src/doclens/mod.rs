//! DCB W1.C — the doc↔code lens: resolve ONE kb document's `coderef/1`
//! extraction against ONE named checkout and answer `codelens/1`.
//!
//! ## What this module is
//!
//! kb extracts HINTS (`carts_controller.rb:284`, `Algolia::SearchService#
//! listable_results`, `acme/shopfront#15357`) out of a document's prose. It
//! has no working tree and no symbol table, so it is structurally unable to
//! say whether any of them is real — that is the DCB invariant. This module
//! is the other half: it takes those hints, resolves them against a repo
//! this daemon actually mirrors, and returns a compound
//! `path_state`/`line_state`/`symbol_state` verdict **computed fresh per
//! request and never cached or stored**.
//!
//! ## Honesty rules that are load-bearing here
//!
//! * **No fuzzy anything.** Path resolution is exact-path or
//!   `/`-anchored-suffix matching over the live `files` table only — never
//!   `search::files`' nucleo+frecency lane, whose ranking is
//!   non-deterministic and carries no cardinality. `present`/`ambiguous`/
//!   `absent` are CARDINALITIES, not rankings. The fuzzy lane appears in the
//!   whole design exactly once, as a `/search?q=` deep LINK on the
//!   >3-candidate and 0-candidate arms — a link, never a verdict.
//! * **doclens owns its own symbol vocabulary.** `symbol_state ∈ hit_unique |
//!   hit_container_matched | hit_ambiguous | no_symbol`; this module never
//!   emits `crate::resolve`'s `exact`/`likely`/`candidate` trust classes and
//!   never imports `CLASS_*`. A doc reference carries strictly less evidence
//!   than a repo-unique name match. Pinned by
//!   `doclens_never_emits_a_resolve_trust_class`.
//! * **A bare line number is `unverifiable` BY DEFINITION.** `confirmed`
//!   requires a context-derived token occurring UNIQUELY within ±3 lines of
//!   the citation; a unique hit within ±64 is `drifted` with a signed
//!   `line_hint_delta`. Zero tokens, or an ambiguous hit, is honestly
//!   unverifiable — never optimistically confirmed.
//! * **`confirmed` never MOVES the reader.** The ±3 window is a tolerance on
//!   the EVIDENCE, not a correction to the citation, so a confirmed ref keeps
//!   `resolved_line = line_hint` and reports the matched line separately as
//!   `token_line` (D6). Only `drifted` (and W2.A's `rev_remap`) move the line.
//! * **`never_scanned` is a third state**, distinct from "zero refs": kb has
//!   no extraction row for this doc at all. Every consumer renders "not
//!   scanned yet — reindex to populate", never "no code refs".
//! * **A checkout is NEVER auto-selected.** No `?repo=` and no usable pin ⇒
//!   `400 repo_required`, pointing at the scorecard. Silently picking a
//!   plausible checkout is the exact failure the pin exists to prevent.
//! * **No caching in v1.** Any future cache key must fingerprint the
//!   WORKTREE, never `head_sha` + `dirty: bool` — two different dirty trees
//!   share that pair.
//!
//! ## Doc text flows INTO this daemon (amendment 9)
//!
//! Resolving a lens pulls a kb document's extracted refs — including a
//! 200-byte `context` window of the author's own prose per ref — across the
//! process boundary into kb-code, using kb-code's FULL-CORPUS kb token. The
//! `[doclens] kbs` allowlist is the ONLY scope on that flow, and there is no
//! other one: an unbounded `?kb=` would turn this daemon into an unscoped
//! read-proxy into every corpus that token can reach. The gate therefore
//! lives INSIDE [`resolve::resolve_lens`] (R8) rather than in the HTTP
//! handler, so W3.A's background sync — which calls the same function
//! in-process — inherits it instead of re-implementing it.
//!
//! ## Route surface (and where each piece mounts)
//!
//! | Route | Router | CORS? | Ships in |
//! |---|---|---|---|
//! | `GET /api/doc-lens` | `doclens_read` | yes (`read_cors`) | W1.C |
//! | `GET /api/doc-lens/repos` | `doclens_read` | yes (`read_cors`) | W1.C |
//! | `PUT`/`DELETE /api/doc-lens/pin` | `doclens_pin` | yes (`pin_cors`) | W1.C |
//! | `GET /api/doc-lens/pins` | `doclens_read` | yes | W2.A |
//! | `GET /api/doc-lens/path` | `doclens_read` | yes (`read_cors`) | SL7e |
//! | `GET /api/doc-lens/resolve-path` | plain `api` | **no** | W2.B |
//! | `POST /api/doc-lens/sync` | `transcripts_api` | no (loopback-only) | W3.A |
//! | `GET /api/doc-refs` | plain `api` | **no** | W3.A |
//!
//! `router.rs`'s `cors_layer_route_set_is_pinned` probe asserts that table;
//! the two must be kept in lock-step.
//!
//! `GET /api/doc-lens/path` (SL7e) grew two OPTIONAL params in SL7f, still
//! the same route/CORS row above: `?repo=` (required only when this daemon
//! serves 2+ checkouts — `repo_required`, unchanged) and `?context=` (the
//! caller's own line text, ≤ [`PATH_LENS_CONTEXT_MAX_CHARS`] — see
//! [`PATH_LENS_NOTE`]).
//!
//! ## Error vocabulary (`{error, reason}`, R12)
//!
//! | `reason` | status | meaning |
//! |---|---|---|
//! | `doclens_disabled` | 403 | `[doclens] kbs` is empty — feature off here |
//! | `kb_not_allowlisted` | 403 | the kb exists but is not in `kbs` |
//! | `invalid_segment` | 400 | `kb`/`doc` failed the segment validators |
//! | `repo_required` | 400 | no `?repo=`, no usable pin — never auto-picked |
//! | `doc_not_found` | 404 | kb 404s the doc after the moves chain (pin dropped) |
//! | `repo_indexing` | 503 | the selected repo's boot walk has not landed |
//! | `kb_daemon_disabled` | 503 | `[kb_daemon] enabled = false` |
//! | `kb_unreachable` | 503 | transport failure talking to kb |
//! | `kb_forbidden` | 502 | kb answered 401/403 — this daemon's token is wrong |
//!
//! ## Known limit, recorded rather than hidden
//!
//! A repo whose boot walk is only PARTIALLY landed is not structurally
//! detectable (`file_count > 0` looks identical to "fully indexed"). Such a
//! repo degrades to extra `absent`s; the answer is the rendered
//! `resolved_unix` plus a manual refresh, not a fake confidence signal.

pub mod cors;
pub mod pins;
pub mod remap;
pub mod resolve;
pub mod sync;
pub mod wire;

use crate::routes::ApiError;

pub const SCHEMA: &str = "codelens/1";
pub const SCORECARD_SCHEMA: &str = "codelens-scorecard/1";
pub const PIN_SCHEMA: &str = "codelens-pin/1";
/// W2.A — `GET /api/doc-lens/pins`. Plural, and a DIFFERENT schema from
/// `PIN_SCHEMA`: one is a single remembered choice, the other is the
/// operator's whole pin ledger with liveness columns computed against the
/// running config.
pub const PINS_SCHEMA: &str = "codelens-pins/1";

/// SL7e (v0.42, slate D29) — `GET /api/doc-lens/path`. Its OWN schema
/// because it answers a strictly smaller question than `codelens/1`: one
/// caller-supplied repo path (± one line), no kb document, no refs array,
/// no symbol lane. Same vocabulary (`PathState`/`LineState`), same
/// per-request-never-persisted posture.
pub const PATH_LENS_SCHEMA: &str = "codelens-path/1";

/// Hard ceiling on refs resolved per lens. The real corpus tops out at a few
/// hundred `<code>` elements in one doc, of which only a fraction survive
/// extraction, so 400 is comfortable headroom AND a hard bound on the
/// file-read fan-out. Truncation is by `ordinal` ascending and sets
/// `truncated = true`; `counts.total` still reports the FULL feed count —
/// never a silent shrink.
pub const MAX_REFS_PER_LENS: usize = 400;

/// `line_state` windows (amendment 7). ±3 confirms; ±64 is the drift search
/// (observed real drift across the five-checkout study: 4–21 lines).
pub const CONFIRM_WINDOW_LINES: u32 = 3;
pub const DRIFT_WINDOW_LINES: u32 = 64;

/// Confirm-token extraction (amendment 7).
pub const MIN_TOKEN_LEN: usize = 4;
pub const MAX_CONFIRM_TOKENS: usize = 8;

/// Decision 3's tiering boundary: >3 candidates renders as a count + a search
/// deep-link instead of an inline list.
pub const AMBIGUITY_INLINE_MAX: usize = 3;

/// Cap on `symbol_hits[]` per ref (the count is always exact).
pub const MAX_SYMBOL_HITS: usize = 5;

/// Per-file byte cap for the line-confirmation read. `routes::read_repo_file`'s
/// working-tree branch is an UNCAPPED `std::fs::read` — the cap there applies
/// only to the git-blob branch. doclens reads its own way, capped: a lens must
/// never pull a 40 MB generated file into memory to check one line. Over-cap
/// files yield `line_state = unverifiable`, `reason = "file_too_large"`.
pub const LENS_FILE_READ_CAP: u64 = 2 * 1024 * 1024;

/// Bounded fan-out over configured repos for the scorecard (kb-server
/// invariant #28's ethos, via [`crate::fanout::buffered_join`] — V70-A1 —
/// one `spawn_blocking` per repo). Lower than `fanout::DEFAULT_FANOUT_CAP`
/// (8): each unit here is a file-read-per-ref scoring pass, not a single
/// cheap query, so 4 keeps the per-request `spawn_blocking` + file-read
/// pressure lighter.
pub const SCORECARD_FANOUT_CAP: usize = 4;

pub const KB_SEGMENT_MAX: usize = 64;
pub const DOC_SEGMENT_MAX: usize = 256;

/// Carried verbatim in every response — `crate::resolve::RESOLVE_NOTE`'s
/// honesty convention.
pub const LENS_NOTE: &str = "compound path/line/symbol state, computed FRESH \
    against the named checkout at resolved_unix and never cached or stored. \
    path_state comes from exact-path and exact path-suffix matching over the \
    live files table only — no fuzzy match, no frecency, so present/ambiguous/\
    absent are cardinalities, not rankings. line_state=confirmed means a \
    context-derived token occurs UNIQUELY within ±3 lines of the cited line; a \
    bare line number with no extractable token is unverifiable BY DEFINITION, \
    never confirmed. symbol_state is doc-lens's OWN vocabulary and never \
    borrows /api/resolve's exact|likely|candidate trust classes: a doc \
    reference carries strictly less evidence than a repo-wide-unique name \
    match. When dirty=true every verdict is against an uncommitted working \
    tree that no sha reproduces. When the document declares a kb-code-rev \
    naming this checkout and is not +dirty, line_state=confirmed additionally \
    means git itself mapped the cited line across the interval \
    (line_evidence=rev_remap) and resolved_line is the mapped line; a +dirty \
    rev is never remapped, because the cited numbers were counted against an \
    uncommitted tree that no sha reproduces.";

/// `codelens-path/1`'s own note (SL7e; the `context=` wiring lands in
/// SL7f). Shorter than [`LENS_NOTE`] because the route answers less — and it
/// says the ONE thing a caption consumer must not get wrong: with no `?context=`
/// there is no confirm token, so a line verdict is `unverifiable` BY
/// DEFINITION (the same rule `LENS_NOTE` states for a bare line number —
/// never optimistically confirmed). An optional `context=` (the caller's own
/// line text, capped at [`PATH_LENS_CONTEXT_MAX_CHARS`]) is run through the
/// SAME [`resolve::confirm_tokens`] a document's own prose would use, making
/// `confirmed`/`drifted` reachable — still `unverifiable` when the context
/// yields no usable token. `file_lines` is reported so a caller can say "the
/// file has N lines" for itself.
pub const PATH_LENS_NOTE: &str = "one repo path resolved against the live \
    files table of the named checkout, computed FRESH and never cached or \
    stored. path_state comes from exact-path and exact path-suffix matching \
    only — no fuzzy match, no frecency, so present/ambiguous/absent are \
    cardinalities, not rankings. line_state is null when no line was asked \
    (the path is the whole claim) and null when the path did not resolve; \
    with a line and no ?context= there are no confirm tokens, so the verdict \
    is unverifiable BY DEFINITION — never confirmed. An optional ?context= \
    (the caller's own line text) is run through the same confirm-token \
    extraction a document's own prose would use, so confirmed/drifted are \
    reachable when the context yields a usable token — still unverifiable \
    when it does not. file_lines is the file's real length so a caller can \
    judge an out-of-range line itself.";

/// Cap on `?context=`'s length (SL7f, v0.42 amendment): the board sends one
/// post's line text, never a whole file — 2,000 chars is generous headroom
/// over any real line while keeping the prose-token scan
/// ([`resolve::confirm_tokens`]'s fallback arm) cheap. Over-cap input is
/// truncated (`chars().take`), never rejected — the same honest-degrade
/// posture as [`LENS_FILE_READ_CAP`], not a new error surface for an
/// advisory field that only ever narrows a verdict from `unverifiable`.
pub const PATH_LENS_CONTEXT_MAX_CHARS: usize = 2_000;

/// Lowercase generic words that are never evidence. Matched against the
/// LOWERCASED token, so compound identifiers (`RecommendService`,
/// `algolia_user_token`) survive while bare `Service`/`params` do not. The
/// uniqueness requirement does most of the precision work; this list only
/// keeps the token list short and the failure explainable.
///
/// Only reachable through [`resolve::confirm_tokens`]' FALLBACK arm — a ref
/// carrying the producer's own `context_tokens` never touches it (§18.1).
pub const TOKEN_STOPLIST: &[&str] = &[
    // language keywords / ubiquitous constructs
    "class",
    "module",
    "self",
    "nil",
    "null",
    "none",
    "true",
    "false",
    "return",
    "yield",
    "then",
    "else",
    "elsif",
    "when",
    "case",
    "each",
    "func",
    "void",
    "this",
    "super",
    "const",
    "static",
    "async",
    "await",
    "catch",
    "throw",
    "raise",
    "begin",
    "ensure",
    "import",
    "export",
    "require",
    "include",
    "extend",
    "public",
    "private",
    "protected",
    // ubiquitous nouns
    "file",
    "files",
    "line",
    "lines",
    "path",
    "paths",
    "code",
    "name",
    "names",
    "type",
    "types",
    "value",
    "values",
    "data",
    "index",
    "order",
    "list",
    "item",
    "items",
    "params",
    "param",
    "args",
    "string",
    "integer",
    "array",
    "hash",
    "object",
    "method",
    "function",
    "field",
    "fields",
    "error",
    "errors",
    "result",
    "results",
    "status",
    "request",
    "response",
    "config",
    "options",
    "token",
    "user",
    "query",
    "test",
    "tests",
    "spec",
    "specs",
    "view",
    "views",
    "model",
    "models",
    "service",
    "services",
    "controller",
    "controllers",
    "component",
    "components",
    "helper",
    "helpers",
    "concern",
    "concerns",
    "app",
    "lib",
    "src",
    "bin",
    "doc",
    "docs",
    // Italian prose function words the motivating corpus is dense with
    "anche",
    "questo",
    "questa",
    "questi",
    "queste",
    "quindi",
    "quando",
    "quello",
    "quella",
    "viene",
    "vengono",
    "essere",
    "della",
    "dello",
    "delle",
    "degli",
    "nella",
    "nello",
    "nelle",
    "sono",
    "come",
    "solo",
    "tutti",
    "tutte",
    "dove",
    "perche",
    "senza",
    "dopo",
    "prima",
    "ogni",
    "oggi",
    "stesso",
    "stessa",
    "ancora",
    "invece",
    "mentre",
    // English prose function words
    "that",
    "this",
    "with",
    "from",
    "into",
    "only",
    "also",
    "must",
    "need",
    "have",
    "been",
    "when",
    "where",
    "which",
    "while",
    "should",
    "would",
    "could",
    "after",
    "before",
    "here",
    "there",
    "them",
    "they",
    "what",
    "than",
    "then",
    "some",
    "same",
    "more",
];

/// The `[doclens]` gate + segment validation EVERY doc-lens surface shares.
///
/// It takes the config SECTION rather than the whole `SharedState`
/// deliberately: this is the security-relevant half of
/// [`resolve::resolve_lens`] (R8 — it must scope W3.A's background pull, not
/// just the routes), and a section-shaped signature makes it directly
/// unit-testable, so "the gate lives in the engine" is pinned by a test
/// rather than by convention.
pub fn gate(cfg: &crate::config::DoclensSection, kb: &str, doc: &str) -> Result<(), ApiError> {
    validate_kb_segment(kb)?;
    validate_doc_segment(doc)?;
    gate_kb_only(cfg, kb)
}

/// The kb-only half of [`gate`] — `[doclens] kbs` enabled + allowlist, with
/// NO doc-segment validation. W2.B's `GET /api/doc-lens/resolve-path` ramp
/// (`wire::resolve_path_route`) is the one caller whose second parameter is a
/// source-relative PATH, not a doc segment (it legitimately contains `/`), so
/// it calls [`validate_kb_segment`] + this fn directly rather than [`gate`] —
/// same enabled/allowlist behavior, same error shapes, no duplicated FORBIDDEN
/// construction.
pub fn gate_kb_only(cfg: &crate::config::DoclensSection, kb: &str) -> Result<(), ApiError> {
    if !cfg.enabled() {
        return Err(ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            "the doc-lens feature is off on this daemon ([doclens] kbs is empty)",
        )
        .with_reason("doclens_disabled"));
    }
    if !cfg.kb_allowed(kb) {
        return Err(ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            format!("kb {kb:?} is not in [doclens] kbs"),
        )
        .with_reason("kb_not_allowlisted"));
    }
    Ok(())
}

/// `kb`: 1..=64 bytes of `[A-Za-z0-9_-]`.
pub fn validate_kb_segment(kb: &str) -> Result<&str, ApiError> {
    if kb.is_empty() || kb.len() > KB_SEGMENT_MAX {
        return Err(ApiError::bad_request_with_reason(
            format!("invalid kb segment {kb:?}: 1..={KB_SEGMENT_MAX} bytes"),
            "invalid_segment",
        ));
    }
    if !kb
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ApiError::bad_request_with_reason(
            format!("invalid kb segment {kb:?}: only [A-Za-z0-9_-] allowed"),
            "invalid_segment",
        ));
    }
    Ok(kb)
}

/// True when any `/`-split segment of `path` is `.` or `..` (DCB-W2.B.R fix
/// 1 — a security finding, not a style nit): `path` ultimately becomes a URL
/// PATH segment (`KbClient::resolve_doc_by_path` interpolates it into
/// `.../docs/by-path/{encoded_path}`), and reqwest's WHATWG `Url::parse`
/// normalizes dot-segments out of a URL's path BEFORE the request ever
/// leaves this process — empirically, `path=../../../research/docs/by-path/a.html`
/// collapses the URL down past `/api/kb/{kb}` and re-targets a DIFFERENT
/// kb entirely, bypassing the `[doclens] kbs` allowlist gate that already
/// ran against the ORIGINAL `?kb=` (the allowlist check is correct; the
/// `path` value is what smuggles the escape). `encode_uri_component` cannot
/// close this on its own — `.` is in its unreserved set (a literal `.` in a
/// real filename must round-trip), so `..` survives encoding unescaped.
///
/// Checked at the route ([`super::wire::resolve_path_route`]) AND,
/// belt-and-braces, inside [`crate::join::kb_client::KbClient::
/// resolve_doc_by_path`] itself — the route is the only production caller
/// today, but a future direct caller of the client must not reopen the hole.
pub(crate) fn path_has_dot_segment(path: &str) -> bool {
    path.split('/').any(|seg| seg == "." || seg == "..")
}

/// `doc`: 1..=256 bytes of `[A-Za-z0-9._-]`. Rejects `/`, `..`, `%`, control
/// chars and every URL-significant byte, so `format!(".../docs/{doc}/code-refs")`
/// in `kb_client` can never mint a path outside the intended one.
///
/// STRICT on purpose (R2): `doc` is the kb ARTIFACT ID on every DCB surface —
/// an opaque token, never a source-relative path — so no caller ever needs a
/// `/` here. A consumer holding only a path resolves it FIRST through kb's own
/// `GET /api/kb/{kb}/docs/by-path/{*path}` and then calls doclens with the id.
/// Relaxing this would put the pin PK, the `doc_refs` PK and the moves-301
/// re-key on two different keys.
pub fn validate_doc_segment(doc: &str) -> Result<&str, ApiError> {
    if doc.is_empty() || doc.len() > DOC_SEGMENT_MAX {
        return Err(ApiError::bad_request_with_reason(
            format!("invalid doc segment {doc:?}: 1..={DOC_SEGMENT_MAX} bytes"),
            "invalid_segment",
        ));
    }
    if !doc
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        return Err(ApiError::bad_request_with_reason(
            format!(
                "invalid doc segment {doc:?}: only [A-Za-z0-9._-] allowed \
                 (the doc key is kb's artifact id, not a source-relative path)"
            ),
            "invalid_segment",
        ));
    }
    if doc.contains("..") {
        return Err(ApiError::bad_request_with_reason(
            format!("invalid doc segment {doc:?}: contains \"..\""),
            "invalid_segment",
        ));
    }
    Ok(doc)
}

/// `{public_base}/a/{kb}/{doc_path}` — kb's path-form artifact permalink
/// (kb invariant #34's shell route). `public_base` is
/// `state.kb_daemon.public_base()` (`public_url` if set, else `url`; the
/// hosted shape MUST set `public_url`, since `url` is a container hostname no
/// browser can resolve). `None` when the base is empty or `doc_path` is.
///
/// Encoding is PER SEGMENT: `doc_path.split('/').map(encode).join("/")`,
/// mirroring `web/src/lib/artifactHref.ts` byte-for-byte — the `/`s stay
/// literal separators, everything else is escaped. Built here, ONCE, because
/// three consumers wanted their own builder and three encoders drift on
/// exactly this rule (R1).
pub(crate) fn doc_href(public_base: &str, kb: &str, doc_path: &str) -> Option<String> {
    let base = public_base.trim().trim_end_matches('/');
    if base.is_empty() || doc_path.is_empty() {
        return None;
    }
    let path = doc_path
        .split('/')
        .map(encode_uri_component)
        .collect::<Vec<_>>()
        .join("/");
    Some(format!("{base}/a/{}/{path}", encode_uri_component(kb)))
}

/// JavaScript `encodeURIComponent`'s exact unreserved set
/// (`A-Za-z0-9 - _ . ! ~ * ' ( )`); everything else is percent-encoded
/// byte-wise, uppercase hex. Hand-rolled rather than pulling in
/// `percent-encoding`: one rule, one place, no new dependency, and the SPA's
/// own builder is the thing it must agree with byte-for-byte.
///
/// `pub(crate)` (not private) so `join::kb_client::KbClient::
/// resolve_doc_by_path` (W2.B) reuses this SAME encoder when building the
/// outbound `by-path/{*path}` URL to kb — one percent-encoding
/// implementation, not a second hand-rolled copy in `join`.
pub(crate) fn encode_uri_component(s: &str) -> String {
    const UNRESERVED_EXTRA: &[u8] = b"-_.!~*'()";
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || UNRESERVED_EXTRA.contains(b) {
            out.push(*b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DoclensSection;

    #[test]
    fn validate_kb_segment_rejects_slash_dotdot_and_percent() {
        assert!(validate_kb_segment("platform").is_ok());
        assert!(validate_kb_segment("kb-docs_2").is_ok());
        for bad in ["", "a/b", "..", "a%2Fb", "a b", "a?b", "a\u{0}b", "ünïcode"] {
            let err = validate_kb_segment(bad).unwrap_err();
            assert_eq!(err.reason(), Some("invalid_segment"), "kb {bad:?}");
        }
        assert!(validate_kb_segment(&"a".repeat(KB_SEGMENT_MAX)).is_ok());
        assert!(validate_kb_segment(&"a".repeat(KB_SEGMENT_MAX + 1)).is_err());
    }

    #[test]
    fn path_has_dot_segment_catches_every_dot_shape_but_not_a_real_dotted_filename() {
        for bad in [
            "..",
            ".",
            "../../../research/docs/by-path/a.html",
            "a/../b",
            "a/..",
            "../a",
            "a/./b",
        ] {
            assert!(path_has_dot_segment(bad), "{bad:?} must be flagged");
        }
        // A real filename containing dots (or a plain multi-segment path)
        // must NOT be flagged — this is a dot-SEGMENT check, not a
        // dot-byte check.
        for ok in ["a.html", "features/algolia/piano.html", "a.b.c/d.rs", ""] {
            assert!(!path_has_dot_segment(ok), "{ok:?} must not be flagged");
        }
    }

    #[test]
    fn validate_doc_segment_rejects_path_separators() {
        assert!(validate_doc_segment("9f8b7182d433").is_ok());
        assert!(validate_doc_segment("piano-di-lavoro.html").is_ok());
        // R2: a source-relative path must 400, never be silently accepted.
        let err = validate_doc_segment("features/algolia/piano.html").unwrap_err();
        assert_eq!(err.reason(), Some("invalid_segment"));
        for bad in ["", "..", "a..b", "a/b", "a%2e%2e", "a b", "a#b", "a?b"] {
            assert!(validate_doc_segment(bad).is_err(), "doc {bad:?}");
        }
    }

    /// R8 — the gate is a property of the ENGINE, not of the HTTP handler:
    /// W3.A's background sync calls `resolve_lens` in-process, so removing a
    /// kb from `[doclens] kbs` must stop that corpus's prose flowing into
    /// this daemon at all, not merely close the routes.
    #[test]
    fn the_kb_allowlist_gate_is_enforced_by_the_engine_not_the_handler() {
        let off = DoclensSection::default();
        assert_eq!(
            gate(&off, "platform", "9f8b7182d433").unwrap_err().reason(),
            Some("doclens_disabled")
        );
        let on = DoclensSection {
            kbs: vec!["platform".into()],
            ..DoclensSection::default()
        };
        assert!(gate(&on, "platform", "9f8b7182d433").is_ok());
        assert_eq!(
            gate(&on, "research", "9f8b7182d433").unwrap_err().reason(),
            Some("kb_not_allowlisted")
        );
        // Segment validation runs FIRST — a traversal attempt is refused
        // before the allowlist is even consulted.
        assert_eq!(
            gate(&on, "platform", "../etc/passwd").unwrap_err().reason(),
            Some("invalid_segment")
        );
        assert_eq!(
            gate(&off, "bad/kb", "x").unwrap_err().reason(),
            Some("invalid_segment")
        );
    }

    #[test]
    fn kb_allowlist_empty_means_the_feature_is_off() {
        let off = DoclensSection::default();
        assert!(!off.enabled());
        assert!(!off.kb_allowed("platform"));
        let on = DoclensSection {
            kbs: vec!["platform".into()],
            ..DoclensSection::default()
        };
        assert!(on.enabled());
        assert!(on.kb_allowed("platform"));
        assert!(!on.kb_allowed("research"));
    }

    #[test]
    fn doc_href_percent_encodes_each_segment_and_keeps_the_separators() {
        assert_eq!(
            doc_href(
                "https://kb.example.com",
                "platform",
                "features/a b/piano#1.html"
            )
            .as_deref(),
            Some("https://kb.example.com/a/platform/features/a%20b/piano%231.html")
        );
        // A trailing slash on the base never doubles up.
        assert_eq!(
            doc_href("https://kb.example.com/", "platform", "a.html").as_deref(),
            Some("https://kb.example.com/a/platform/a.html")
        );
    }

    #[test]
    fn doc_href_is_none_without_a_kb_public_base() {
        assert_eq!(doc_href("", "platform", "a.html"), None);
        assert_eq!(doc_href("   ", "platform", "a.html"), None);
        assert_eq!(doc_href("https://kb.example.com", "platform", ""), None);
    }

    #[test]
    fn doclens_section_defaults_are_the_hand_written_ones() {
        // The derived-`Default` trap (R6): a derived impl would give
        // deadline_ms = 0 (instantly expired) and batch_cap = 0.
        let d = DoclensSection::default();
        assert_eq!(d.deadline_ms, 3_000);
        assert_eq!(d.batch_cap, 200);
        assert_eq!(d.max_refs(), MAX_REFS_PER_LENS);
        assert!(d.kbs.is_empty() && d.origins.is_empty());
        assert_eq!(d.sync_interval_secs, 0);
        assert!(!d.sync_on_boot);
    }

    #[test]
    fn max_refs_override_is_clamped() {
        let mk = |n: usize| DoclensSection {
            max_refs: Some(n),
            ..DoclensSection::default()
        };
        assert_eq!(mk(0).max_refs(), 1);
        assert_eq!(mk(10).max_refs(), 10);
        assert_eq!(mk(99_999).max_refs(), 2_000);
    }

    /// Golden pin: a silent edit to either constant shows up in review.
    #[test]
    fn lens_note_and_token_stoplist_are_pinned() {
        insta::assert_snapshot!("lens_note", LENS_NOTE);
        insta::assert_snapshot!("token_stoplist", TOKEN_STOPLIST.join("\n"));
        // The stoplist is only ever consulted lowercased — a stray uppercase
        // entry would be dead weight that never matches.
        assert!(TOKEN_STOPLIST.iter().all(|w| w == &w.to_ascii_lowercase()));
    }
}
