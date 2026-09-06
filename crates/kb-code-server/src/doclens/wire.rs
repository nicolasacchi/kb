//! DCB W1.C — the `codelens/1` + `codelens-scorecard/1` wire types and the
//! two GET handlers.
//!
//! The handlers are deliberately THIN: `Query` → [`resolve::resolve_lens`] /
//! [`resolve::resolve_scorecard`] → `Json`. Everything a caller must inherit
//! (the `[doclens] kbs` gate, segment validation, the pin's 404-drop and
//! moves-301 re-key, the deadline) lives in the engine, so W3.A's in-process
//! sync gets all of it for free (R8).
//!
//! *(W2.B adds a THIRD handler here — `GET /api/doc-lens/resolve-path?kb=&path=`
//! — which mounts OUTSIDE the CORS set, on the plain `api` router. See this
//! module's parent doc for the full route table; `router.rs`'s
//! `cors_layer_route_set_is_pinned` asserts that route ABSENT from the
//! ACAO-carrying set.)*

use axum::{
    extract::{Query, State},
    http::header,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use super::remap::RevRemap;
use super::resolve::{
    self, kb_client_error, IssueRef, LineEvidence, LineState, PathState, SearchLink, SpanOutcome,
    SymbolHit, SymbolState,
};
use super::{gate_kb_only, path_has_dot_segment, validate_doc_segment, validate_kb_segment};
use crate::routes::ApiError;
use crate::state::SharedState;

/// `<meta name="kb-code-rev">` as `codelens/1` spells it. The INBOUND
/// `coderef/1` field is `label`; the OUTBOUND name is `repo_label` (W2.A's
/// remap consumes this shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeRevOut {
    pub repo_label: String,
    pub sha: String,
    pub dirty: bool,
}

/// W2.A — what the `kb-code-rev` remap did for this whole document, reported
/// ONCE. `null` when the doc declared no `kb-code-rev`.
///
/// `state ∈ applied | skipped | unavailable`;
/// `reason ∈ null | "doc_rev_dirty" | "rev_label_mismatch" | "rev_unknown"`
/// (the engine's fourth internal reason, `no_doc_rev`, is exactly the case
/// this whole block is `null` for).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevRemapOut {
    pub state: &'static str,
    pub reason: Option<&'static str>,
    pub repo_label: Option<String>,
    /// As WRITTEN in the doc (possibly abbreviated).
    pub sha: Option<String>,
    /// Full 40-hex, when it resolved in the selected checkout.
    pub resolved_sha: Option<String>,
    /// The DOC's `+dirty` marker — never the selected repo's own dirtiness
    /// (which `repo.dirty` already reports, and which means something else).
    pub dirty: bool,
    pub paths_mapped: usize,
    pub budget_exhausted: bool,
}

impl From<&RevRemap> for RevRemapOut {
    fn from(r: &RevRemap) -> Self {
        Self {
            state: r.state.as_str(),
            reason: r.reason,
            repo_label: r.repo_label.clone(),
            sha: r.sha.clone(),
            resolved_sha: r.resolved_sha.clone(),
            dirty: r.dirty,
            paths_mapped: r.paths_mapped(),
            budget_exhausted: r.budget_exhausted(),
        }
    }
}

/// The checkout this lens was computed against. `state ∈ ready | indexing |
/// error` is a per-REPO state and is NEVER a `path_state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoOut {
    pub name: String,
    pub root: String,
    pub state: &'static str,
    pub head_sha: Option<String>,
    pub head_branch: Option<String>,
    pub dirty: Option<bool>,
    /// `"param"` (an explicit `?repo=`) or `"pin"` — never "guessed".
    pub source: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LensCounts {
    /// The FULL feed count, even when `refs` was truncated.
    pub total: usize,
    /// How many refs this response actually resolved.
    pub resolved: usize,
    pub present: usize,
    pub ambiguous: usize,
    pub absent: usize,
    pub external: usize,
    pub confirmed: usize,
    pub drifted: usize,
    pub unverifiable: usize,
    pub line_absent: usize,
    pub declared_but_absent: usize,
}

/// One ref-bearing heading. `key == anchor` by construction on the producer
/// side; there is NO sentinel row for ungrouped refs — those carry a null
/// `group` FK and are counted by the envelope's `ungrouped_count` (R9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupOut {
    pub key: String,
    pub label: String,
    pub anchor: String,
    pub ordinal: u32,
    pub ref_count: usize,
}

/// Where a consumer should open the reader. Built from the RESOLUTION
/// (`resolved_path` + `resolved_line`), never from `path_hint`/`line_hint` —
/// the hint is what the prose said, the resolution is what this daemon
/// verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReaderTarget {
    pub repo: String,
    pub path: String,
    pub line: Option<u32>,
}

/// CT-F2 — "was this ref's citation true AT THE REV THE DOC DECLARED?",
/// additive alongside (never instead of) the ref's ever-present current-tree
/// verdict. Reuses `path_state`/`line_state`'s own vocabulary rather than
/// minting a fourth one — evaluated with NO remap involved (the doc's own
/// rev IS the coordinate space its line numbers were counted in). `null` on
/// the ref unless the response's `era == "declared"` (see
/// [`CodeLensOut::era`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WhenWrittenOut {
    pub path_state_at_rev: PathState,
    pub line_state_at_rev: LineState,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RefOut {
    pub ordinal: u32,
    pub group: Option<String>,
    pub kind: String,
    pub raw: String,
    pub declared: bool,

    // --- what the document said (hints) --------------------------------
    pub path_hint: Option<String>,
    pub line_hint: Option<u32>,
    pub line_hint_end: Option<u32>,
    pub symbol_container: Option<String>,
    pub symbol_member: Option<String>,
    pub context: Option<String>,

    // --- what this daemon verified -------------------------------------
    /// `null` for a ref with no `path_hint` (D5) and for `kind == "issue"`.
    pub path_state: Option<PathState>,
    pub resolved_path: Option<String>,
    pub candidate_count: usize,
    /// `[]` when `candidate_count > AMBIGUITY_INLINE_MAX` (the count stays
    /// exact — the list is replaced by `search`, not the number).
    pub candidates: Vec<String>,
    pub issue: Option<IssueRef>,

    pub line_state: LineState,
    pub line_evidence: LineEvidence,
    pub confirm_token: Option<String>,
    pub token_line: Option<u32>,
    pub resolved_line: Option<u32>,
    /// W2.A ranges (E6) — the MAPPED end of a `path:5-19` citation. `null`
    /// unless the ref is a range that `rev_remap` mapped end-to-end.
    pub resolved_line_end: Option<u32>,
    pub line_hint_delta: Option<i64>,
    /// `0` when no file was read at all — the deadline-cut and
    /// `no_line_hint`/`external`/`issue`/path-absent paths, which never
    /// touch a file. A `rev_remap` ref is NOT exempt: DCB-W2.A.R fix 2 bound-
    /// checks every mapped line/end against the working tree's real length
    /// via one memoised read per path, so a shipped `rev_remap` confirmation
    /// always carries the real count too (`resolved_line <= file_lines`).
    pub file_lines: u32,
    pub line_reason: Option<&'static str>,
    /// W2.A — this ref's remap outcome:
    /// `null | "applied" | "inside_change" | "path_absent_at_rev" |
    /// "budget_exhausted" | "diff_failed"`. `null` when the doc-level remap
    /// is not `applied`. On a multi-span ref this is the FIRST span's outcome
    /// (§9b.1); each span's own `line_evidence` says whether IT remapped.
    pub remap: Option<&'static str>,

    pub symbol_state: SymbolState,
    pub symbol_hit_count: usize,
    pub symbol_hits: Vec<SymbolHit>,

    /// Per-span line outcomes; `[]` for `path`/`symbol_*`/`issue`/`external`
    /// and for any ref whose path did not resolve. A `path_line` ref has
    /// exactly one span; the ref-level `line_*` fields are the rollup
    /// (weakest state wins; the FIRST span drives `reader.line`).
    pub spans: Vec<SpanOutcome>,

    pub reader: Option<ReaderTarget>,
    pub search: Option<SearchLink>,
    /// `"declared but absent"` | `"unusable path"` | `"unusable issue ref"`.
    pub note: Option<String>,
    /// CT-F2 — `null` unless the response's `era == "declared"` (an honest
    /// absence, never a guess): the doc carried no usable `kb-code-rev`, or
    /// the caller never asked via `?at=declared` in the first place.
    pub when_written: Option<WhenWrittenOut>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeLensOut {
    pub schema: &'static str,
    pub kb: String,
    pub doc_id: String,
    /// The id the caller ASKED for, when kb's moves chain answered a
    /// different one (the pin was re-keyed to `doc_id`).
    pub moved_from: Option<String>,
    pub doc_path: Option<String>,
    /// The ONE kb link-out, built server-side (`doclens::doc_href`); `null`
    /// when `[kb_daemon]` has no usable public base.
    pub doc_href: Option<String>,
    pub doc_hash: Option<String>,
    pub doc_title: Option<String>,
    pub doc_extracted_at: Option<i64>,
    pub doc_code_rev: Option<CodeRevOut>,
    /// W2.A — `null` when the doc declared no `kb-code-rev` (`doc_code_rev`
    /// is then null too) — but that is not the ONLY way to see `null` here:
    /// a repo that is `indexing` or `error` also short-circuits `rev_remap`
    /// to `null` even when `doc_code_rev` is `Some`, since nothing was
    /// resolved to report it against (`resolve_lens`'s blocking closure).
    pub rev_remap: Option<RevRemapOut>,
    /// kb has NO extraction row for this doc — the THIRD state, distinct
    /// from `refs: []`. Consumers must say so explicitly and NEVER render
    /// "no code refs" (the SPA says "code refs never scanned" plus a
    /// `kb reindex` remedy hint since CT-E3; the contract is the
    /// three-state distinction, not the exact wording).
    pub never_scanned: bool,
    pub repo: RepoOut,
    pub resolved_unix: i64,
    pub truncated: bool,
    pub partial: bool,
    pub partial_reason: Option<&'static str>,
    pub counts: LensCounts,
    pub ungrouped_count: u32,
    pub groups: Vec<GroupOut>,
    pub refs: Vec<RefOut>,
    /// CT-F2 — `"none" | "declared"` (a future `"session"` is a recorded
    /// follow-up, not yet built): whether every ref's `when_written` was
    /// actually computed against a declared rev this request. `"none"` both
    /// when the caller never passed `?at=declared` AND when they did but the
    /// doc had no usable `kb-code-rev` — the per-ref `null`s already say
    /// which, this field just spares a caller scanning `refs[]` to learn it.
    pub era: &'static str,
    pub note: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScorecardRepoOut {
    pub name: String,
    pub root: String,
    pub state: &'static str,
    pub head_sha: Option<String>,
    pub head_branch: Option<String>,
    pub dirty: Option<bool>,
    /// All four are `null` on a non-`ready` repo — an unscored repo reports
    /// nothing rather than a zero that reads like a verdict.
    pub present: Option<usize>,
    pub ambiguous: Option<usize>,
    pub absent: Option<usize>,
    pub external: Option<usize>,
    pub partial: bool,
    pub reason: Option<String>,
}

impl ScorecardRepoOut {
    pub(crate) fn blank(name: &str, root: &str, state: &'static str) -> Self {
        Self {
            name: name.to_string(),
            root: root.to_string(),
            state,
            head_sha: None,
            head_branch: None,
            dirty: None,
            present: None,
            ambiguous: None,
            absent: None,
            external: None,
            partial: false,
            reason: None,
        }
    }

    /// One broken repo must never sink the scorecard — it reports
    /// `state: "error"` + `reason` and the others still score.
    pub(crate) fn error(name: &str, root: &str, reason: String) -> Self {
        Self {
            reason: Some(reason),
            ..Self::blank(name, root, "error")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScorecardOut {
    pub schema: &'static str,
    pub kb: String,
    pub doc_id: String,
    pub doc_hash: Option<String>,
    pub doc_title: Option<String>,
    pub never_scanned: bool,
    pub resolved_unix: i64,
    /// What a reader pre-selects its repo picker with; `null` when the doc
    /// has no pin.
    pub pinned_repo: Option<String>,
    pub counted_refs: usize,
    pub truncated: bool,
    /// `state.repos` order (config order) — the same submission-order
    /// determinism kb-server invariant #28 requires.
    pub repos: Vec<ScorecardRepoOut>,
    pub note: &'static str,
}

// --- handlers --------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LensParams {
    pub kb: String,
    /// kb's ARTIFACT ID (D14) — never a source-relative path.
    pub doc: String,
    pub repo: Option<String>,
    /// CT-F2, opt-in — `?at=declared` additionally resolves each ref's
    /// `when_written` against the doc's declared `kb-code-rev` (never
    /// persisted; per-request only, same DCB posture as everything else
    /// here). Any other value (including absent) is the byte-identical
    /// pre-CT-F2 response.
    pub at: Option<String>,
}

/// `GET /api/doc-lens?kb=&doc=&repo=&at=`.
pub async fn doc_lens_route(
    State(state): State<SharedState>,
    Query(p): Query<LensParams>,
) -> Result<impl IntoResponse, ApiError> {
    let at_declared = p.at.as_deref() == Some("declared");
    let out = resolve::resolve_lens(&state, &p.kb, &p.doc, p.repo.as_deref(), at_declared).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[derive(Debug, Deserialize)]
pub struct ScorecardParams {
    pub kb: String,
    pub doc: String,
}

/// `GET /api/doc-lens/repos?kb=&doc=`.
pub async fn doc_lens_repos_route(
    State(state): State<SharedState>,
    Query(p): Query<ScorecardParams>,
) -> Result<impl IntoResponse, ApiError> {
    let out = resolve::resolve_scorecard(&state, &p.kb, &p.doc).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[derive(Debug, Deserialize)]
pub struct ResolvePathParams {
    pub kb: String,
    /// A source-relative PATH, percent-decoded once by axum's `Query`
    /// extractor — deliberately NOT run through [`validate_doc_segment`]
    /// (which rejects `/`): the whole point of this ramp is that the caller
    /// holds a path, not an artifact id.
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ResolvePathOut {
    pub doc_id: String,
}

/// `GET /api/doc-lens/resolve-path?kb=&path=` (W2.B, R2/R20) — the
/// PATH-addressed lens entry ramp's server side: resolves a source-relative
/// path to kb's artifact id via kb's own by-path lookup
/// (`KbClient::resolve_doc_by_path`), so `LensEntryByPath.tsx` never needs a
/// browser-side cross-origin call into kb (kb's own CORS layer is
/// loopback-origin-only — see this module's parent doc / R20).
///
/// Mounts on the PLAIN `auth_bearer` `api` router (`router.rs`) — same-origin
/// only, deliberately NOT `doclens_read`: a corpus-wide path→id existence
/// oracle behind an exact-origin ACAO + Allow-Credentials is a surface no
/// consumer asked for (web-code is this daemon's own frontend, always
/// same-origin to it). `cors_layer_route_set_is_pinned`
/// (`tests/doclens/doclens_route.rs`) asserts this route carries no ACAO.
///
/// Same `[doclens] kbs` gate as the sibling doc-lens routes (kb segment
/// validation, then enabled + allowlist via [`gate_kb_only`]) — `path` is
/// NOT run through [`validate_doc_segment`] (it legitimately contains `/`);
/// the RESULT (`doc_id`) is what gets validated as a doc segment before
/// being handed back, so a caller of this route can never receive an id
/// shape a later `?doc=` call would itself refuse.
pub async fn resolve_path_route(
    State(state): State<SharedState>,
    Query(p): Query<ResolvePathParams>,
) -> Result<impl IntoResponse, ApiError> {
    validate_kb_segment(&p.kb)?;
    gate_kb_only(&state.doclens, &p.kb)?;
    if p.path.trim().is_empty() {
        return Err(ApiError::bad_request_with_reason(
            "path must not be empty",
            "invalid_segment",
        ));
    }
    // DCB-W2.B.R fix 1 (security) — reject a dot-segment BEFORE it ever
    // reaches `KbClient::resolve_doc_by_path` — see `path_has_dot_segment`'s
    // doc for the exact traversal this closes (a `..`-laden `path` value
    // that reqwest's URL parser normalizes past the `?kb=` allowlist gate
    // already enforced above).
    if path_has_dot_segment(&p.path) {
        return Err(ApiError::bad_request_with_reason(
            format!(
                "invalid path segment in {:?}: \".\"/\"..\" are not allowed",
                p.path
            ),
            "invalid_segment",
        ));
    }

    let resolved = state
        .kb_client
        .resolve_doc_by_path(&p.kb, &p.path)
        .await
        .map_err(kb_client_error)?;
    let Some(doc_id) = resolved else {
        return Err(
            ApiError::not_found(format!("kb {:?} has no doc at path {:?}", p.kb, p.path))
                .with_reason("doc_not_found"),
        );
    };
    // Belt-and-suspenders: a caller of THIS route must never receive an id
    // shape doc-lens's own `?doc=` would itself refuse on the very next
    // request — kb's by-path lookup is not this daemon's to validate, so an
    // id it returns that fails our own strict segment rule is treated as an
    // upstream contract violation, not silently handed through.
    if validate_doc_segment(&doc_id).is_err() {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            format!("kb returned an unusable doc id for path {:?}", p.path),
        )
        .with_reason("kb_upstream_error"));
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(ResolvePathOut { doc_id }),
    ))
}

// --- SL7e (v0.42, slate D29) — the path-addressed lens ---------------------

#[derive(Debug, Deserialize)]
pub struct PathLensParams {
    /// The `[doclens] kbs` allowlist gate's subject, and nothing else — this
    /// route makes NO call into kb (see [`resolve::resolve_path_lens`]). It
    /// is required anyway so the feature switches off per-corpus exactly
    /// like every sibling doc-lens read, rather than becoming an unscoped
    /// path oracle the moment a caller omits it.
    pub kb: String,
    /// A REPO-relative path, percent-decoded once by axum's `Query`
    /// extractor. Not a kb source path and not a doc segment (it
    /// legitimately contains `/`).
    pub path: String,
    /// One line to ask about. Absent ⇒ the path is the whole question and
    /// `line_state` comes back null.
    pub line: Option<u32>,
    /// Optional; a daemon serving exactly one checkout needs no opinion.
    pub repo: Option<String>,
    /// SL7f (v0.42 amendment): the caller's own line text (the slate post's
    /// cited line), fed to [`resolve::confirm_tokens`] exactly as a
    /// document's `context` would be — the ONLY way this route can mint
    /// `confirmed`/`drifted` rather than `unverifiable` for a `?line=`.
    /// Absent ⇒ byte-identical to SL7e (no confirm tokens, `unverifiable`).
    /// Truncated at [`super::PATH_LENS_CONTEXT_MAX_CHARS`] chars, never
    /// rejected — see [`super::PATH_LENS_NOTE`].
    pub context: Option<String>,
}

/// `codelens-path/1`. Deliberately NOT a trimmed `CodeLensOut`: that shape
/// is a document's whole ref feed (groups, counts, truncation, symbols,
/// `when_written`), none of which a single path has, and reusing it would
/// mean a wire full of structurally-empty arrays a consumer has to learn to
/// ignore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathLensOut {
    pub schema: &'static str,
    /// The checkout that answered — always named, never inferred by the
    /// caller from the fact that it got a 200.
    pub repo: String,
    /// Echoed back exactly as asked (never the normalised or resolved form,
    /// which are their own fields).
    pub path: String,
    pub line_hint: Option<u32>,
    /// `null` only for a hint this daemon refuses to interpret as a path at
    /// all; `external` is unreachable here (it is the producer's own
    /// classification of a gem/vendor ref, and there is no producer).
    pub path_state: Option<PathState>,
    /// Why a non-obvious `path_state` came out that way — `"unusable path"`
    /// for a hint this daemon refuses to read as a path at all, `null`
    /// otherwise. `resolve_path`'s own note field, not a second vocabulary.
    pub path_note: Option<String>,
    pub resolved_path: Option<String>,
    /// Exact on every tier (the `ambiguous` list itself is not carried —
    /// `/api/doc-lens` renders candidates, a caption counts them).
    pub candidate_count: usize,
    /// `null` = no line verdict (none asked, or the path did not resolve) —
    /// the same "null is the fifth case" rule `path_state` follows for a ref
    /// with no path hint. NEVER `absent` as a stand-in for "not asked".
    pub line_state: Option<LineState>,
    /// `no_line_hint` · `path_not_present` · `no_token` · `file_too_large` ·
    /// `unreadable` · `not_text` — the reason vocabulary `LineOutcome`
    /// already uses.
    pub line_reason: Option<&'static str>,
    pub resolved_line: Option<u32>,
    /// The file's real length, so a caller can decide for itself that a
    /// cited line is out of range (this daemon reports `unverifiable`, not
    /// `absent`, for an unconfirmable line — see [`super::PATH_LENS_NOTE`]).
    pub file_lines: Option<u32>,
    pub resolved_unix: i64,
    pub note: &'static str,
}

/// `GET /api/doc-lens/path?kb=&path=[&line=][&repo=][&context=]` (SL7e —
/// kb's slate board captions `found`/`tried` cards with this, D29; SL7f adds
/// `?context=`, wiring `confirmed`/`drifted` for a `?line=`).
///
/// Mounts on `doclens_read`, the CORS'd set, with the same `auth_bearer`
/// posture as `/api/doc-lens`: kb's SPA is cross-origin to this daemon and
/// the caption is a read of exactly the information class the doc lens
/// already serves that origin — a `present`/`absent` verdict about a path
/// the caller already named. It is NOT `resolve-path`'s corpus-wide
/// existence oracle (that one enumerates kb DOCUMENTS from a path, and
/// stays same-origin).
///
/// `404` is never used for a file that is not there — that is
/// `path_state: "absent"`, a verdict, at 200. A 404 here means the `?repo=`
/// named does not exist.
pub async fn path_lens_route(
    State(state): State<SharedState>,
    Query(p): Query<PathLensParams>,
) -> Result<impl IntoResponse, ApiError> {
    let out = resolve::resolve_path_lens(
        &state,
        &p.kb,
        &p.path,
        p.line,
        p.repo.as_deref(),
        p.context.as_deref(),
    )
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ref(symbol_state: SymbolState) -> RefOut {
        RefOut {
            ordinal: 1,
            group: None,
            kind: "symbol_method".into(),
            raw: "Algolia::SearchService#listable_results".into(),
            declared: false,
            path_hint: None,
            line_hint: None,
            line_hint_end: None,
            symbol_container: Some("Algolia::SearchService".into()),
            symbol_member: Some("listable_results".into()),
            context: Some("il servizio di ricerca".into()),
            path_state: Some(PathState::Present),
            resolved_path: Some("app/services/algolia/search_service.rb".into()),
            candidate_count: 1,
            candidates: vec!["app/services/algolia/search_service.rb".into()],
            issue: None,
            line_state: LineState::Confirmed,
            line_evidence: LineEvidence::ContextToken,
            confirm_token: Some("listable_results".into()),
            token_line: Some(3),
            resolved_line: Some(3),
            resolved_line_end: None,
            line_hint_delta: Some(0),
            file_lines: 15,
            line_reason: None,
            remap: Some("applied"),
            symbol_state,
            symbol_hit_count: 1,
            symbol_hits: vec![SymbolHit {
                path: "app/services/algolia/search_service.rb".into(),
                line_start: 3,
                line_end: 5,
                kind: "method".into(),
                container: Some("SearchService".into()),
            }],
            spans: Vec::new(),
            reader: None,
            search: None,
            note: None,
            when_written: None,
        }
    }

    /// A full response envelope around one [`sample_ref`] — [G2] widens
    /// `doclens_never_emits_a_resolve_trust_class` from a bare `RefOut` to
    /// this, so the forbidden-literal scan below covers `repo.state`
    /// (a DIFFERENT `ready|indexing|error` vocabulary that must not
    /// accidentally collide), `symbol_hits`, and both `note` surfaces (the
    /// per-ref `Option<String>` AND the fixed `CodeLensOut::note` constant)
    /// — not just the slice `sample_ref` alone exercises.
    fn sample_lens(symbol_state: SymbolState) -> CodeLensOut {
        let mut with_note = sample_ref(symbol_state);
        with_note.note = Some("unusable path".to_string());
        CodeLensOut {
            schema: "codelens/1",
            kb: "platform".into(),
            doc_id: "9f8b7182d433".into(),
            moved_from: None,
            doc_path: Some("docs/checkout.html".into()),
            doc_href: Some("https://kb.example.com/a/platform/docs/checkout.html".into()),
            doc_hash: Some("deadbeef".into()),
            doc_title: Some("Checkout flow".into()),
            doc_extracted_at: Some(1_754_500_000),
            doc_code_rev: Some(CodeRevOut {
                repo_label: "alpha".into(),
                sha: "abc123".into(),
                dirty: false,
            }),
            rev_remap: Some(RevRemapOut {
                state: "applied",
                reason: None,
                repo_label: Some("alpha".into()),
                sha: Some("abc123".into()),
                resolved_sha: Some("abc123def456".into()),
                dirty: false,
                paths_mapped: 1,
                budget_exhausted: false,
            }),
            never_scanned: false,
            repo: RepoOut {
                name: "alpha".into(),
                root: "/repos/alpha".into(),
                state: "ready",
                head_sha: Some("abc123".into()),
                head_branch: Some("main".into()),
                dirty: Some(false),
                source: "pin",
            },
            resolved_unix: 1_754_500_100,
            truncated: false,
            partial: false,
            partial_reason: None,
            counts: LensCounts::default(),
            ungrouped_count: 1,
            groups: vec![GroupOut {
                key: "checkout".into(),
                label: "Checkout".into(),
                anchor: "checkout".into(),
                ordinal: 0,
                ref_count: 1,
            }],
            refs: vec![with_note, sample_ref(symbol_state)],
            era: "none",
            note: super::super::LENS_NOTE,
        }
    }

    /// Amendment 6's mandated test (R7's ratified name). doc-lens has its OWN
    /// symbol vocabulary and must never emit `/api/resolve`'s trust classes —
    /// a doc reference carries strictly less evidence than a repo-unique name
    /// match, and borrowing the word `exact` for it would be the exact
    /// "wrong-exact optics" failure the whole design exists to avoid.
    // invariant:2 doclens never emits a resolve trust class (exact/likely/candidate)
    #[test]
    fn doclens_never_emits_a_resolve_trust_class() {
        let all_four = [
            SymbolState::HitUnique,
            SymbolState::HitContainerMatched,
            SymbolState::HitAmbiguous,
            SymbolState::NoSymbol,
        ];
        for st in all_four {
            // [G2] The FULL envelope, not just one `RefOut` — see
            // `sample_lens`'s doc for what that additionally covers.
            let json = serde_json::to_string(&sample_lens(st)).unwrap();
            for forbidden in ["\"exact\"", "\"likely\"", "\"candidate\""] {
                assert!(
                    !json.contains(forbidden),
                    "codelens/1 must never emit {forbidden}: {json}"
                );
            }
        }
        // And the vocabulary really is the four documented values.
        let rendered: Vec<String> = all_four
            .iter()
            .map(|s| serde_json::to_string(s).unwrap())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "\"hit_unique\"",
                "\"hit_container_matched\"",
                "\"hit_ambiguous\"",
                "\"no_symbol\""
            ]
        );
    }

    #[test]
    fn path_line_and_symbol_states_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&PathState::External).unwrap(),
            "\"external\""
        );
        assert_eq!(
            serde_json::to_string(&LineState::Unverifiable).unwrap(),
            "\"unverifiable\""
        );
        assert_eq!(
            serde_json::to_string(&LineEvidence::ContextToken).unwrap(),
            "\"context_token\""
        );
        // W2.A's fourth tier exists on the wire vocabulary from day one so
        // consumers key on `line_evidence`, not on `line_state` alone.
        assert_eq!(
            serde_json::to_string(&LineEvidence::RevRemap).unwrap(),
            "\"rev_remap\""
        );
    }

    #[test]
    fn a_null_path_state_serializes_as_null_not_a_fifth_string() {
        let mut r = sample_ref(SymbolState::NoSymbol);
        r.path_state = None;
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert!(v["path_state"].is_null());
        // `indexing` is a per-REPO scorecard state and must never leak here.
        assert!(!serde_json::to_string(&r).unwrap().contains("indexing"));
    }

    /// W2.A's two additions are strictly ADDITIVE to the frozen `codelens/1`
    /// (§7): a W1.C-era client keeps working and simply renders no
    /// git-verified tier.
    #[test]
    fn rev_remap_block_and_per_ref_remap_field_are_the_frozen_shape() {
        let v = serde_json::to_value(sample_lens(SymbolState::HitUnique)).unwrap();
        assert_eq!(v["rev_remap"]["state"], "applied");
        assert!(v["rev_remap"]["reason"].is_null());
        assert_eq!(v["rev_remap"]["repo_label"], "alpha");
        assert_eq!(v["rev_remap"]["paths_mapped"], 1);
        assert_eq!(v["rev_remap"]["budget_exhausted"], false);
        // The doc's `+dirty` marker is a DIFFERENT fact from the selected
        // repo's own dirtiness — both are on the wire, neither aliases.
        assert_eq!(v["rev_remap"]["dirty"], false);
        assert_eq!(v["repo"]["dirty"], false);
        assert_eq!(v["refs"][0]["remap"], "applied");
        assert!(v["refs"][0]["resolved_line_end"].is_null());

        // A doc with no `kb-code-rev` carries neither block (§7).
        let mut bare = sample_lens(SymbolState::HitUnique);
        bare.doc_code_rev = None;
        bare.rev_remap = None;
        for r in &mut bare.refs {
            r.remap = None;
        }
        let v = serde_json::to_value(&bare).unwrap();
        assert!(v["rev_remap"].is_null() && v["doc_code_rev"].is_null());
        assert!(v["refs"][0]["remap"].is_null());
    }

    /// CT-F2 — additive on top of the already-additive W2.A shape: a
    /// pre-CT-F2 client keeps working (`era` defaults to `"none"`,
    /// `when_written` defaults to `null`), and a client that DID opt in via
    /// `?at=declared` sees the compound `{path_state_at_rev, line_state_at_rev}`
    /// block using the SAME `path_state`/`line_state` vocabulary as the
    /// current-tree fields, never a new one.
    #[test]
    fn when_written_and_era_are_additive_and_reuse_the_existing_vocabulary() {
        let bare = sample_lens(SymbolState::HitUnique);
        assert_eq!(bare.era, "none");
        let v = serde_json::to_value(&bare).unwrap();
        assert_eq!(v["era"], "none");
        assert!(v["refs"][0]["when_written"].is_null());

        let mut declared = sample_lens(SymbolState::HitUnique);
        declared.era = "declared";
        declared.refs[0].when_written = Some(WhenWrittenOut {
            path_state_at_rev: PathState::Present,
            line_state_at_rev: LineState::Drifted,
        });
        let v = serde_json::to_value(&declared).unwrap();
        assert_eq!(v["era"], "declared");
        assert_eq!(v["refs"][0]["when_written"]["path_state_at_rev"], "present");
        assert_eq!(v["refs"][0]["when_written"]["line_state_at_rev"], "drifted");
    }

    #[test]
    fn scorecard_row_for_an_unready_repo_reports_null_columns_not_zeroes() {
        let row = ScorecardRepoOut::blank("demo-repo", "/srv/demo-repo", "indexing");
        let v = serde_json::to_value(&row).unwrap();
        for k in [
            "present",
            "ambiguous",
            "absent",
            "external",
            "head_sha",
            "dirty",
        ] {
            assert!(v[k].is_null(), "{k} must be null on a non-ready repo");
        }
        assert_eq!(v["state"], "indexing");
        let err = ScorecardRepoOut::error("kb", "/home/user/project/kb", "root vanished".into());
        assert_eq!(err.state, "error");
        assert_eq!(err.reason.as_deref(), Some("root vanished"));
    }
}
