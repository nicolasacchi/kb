// Hand-written wire types mirroring `crates/kb-code-server/src/routes.rs`'s
// JSON response shapes. kb-code has no ts-rs export pipeline yet (unlike
// kb's own `web/src/api/generated/` — see `just types` in the root
// justfile) — these are typed by hand against the Rust `Serialize` structs
// (field-for-field, same names: serde's default `snake_case` matches these
// structs' own field names already, so no rename mapping is needed). If
// kb-code ever grows a `ts-export` feature, these can be swapped for
// generated bindings without changing any call site (same migration path
// kb's own `web/src/api/client.ts` describes in its header comment).

export interface HeadInfo {
  detached: boolean;
  unborn: boolean;
  branch: string | null;
  sha: string | null;
}

export type WatcherState = "watching" | "polling" | "gated";

export interface RepoListEntry {
  name: string;
  path: string;
  file_count: number;
  symbol_count: number;
  head: HeadInfo | null;
  watcher: WatcherState;
}

export interface ReposResponse {
  repos: RepoListEntry[];
}

export type EntryKind = "file" | "dir" | "symlink" | "submodule";

// ── kbc-tree/1 (V71-F1) ──────────────────────────────────────────────────
//
// `GET /api/tree/2` — the PROJECTED, decorated tree. The projection is
// computed once, server-side (see the crate's `tree` module doc); these
// types are the SPA's view of rows it did not compute, and nothing in
// `web-code` re-derives a count, an aggregate or a rank from them.

export type TreeView = "physical" | "role" | "namespace" | "change";
export type TreeRowKind = "dir" | "file" | "group" | "entity";
export type TreeLane = "git" | "review" | "findings" | "annot" | "todo" | "bookmark";

export interface TreeFacts {
  git?: string;
  git_changed?: number;
  review?: string;
  review_viewed?: number;
  review_total?: number;
  findings?: string;
  annot?: number;
  todo?: number;
  bookmark?: number;
}

export interface TreeRow {
  id: string;
  kind: TreeRowKind;
  label: string;
  depth: number;
  path?: string;
  ent?: string;
  /// `exact` | `likely` | `candidate` — present ONLY on an inferred row.
  trust?: string;
  children: number;
  files: number;
  has_more?: boolean;
  /// UTF-16 `[start, end)` ranges into `label`, from the daemon's ONE
  /// matcher. Never re-derived here (invariant 16b).
  match_ranges?: [number, number][];
  match_count?: number;
  facts?: TreeFacts;
}

export interface TreeTruncation {
  by: string;
  returned: number;
  total?: number;
  reason: string;
}

export interface TreeV2Response {
  schema: string;
  repo: string;
  view: TreeView;
  views_available: TreeView[];
  generation: number;
  root?: string;
  depth: number;
  scope?: string;
  scope_applied: boolean;
  filter?: string;
  mode: string;
  decorate: string[];
  base?: string;
  counts: { files: number; rows: number; matched?: number };
  rows: TreeRow[];
  unplaced: TreeRow[];
  unplaced_total: number;
  truncated?: TreeTruncation | null;
  notes: string[];
  diagnostics: { severity: string; token: string; message: string; suggestion?: string }[];
}

export interface TreeEntry {
  name: string;
  kind: EntryKind;
  size: number | null;
  oid: string;
}

export interface TreeResponse {
  repo: string;
  path: string;
  ref: string;
  entries: TreeEntry[];
}

export interface Symbol {
  ordinal: number;
  name: string;
  /// e.g. "function" | "method" | "class" | "struct" | "key" (YAML/TOML/
  /// JSON key-paths) | … — the fixed vocabulary lives server-side
  /// (`extract::map_kind` / `yaml`/`keypath` modules); the SPA treats it as
  /// an opaque label for grouping/icon lookup, never exhaustively matches it.
  kind: string;
  line_start: number;
  line_end: number;
  col_start: number;
  col_end: number;
  container: string | null;
  signature: string | null;
  /// `Some` for the four token-level languages when a doc comment directly
  /// precedes the symbol; `null` otherwise (no grammar coverage, or none
  /// found) — `extract::Symbol.doc`, sent unconditionally (no
  /// `skip_serializing_if`, unlike `trailers`/`files` elsewhere in this
  /// file). Added here (rather than left off `Symbol`) because `DefHit`
  /// below (B1) flattens this interface wholesale and must mirror the wire
  /// field-for-field.
  doc: string | null;
}

// Mirrors `highlight::HighlightClass`'s `#[serde(rename_all = "snake_case")]`
// — one CSS class per bucket (`styles/reader.css` `.kbc-hl-*`).
export type HighlightClass =
  | "keyword"
  | "string"
  | "comment"
  | "function"
  | "type"
  | "number"
  | "variable"
  | "constant"
  | "operator"
  | "punctuation"
  | "property"
  | "attribute"
  | "label"
  | "escape"
  | "other";

export interface Span {
  byte_start: number;
  byte_len: number;
  class: HighlightClass;
}

export type FileEncoding = "utf8" | "base64";

export interface FileResponse {
  repo: string;
  path: string;
  ref: string | null;
  size: number;
  blob_hash: string;
  lang: string | null;
  encoding: FileEncoding;
  content: string;
  symbols: Symbol[];
  highlights: Span[] | null;
}

export type RefKind = "branch" | "tag";

export interface RefInfo {
  name: string;
  full_name: string;
  kind: RefKind;
  target_sha: string;
  is_head: boolean;
}

export interface RefsResponse {
  repo: string;
  refs: RefInfo[];
}

export interface DiffResponse {
  repo: string;
  path: string;
  from: string;
  to: string | null;
  diff: string;
}

export interface ApiErrorBody {
  error: string;
}

/// `routes::IdentityResponse` (`GET /api/identity`) — trimmed to the fields
/// the SPA actually reads (`repos`/`started_at` have no client today; add
/// them here if/when a caller needs them, same convention as this file's
/// header comment). `kb_public_url` is `KbDaemonSection::public_base()`
/// (`[kb_daemon] public_url` if the operator set one, else the federation
/// `url`) — the sole boot-time consumer is `hooks/useIdentity.ts`, which
/// feeds it to `lib/searchLanes.ts`'s `setKbSessionBase`.
export interface IdentityOut {
  name: string;
  version: string;
  /// Optional on the wire: daemons built before 0f111aec (config-aware kb
  /// link base) don't send it at all — the SPA must degrade to its default
  /// session base, never crash at boot (a3 regression fix; the e2e harness
  /// caught the unguarded read white-screening the whole app).
  kb_public_url?: string;
}

// --- Search-Everywhere (W4.3) ---------------------------------------------
//
// Mirrors `crates/kb-code-server/src/search/{files,symbols,text,unified}.rs`
// + `semantic/store.rs` + `transcripts/search.rs`'s hit shapes, field-for-
// field, same convention as every other type in this file (see the header
// comment). `LaneSection.results` stays `unknown` — its element shape
// differs per lane (the server's own `LaneSection.results` is a bare
// `serde_json::Value` for exactly this reason, see `search/unified.rs`'s
// doc) — callers narrow it with the matching per-lane hit type via
// `section.lane`.

export type SearchLane = "files" | "symbols" | "text" | "semantic" | "sessions" | "transcripts";

/// `search::matcher::MatchTier` — the HARD ordering tier a hit sits in.
/// Every `exact` sorts above every `prefix`, which sorts above every
/// `fuzzy`; a score (and every ranking factor) only ever orders WITHIN one
/// tier, which is what stops a learned signal displacing the file or symbol
/// the operator named exactly.
export type MatchTier = "exact" | "prefix" | "fuzzy";

/// `search::ExplainFactor` — one applied ranking factor. `kind` says HOW it
/// combined, so a reader can reproduce the arithmetic instead of trusting
/// the total. A factor whose flag is off is ABSENT, never present-and-neutral.
export interface ExplainFactor {
  name: string;
  kind: "delta" | "multiplier";
  value: number;
  why: string;
}

/// `search::Explain` — one hit's ranking decomposition, present iff the
/// query carried `explain:1`.
export interface HitExplain {
  lane: string;
  /** 1-based position within this lane's section. */
  rank: number;
  tier: MatchTier;
  base: number;
  factors: ExplainFactor[];
  final_score: number;
}

/// `search::files::FileHit`. V71-D1 adds `tier`/`ranges`/`hit_id`/`explain`
/// — all additive: an older daemon simply omits them.
export interface FileHit {
  repo: string;
  path: string;
  score: number;
  /** Absent on a `recent` (empty-query) hit — there is no needle. */
  tier?: MatchTier;
  /** UTF-16 `[start, end)` offsets of the matched characters in `path`. */
  ranges?: [number, number][];
  hit_id?: string;
  explain?: HitExplain;
  /**
   * V71-D2 — the blob this hit was indexed from. Absent on a `recent`
   * (empty-query) row, which comes out of open history and makes no claim
   * about content.
   */
  blob_sha?: string;
}

/// `search::symbols::SymbolHit` — `#[serde(flatten)]` on the Rust side
/// splices `Symbol`'s own fields (see `Symbol` above) directly into this
/// object, so this interface repeats them rather than nesting a `symbol`
/// key.
export interface SymbolHit {
  repo: string;
  path: string;
  score: number;
  tier?: MatchTier;
  /**
   * UTF-16 `[start, end)` offsets into the `Container::name` HAYSTACK the
   * daemon matched against — NOT into `path` and NOT into `name` alone. Use
   * `lib/matchRanges.ts`'s `sliceRanges` to re-base them onto whichever part
   * a row actually renders.
   */
  ranges?: [number, number][];
  hit_id?: string;
  explain?: HitExplain;
  ordinal: number;
  name: string;
  kind: string;
  line_start: number;
  line_end: number;
  col_start: number;
  col_end: number;
  container: string | null;
  signature: string | null;
}

/// `search::text::TextMatch` — `byte_range` is a Rust `(usize, usize)`
/// tuple, serialized as a two-element JSON array.
export interface TextMatch {
  line_no: number;
  line: string;
  byte_range: [number, number];
}

/// `search::text::TextFileResult` — one row per FILE (a file can carry
/// several matches); `lib/searchRows.ts`'s `flattenTextRows` turns this
/// into the box's own one-row-per-match shape.
export interface TextFileResult {
  path: string;
  matches: TextMatch[];
}

/// `semantic::store::ChunkHit`.
export interface ChunkHit {
  repo: string;
  path: string;
  span_start: number;
  span_end: number;
  score: number;
  snippet: string;
}

/// `search::sessions::SessionHit`.
export interface SessionHit {
  session_id: string;
  title: string;
  score: number;
  started_at: number;
  /// Always `"kb digests"` — see that module's doc.
  source: string;
  files_changed?: unknown;
}

/// `transcripts::search::TranscriptHit` — carries NO file/path field today;
/// see `lib/searchLanes.ts`'s `transcriptReaderPath` for how the SPA stays
/// forward-compatible with a future server field without fabricating one
/// now.
export interface TranscriptHit {
  session_id: string;
  uuid: string;
  ts: number;
  kind: string;
  tool_name: string | null;
  project_dir: string;
  snippet: string;
  is_sidechain: boolean;
}

/// `GET /api/search/files`'s body (`routes::search_files`) — the files
/// lane's OWN standalone endpoint (distinct from the unified `GET
/// /api/search`). `q: ""` returns `FileIndex::recent`'s opened-at-ordered
/// list (see `api/client.ts`'s `fetchSearchFiles` doc) rather than a fuzzy/
/// frecency blend — Home's "Recent files" card section (F4) is the sole
/// caller today.
export interface SearchFilesResponse {
  q: string;
  hits: FileHit[];
}

/// `search::unified::LaneExplain` — the SECTION-level half of `explain:1`.
/// It states the unusual, honest thing: this box does not fuse. Sections are
/// fixed and never interleaved, so a hit's `rank` is a position within ONE
/// lane and there is no cross-lane additive score to show.
export interface LaneExplain {
  rank_basis: string;
  fusion: string;
  factors_on: string[];
  note: string;
}

/// `search::unified::LaneSection`.
export interface LaneSection {
  lane: SearchLane;
  results: unknown;
  truncated: boolean;
  unavailable_reason?: string;
  pending?: boolean;
  explain?: LaneExplain;
  /**
   * V71-D2 — present iff the query carried a `group:` other than `none`.
   * A PARTITION of `results` addressed by POSITION; absent (not empty)
   * when ungrouped, so "no grouping asked for" is distinguishable from
   * "grouped into nothing".
   */
  groups?: HitGroup[];
}

/// `search::results::HitGroup` (V71-D2). `indices` is the primary address:
/// four of the six lanes carry no stable hit id, so `hit_ids` is present
/// only where one exists (files/symbols) and is never fabricated.
export interface HitGroup {
  key: string;
  /** Never empty — the server labels the "this lane has no such key" case. */
  label: string;
  count: number;
  indices: number[];
  hit_ids?: string[];
}

/// `search::results::FacetValue` (V71-D2). `clause` is the kbcq/1 token that
/// narrows the query to this value — a facet WRITES the query; it is never
/// hidden client state.
export interface FacetValue {
  value: string;
  count: number;
  clause: string;
}

/// `search::results::FacetGroup`. `writes` says HOW `clause` is applied:
/// `"clause"` appends it, `"prefix"` replaces the query's lane prefix
/// (kbcq/1 selects a lane by prefix, so there is no `lane:` key).
export interface FacetGroup {
  field: string;
  label: string;
  writes: "clause" | "prefix";
  values: FacetValue[];
  omitted?: number;
}

/// `search::results::Facets`. `basis` is always `"page"` — the counts are
/// over the hits this response returned, never a corpus estimate.
export interface Facets {
  basis: string;
  note: string;
  groups: FacetGroup[];
}

/// `search::unified::Freshness` (V71-D2) — the LLM contract's staleness
/// half. `generation` is the daemon's monotonic index counter, NOT a commit
/// distance; the per-hit half is `FileHit.blob_sha`.
export interface Freshness {
  generation: number;
  as_of: string;
}

/// `search::unified::UnifiedSearchResponse` — `GET /api/search`'s body.
export interface UnifiedSearchResponse {
  sections: LaneSection[];
  query_echo: string;
  /**
   * kbcq/1's canonical re-rendering of the query that actually RAN
   * (`grammar::normalize`) — pasteable back into the box. Empty for the
   * empty-`q` recents short-circuit, which parses nothing.
   */
  normalized?: string;
  /**
   * Non-fatal parse notes (`grammar::Diagnostic`): unknown filter, bad
   * value, unsupported negation. A query with diagnostics still RAN — the
   * offending token was searched as an ordinary word.
   */
  diagnostics?: { severity: string; token: string; message: string; suggestion?: string }[];
  /** V71-D2 — present iff the query carried `facets:1`. */
  facets?: Facets;
  /** V71-D2 — always present on a current daemon; optional so an older one degrades. */
  stale?: Freshness;
}

/// `routes::search_semantic`'s body — `GET /api/search/semantic`. The
/// staged follow-up `useOmniSearch` fires when a unified response's
/// semantic section comes back `pending: true` (see that hook's doc).
export interface SemanticSearchResponse {
  q: string;
  repo: string | null;
  hits: ChunkHit[];
}

// --- Blame + provenance (W4.4) ---------------------------------------------
//
// Mirrors `crates/kb-code-server/src/blame/{mod,incremental,timeline}.rs` +
// `join/ladder.rs` + `provenance/{why,story}.rs`, field-for-field (see the
// header comment).

/// `blame::incremental::BlameRegion`.
export interface BlameRegion {
  sha: string;
  orig_start: number;
  final_start: number;
  count: number;
  author: string;
  author_mail: string;
  author_time: number;
  subject: string;
  previous_sha: string | null;
  previous_filename: string | null;
  filename: string;
  boundary: boolean;
}

/// git's own 40-zero "not yet committed" blame sentinel
/// (`provenance::UNCOMMITTED_SHA`) — a region carrying this sha has no
/// commit to join through; `why`'s uncommitted-live path answers it from
/// the local transcripts index instead (see `AttributionOut`'s `via`).
export const UNCOMMITTED_SHA = "0000000000000000000000000000000000000000";

/// `GET /api/blame`'s body (`routes::blame`).
export interface BlameResponse {
  repo: string;
  path: string;
  ref: string;
  dirty: boolean;
  cached: boolean;
  regions: BlameRegion[];
  truncated: boolean;
}

/// `blame::timeline::TimelineEntry`.
export interface TimelineEntry {
  sha: string;
  author_time: number;
  subject: string;
}

/// `GET /api/blame/timeline`'s body (`routes::blame_timeline`).
export interface BlameTimelineResponse {
  repo: string;
  path: string;
  line: number;
  max: number;
  entries: TimelineEntry[];
}

export type Confidence = "trailer" | "exact" | "fuzzy" | "none";

/// `provenance::why::AttributionOut` — the line-grade `/api/why` query's
/// attribution. `via: "uncommitted-live"` is the one case with no
/// `session_id` but a (possibly empty) `session_ids` list instead — see the
/// module doc on `provenance::why`.
export interface AttributionOut {
  confidence: Confidence;
  via: string;
  session_id?: string;
  session_ids?: string[];
  display_name?: string;
  /// The kb corpus the resolved session lives in — absent when neither the
  /// join ladder nor the (loopback-only) enrichment follow-up resolved one.
  /// Scopes an "open session in kb" link with `?kb=`.
  kb?: string;
}

/// `join::kb_client::WhyDecision`.
export interface WhyDecision {
  kind: string;
  prompt?: string;
  answer?: string;
}

/// `provenance::why::KbContextOut`.
export interface KbContextOut {
  decisions: WhyDecision[];
  prompt_excerpt?: string;
}

/// `provenance::why::RegionOut`.
export interface RegionOut {
  sha: string;
  subject: string;
  author: string;
  author_time: number;
}

/// `provenance::why::LineWhyOut` — `GET /api/why?line=`'s body.
export interface LineWhyOut {
  line: number;
  region: RegionOut;
  attribution: AttributionOut;
  /// Lance artifact ids the resolved session also wrote — display-only ids
  /// to link out to, never fetched memory bodies. Absent/empty when no
  /// session resolved, the enrichment follow-up didn't run (non-loopback),
  /// or it found none.
  session_memory_ids?: string[];
  kb_context?: KbContextOut;
  timeline_available: boolean;
}

/// `provenance::why::FileSessionOut`.
export interface FileSessionOut {
  session_id?: string;
  display_name?: string;
  confidence: Confidence;
  via: string;
  lines: number;
  regions: number;
}

/// `provenance::why::FileWhyOut` — `GET /api/why` (no `line`)'s body.
export interface FileWhyOut {
  repo: string;
  path: string;
  sessions: FileSessionOut[];
  uncommitted_lines: number;
}

/// `provenance::story::StoryEntry` — one beat of `GET /api/story`'s
/// chronological (oldest-first) session timeline. `status: "gap"` is
/// CT-E2's attention-gap beat: a run of CONSECUTIVE commits the join
/// ladder resolved NO session for, collapsed SERVER-side into one beat
/// carrying `commit_count` + the `first_seen`..`last_seen` author-time
/// range (one beat per run, never per commit). Every non-gap entry carries
/// a `session_id`; a single-commit gap additionally keeps its
/// `sha`/`subject`, a multi-commit gap carries neither.
export interface StoryEntry {
  session_id?: string;
  sha?: string;
  display_name?: string;
  subject?: string;
  confidence: Confidence;
  via: string;
  first_seen: number;
  /// Gap beats only — the LATEST author-time in the collapsed run.
  last_seen?: number;
  lines_touched: number;
  status: "owns-lines" | "historical" | "gap";
  /// Gap beats only — how many session-less commits the beat collapses.
  commit_count?: number;
  /// Gap beats only — the honesty split: `"no-captured-session"` (kb
  /// answered, nothing matched — definitely uncaptured) vs
  /// `"join-unavailable"` (kb unreachable/disabled — coverage UNKNOWN,
  /// not known-absent). See `provenance::story`'s module doc.
  reason?: "no-captured-session" | "join-unavailable";
}

/// `provenance::story::StoryOut` — `GET /api/story`'s body.
export interface StoryOut {
  repo: string;
  path: string;
  symbol?: string;
  entries: StoryEntry[];
}

// --- Session diff (W3.5 / W4.4) --------------------------------------------
//
// Mirrors `crates/kb-code-server/src/sessiondiff/mod.rs`, field-for-field.

export interface CommitFileOut {
  path: string;
  insertions: number;
  deletions: number;
  binary: boolean;
}

export interface CommitEntryOut {
  sha: string;
  repo?: string;
  subject?: string;
  author?: string;
  /// `#[serde(default, skip_serializing_if = "Vec::is_empty")]` server-side —
  /// an empty vec is OMITTED from the wire, not sent as `[]` (true for both
  /// diffed AND unresolved commits, whenever there happen to be no
  /// trailers/files); every reader must default with `?? []`.
  trailers?: string[];
  diffed: boolean;
  author_time?: number;
  /// Same `skip_serializing_if = "Vec::is_empty"` contract as `trailers` —
  /// see that field's comment. `commit_entry_unresolved` (server-side)
  /// always omits this key; a diffed commit with zero changed files omits
  /// it too.
  files?: CommitFileOut[];
  insertions: number;
  deletions: number;
}

export interface UncommittedTurnOut {
  ts: number;
  tool_name: string;
  file_paths: string[];
  uuid: string;
  src_file: string;
  byte_offset: number;
  byte_len: number;
}

/// `sessiondiff::Segment` — internally tagged on `kind`
/// (`"prompt" | "commits" | "uncommitted"`).
export type Segment =
  | { kind: "prompt"; ts: number; uuid: string; text: string }
  | { kind: "commits"; commits: CommitEntryOut[] }
  | { kind: "uncommitted"; files: string[]; turns: UncommittedTurnOut[] };

/// `sessiondiff::CommitsStatus` — internally tagged on `status`.
export type CommitsStatus = { status: "ok" } | { status: "degraded"; reason: string };

export interface SessionDiffTotals {
  commits: number;
  commits_diffed: number;
  files: number;
  insertions: number;
  deletions: number;
}

/// `GET /api/session-diff`'s body (`sessiondiff::SessionDiff`,
/// `session-diff/1`). Loopback-only (`router.rs`'s transcripts sub-router)
/// — raw transcript prompt text travels in `Segment`'s `"prompt"` variant.
export interface SessionDiff {
  version: string;
  session_id: string;
  display_name?: string;
  segments: Segment[];
  repos_touched: string[];
  totals: SessionDiffTotals;
  commits_status: CommitsStatus;
}

// --- Annotations (W4.6; Phase D adds anchor kinds/threads/intents) --------
//
// Mirrors `crates/kb-code-server/src/routes.rs`'s `AnnotationView` +
// `kb_core::review::Anchor` (reused wholesale server-side — the SPA treats
// it as opaque, round-tripping it through PATCH/DELETE without ever
// constructing one itself: the anchor is always server-built, from a POST's
// `line` + the file's own current content, never client-supplied).
export type Anchor = Record<string, unknown>;

/// `crate::annotations::ANCHOR_KINDS` — kept as a plain `string` on
/// `AnnotationView` itself (an unrecognized value from an older/newer
/// daemon must never crash a render, only degrade to the "line" rendering —
/// same open-vocabulary posture `ResolveCandidate.precision` documents
/// above), this alias is for CALLERS constructing/comparing a KNOWN kind.
//
// ── PRR-U4 — `"review"` added: `annotations::ANCHOR_KIND_REVIEW`, the
// path-less review-level "general question" kind (PRR-R3 design
// arbitration #6, `routes::assemble_top_level_annotation`'s dedicated
// branch) — the Ask-the-agent card's own POST needs to type-check this
// value. Purely additive; no existing caller constructs or switches
// exhaustively on `AnchorKind` today (only `AnnotationsPanel.tsx`/`lib/
// annotations.ts` assign one of the original four), so widening the union
// changes nothing for them. ──
// ── V70-A10 — `"set"` added: `annotations::ANCHOR_KIND_SET`, the
// path-less WORKSPACE-level general-note kind (the exact same shape as
// `"review"` above, one level down) — `WorkspaceNotesPanel`'s composer
// needs to type-check this value for a note with no code selection. ──
export type AnchorKind = "line" | "range" | "symbol" | "diff" | "review" | "set";

/// `crate::annotations::INTENTS` — same "open string on the wire, closed
/// alias for callers" posture as `AnchorKind` above.
export type AnnotationIntent = "note" | "question" | "todo" | "flag-for-agent" | "tour-stop";

export interface AnnotationView {
  id: string;
  repo: string;
  path: string;
  /// `null` only for a REPLY (`parent_id` set) — a reply has no anchor of
  /// its own, see `routes::annotation_view`'s doc.
  anchor: Anchor | null;
  /// `"line"` (the v1 default) | `"range"` | `"symbol"` | `"diff"` —
  /// always `"line"` for a reply (the row's own stored kind, unused).
  anchor_kind: string;
  /// `"note"` (default) | `"question"` | `"todo"` | `"flag-for-agent"` |
  /// `"tour-stop"`.
  intent: string;
  /// `Some` marks this row as a REPLY nested one level under the
  /// referenced (non-reply) annotation.
  parent_id: string | null;
  body: string;
  author: string;
  created_at: number;
  updated_at: number;
  resolved: boolean;
  line: number;
  stale: boolean;
  /// `range` only — the inclusive end line, already normalized so
  /// `line <= line_end` (`crate::annotations::normalize_range`).
  line_end: number | null;
  /// `diff` only — the full commit sha this annotation is pinned to.
  sha: string | null;
  /// V70-A10 — present only on a workspace-scoped row (`reading_sets.id`).
  /// `undefined` (the server omits the key, `skip_serializing_if`) for
  /// every pre-V0029 / non-workspace annotation.
  set_id?: string | null;
}

export interface AnnotationsListResponse {
  repo: string;
  path: string;
  annotations: AnnotationView[];
}

/// V70-A10 — `GET /api/annotations?set_id=`'s body (`routes::
/// list_annotations`'s second query shape). Every annotation scoped to
/// that workspace, general path-less notes AND code-anchored comments
/// alike (each still carrying its OWN `path`, `""` for a general note) —
/// group/thread client-side (`lib/workspaceNotes.ts`).
export interface WorkspaceNotesResponse {
  set_id: string;
  annotations: AnnotationView[];
}

/// `routes::OpenAnnotationEntry` — `#[serde(flatten)]` on the Rust side
/// splices `AnnotationView`'s own fields directly into this object (same
/// `#[serde(flatten)]` convention `SymbolHit`/`DefHit` use elsewhere in this
/// file), plus the one extra field: the annotation's own direct reply
/// count (replies themselves are never listed by `GET /api/annotations/
/// open` — see that route's doc).
export interface OpenAnnotationEntry extends AnnotationView {
  reply_count: number;
}

/// `GET /api/annotations/open`'s body (`routes::list_open_annotations`) —
/// every unresolved, top-level annotation across the WHOLE repo, newest
/// first, optionally filtered to one `intent`/`path_prefix`.
export interface OpenAnnotationsResponse {
  repo: string;
  intent: string | null;
  path_prefix: string | null;
  annotations: OpenAnnotationEntry[];
  truncated: boolean;
}

// --- Checkout (W4.7) --------------------------------------------------------

export interface CheckoutResponse {
  repo: string;
  ref: string;
  detached: boolean;
}

/// `POST /api/checkout`'s 409 body (`routes::checkout_route`) — a dirty
/// working tree is not a plain `{"error": ...}` `ApiError`, see that
/// route's doc.
export interface CheckoutDirtyBody {
  error: string;
  repo: string;
  dirty_paths: string[];
}

// --- Defs + xrefs (B1 — tier-0 clickable code) ------------------------------
//
// Mirrors `crates/kb-code-server/src/agentview/xref.rs`, field-for-field (see
// the header comment). Both endpoints are deliberately TAGS-TIER (a
// tree-sitter symbol-table lookup / a plain text-grep, never a real
// reference-graph or scope resolution) — `approximate`/`note`/
// `time_budget_exceeded` carry that honesty into the wire response itself;
// the peek panel (`components/peek/PeekPanel.tsx`) must always surface it,
// never hide it.

/// `agentview::xref::DefHit` — `#[serde(flatten)]` on the Rust side splices
/// `Symbol`'s own fields directly into this object (see `Symbol` above),
/// same convention as `SymbolHit`.
export interface DefHit extends Symbol {
  repo: string;
  path: string;
  /// `false` only when this hit came from the exact-name pass
  /// (`DefsOut.exact`); every fuzzy-fallback hit is `true`.
  approximate: boolean;
}

/// `GET /api/defs`'s body (`agentview::xref::DefsOut`).
export interface DefsOut {
  schema: string;
  symbol: string;
  /// `true` when `results` came from the exact-name pass; `false` means the
  /// exact pass found nothing and every result is a fuzzy fallback (all
  /// `results[].approximate === true`).
  exact: boolean;
  results: DefHit[];
}

/// `agentview::xref::RefHit`.
export interface RefHit {
  path: string;
  line: number;
  text: string;
  /// Always `true` — plain text matching, never scope-resolved (see
  /// `RefsOut.note`).
  approximate: boolean;
}

/// `GET /api/xrefs`'s body (`agentview::xref::RefsOut`).
export interface RefsOut {
  schema: string;
  repo: string;
  symbol: string;
  results: RefHit[];
  truncated: boolean;
  /// The search hit its time budget before finishing — an honest "there may
  /// be more (or ANY) matches we didn't get to look for" signal, distinct
  /// from `truncated` (mirrors `search::text`'s own split).
  time_budget_exceeded: boolean;
  /// The server's own honesty note (`xref::REFS_NOTE`) — surfaced as the
  /// approximate badge's tooltip, never paraphrased away.
  note: string;
}

// --- Time-first-class (Phase C — commit/compare/branches/file-history) ----
//
// Mirrors `crates/kb-code-server/src/history/{mod,commit,compare,branches,
// file_history}.rs` + `routes.rs`'s four response wrappers + `numstat::
// FileChange`, field-for-field (see the header comment). `join::ladder::
// Attribution` (this file's `LadderAttribution`) is a DIFFERENT shape from
// `AttributionOut` above (that one's `provenance::why::AttributionOut`, the
// line-grade why query's narrower attribution) — same underlying
// confidence/via vocabulary, but `LadderAttribution` additionally carries
// `schema`/`kb`/`started_at`/the echoed `sha` (join/1's full wire shape),
// which `AttributionOut` never did. Kept as two separate interfaces rather
// than unified, mirroring the Rust side's own two distinct structs.

/// `history::Person` — `{name, email, time}`, `time` unix seconds.
export interface Person {
  name: string;
  email: string;
  time: number;
}

/// `history::Trailer`.
export interface Trailer {
  key: string;
  value: string;
}

/// `numstat::FileChange` — one file's merged numstat + name-status row.
export interface FileChange {
  path: string;
  /// `Some` only for a rename/copy (`status` `"R"`/`"C"`) — omitted from the
  /// wire otherwise (`skip_serializing_if = "Option::is_none"`).
  old_path?: string;
  insertions: number;
  deletions: number;
  binary: boolean;
  /// First character of git's name-status code (`A`/`M`/`D`/`R`/`C`/…).
  status: string;
}

/// `history::FileTotals`.
export interface FileTotals {
  files: number;
  insertions: number;
  deletions: number;
}

/// `history::CommitSummary` — shared by `compare/1`'s `commits[]` and
/// `file-history/1`'s `entries[]`.
export interface CommitSummary {
  sha: string;
  subject: string;
  /// `"Name <email>"` plain display string.
  author: string;
  author_time: number;
}

/// `join::ladder::Attribution` (`join/1`'s full wire shape) — see this
/// section's header comment for how it differs from `AttributionOut`.
export interface LadderAttribution {
  schema: string;
  confidence: Confidence;
  via: string;
  session_id?: string;
  kb?: string;
  display_name?: string;
  started_at?: number;
  /// The canonical (locally disambiguated, full-hex when resolvable) sha
  /// this attribution is FOR.
  sha: string;
}

/// `routes::CommitPageResponse` — `GET /api/commit`'s body (`commit/1`).
export interface CommitPageResponse {
  schema: string;
  repo: string;
  sha: string;
  subject: string;
  body?: string;
  author: Person;
  committer: Person;
  parents: string[];
  trailers: Trailer[];
  attribution: LadderAttribution;
  files: FileChange[];
  totals: FileTotals;
}

/// `history::compare::Resolved`.
export interface CompareResolved {
  from_sha: string;
  to_sha: string;
  /// `None` when `from`/`to` share no common ancestor.
  merge_base: string | null;
}

/// `routes::CompareCommitOut` — Phase G-server wraps the plain
/// `CommitSummary` (`#[serde(flatten)]`, so its four fields land at the TOP
/// level) with an OPTIONAL `attribution`, present only when the compare was
/// fetched with `?attribution=true`. Session-grouped review (`lib/
/// reviewGroups.ts`) is the sole consumer of this field.
export interface CompareCommitOut extends CommitSummary {
  attribution?: LadderAttribution;
}

/// `routes::ComparePageResponse` — `GET /api/compare`'s body (`compare/1`).
export interface ComparePageResponse {
  schema: string;
  repo: string;
  from: string;
  to: string;
  three_dot: boolean;
  resolved: CompareResolved;
  /// Newest-first.
  commits: CompareCommitOut[];
  commits_truncated: boolean;
  files: FileChange[];
  totals: FileTotals;
}

/// `routes::BranchLast`.
export interface BranchLast {
  subject: string;
  author_time: number;
}

/// `routes::BranchOut`.
export interface BranchOut {
  name: string;
  target_sha: string;
  is_head: boolean;
  /// V70-A3X: `null` when there was NOTHING to compare against (a
  /// detached/unborn HEAD with no `origin/HEAD` either) — distinct from a
  /// genuinely measured zero-diff (the default branch itself, or an
  /// ordinary `rev-list` count of `0`), which is `0`, not `null`.
  ahead: number | null;
  behind: number | null;
  /// `undefined` only if `commit_info` itself failed (rare) — see
  /// `routes::branches_route`'s doc.
  last?: BranchLast;
  attribution: LadderAttribution;
  /// Remote-tracking only (`"origin"`). Absent on a local branch.
  remote?: string | null;
  /// True when an open local review's `head_ref` matches this name.
  has_open_review?: boolean;
  /// Present only for `?sort=suggested`. `terms` names each signal;
  /// `score` is their sum.
  suggest?: { score: number; terms: Record<string, number> };
}

/// `routes::BranchesResponse` — `GET /api/branches`'s body (`branches/1`).
export interface BranchesResponse {
  schema: string;
  repo: string;
  default: string | null;
  branches: BranchOut[];
  truncated: boolean;
}

/// `routes::FileHistoryResponse` — `GET /api/file-history`'s body
/// (`file-history/1`). Newest-first.
export interface FileHistoryResponse {
  schema: string;
  repo: string;
  path: string;
  entries: CommitSummary[];
  truncated: boolean;
}

// --- Resolve (B3 — position-based lookup, the peek's PRIMARY path) --------
//
// Mirrors `crates/kb-code-server/src/resolve.rs`, field-for-field (see the
// header comment). `precision` is an OPEN string vocabulary — `"file-local"`
// | `"tags-approx"` today, with `"import-heuristic"`/`"scip-exact"` arriving
// in later Waves — every reader treats an unrecognized value as the generic
// approximate tier rather than crashing on it (see
// `components/peek/PeekPanel.tsx`'s `PrecisionBadge`).

export interface ResolvePosition {
  line: number;
  col: number;
}

/// `resolve::Candidate`.
export interface ResolveCandidate {
  repo: string;
  path: string;
  line: number;
  kind: string | null;
  container: string | null;
  signature: string | null;
  doc: string | null;
  precision: string;
  /// Trust class — `"exact"` | `"likely"` | `"candidate"` (V3.G1 / H1).
  /// Optional for older daemons that only sent `precision`.
  class?: string;
}

// --- hover/1 (PRR-N5 server, V70-A6's first SPA consumer) -------------------
//
// `GET /api/hover?repo=&path=&line=&col=` — mirrors
// `crates/kb-code-server/src/hover.rs`'s `HoverOut` field-for-field. The
// route shipped in PRR-N5 and, until V70-A6, had ZERO SPA consumers (recon
// navigation-history.md / reader-selection.md): the identifier tooltip
// (`editor/hoverTooltip.ts`) is the first.
//
// `precision`/`trust` are `null` ONLY when resolve found no candidate at all
// — an honest absence, never a fabricated tier (that module's own doc).

export interface HoverSymbol {
  kind: string;
  name: string;
  container: string | null;
  signature: string | null;
  doc: string | null;
}

export interface HoverDefsite {
  path: string;
  line: number;
}

export interface HoverFramework {
  kind: string;
  dst_kind: string | null;
  dst_path: string | null;
  trust: string;
}

export interface HoverOut {
  schema: string;
  path: string;
  line: number;
  col: number;
  precision: string | null;
  trust: string | null;
  symbol: HoverSymbol | null;
  defsite: HoverDefsite | null;
  framework: HoverFramework | null;
}

// --- Hierarchy (V3.1-H1 wire shapes; SPA H3a consumer) ----------------------
//
// Mirrors `crates/kb-code-server/src/hierarchy.rs`, field-for-field. Class is
// mandatory on every edge (`exact`/`likely`/`candidate`); truncation is
// callers-only on the wire today (`CallersOut.truncated`).

export interface HierarchyFunctionRef {
  name: string;
  kind: string;
  path: string;
  line: number;
}

export interface HierarchyResolveTarget {
  path: string;
  line: number;
  class: string;
  precision: string;
}

export interface HierarchyCalleeSite {
  name: string;
  qualifier?: string | null;
  line: number;
  col: number;
  arg_count?: number | null;
  class: string;
  target?: HierarchyResolveTarget | null;
}

export interface HierarchyCalleesOut {
  schema: string;
  function: HierarchyFunctionRef;
  callees: HierarchyCalleeSite[];
}

export interface HierarchyEnclosingRef {
  name: string;
  kind: string;
  line: number;
}

export interface HierarchyCallerSite {
  line: number;
  col: number;
  class: string;
}

export interface HierarchyCallerGroup {
  path: string;
  enclosing?: HierarchyEnclosingRef | null;
  sites: HierarchyCallerSite[];
}

export interface HierarchyCallersOut {
  schema: string;
  function: HierarchyFunctionRef;
  callers: HierarchyCallerGroup[];
  truncated: boolean;
}

export interface HierarchyTypeVia {
  path: string;
  line: number;
}

export interface HierarchyTypeTarget {
  path: string;
  line: number;
}

export interface HierarchyTypeEdge {
  name: string;
  kind: string;
  via: HierarchyTypeVia;
  class: string;
  target?: HierarchyTypeTarget | null;
}

export interface HierarchyTypesOut {
  schema: string;
  name: string;
  supertypes: HierarchyTypeEdge[];
  subtypes: HierarchyTypeEdge[];
}

/// `GET /api/resolve`'s body (`resolve::ResolveOut`, `resolve/1`).
export interface ResolveOut {
  schema: string;
  ident: string;
  position: ResolvePosition;
  /// `"def"` | `"ref"` | `"import"` when resolved via the occurrences
  /// table; `null` when resolved via the plain word-scan fallback (see the
  /// Rust module doc).
  role: string | null;
  candidates: ResolveCandidate[];
  total: number;
  /// The server's own honesty note (`resolve::RESOLVE_NOTE`) — surfaced next
  /// to every precision badge's tooltip, never paraphrased away.
  note: string;
}

// --- PRR-N5 — framework edges (`GET /api/framework/edges`, T1 rails-lens
// reader surface, design-ui.md §9.4a) ---------------------------------------
//
// Mirrors `crates/kb-code-server/src/framework_edges.rs` +
// `frameworks::FrameworkEdge` field-for-field. `kind`/`trust` are
// closed-vocabulary strings server-side (`frameworks::EdgeKind`/
// `frameworks::Trust`) but travel as plain `string` here — same
// open-string-on-the-wire posture `ResolveCandidate.precision` documents
// above, since a client build can lag a daemon that's grown a new edge kind.

export interface FrameworkEdge {
  kind: string;
  src_path: string;
  src_line: number | null;
  src_symbol: string | null;
  dst_kind: string | null;
  dst_path: string | null;
  dst_symbol: string | null;
  /// `"likely"` | `"candidate"` — `frameworks::Trust` has no `"exact"`
  /// variant (see that enum's own doc: this lens never claims certainty).
  trust: string;
  extra_json: string | null;
}

export type FrameworkEdgeDirection = "src" | "dst";

/// `#[serde(flatten)]` on the Rust side splices `FrameworkEdge`'s own fields
/// directly alongside `direction` — same flatten convention
/// `OpenAnnotationEntry` uses above.
export interface FrameworkEdgeOut extends FrameworkEdge {
  /// `"src"` — `path` PRODUCED this edge. `"dst"` — `path` is this edge's
  /// TARGET. See `framework_edges.rs`'s own doc.
  direction: FrameworkEdgeDirection;
}

/// `GET /api/framework/edges`'s body (`framework-edges/1`).
export interface FrameworkEdgesOut {
  schema: string;
  path: string;
  edges: FrameworkEdgeOut[];
  total: number;
}

// --- PRR-N5 — resolve-symbol (`GET /api/resolve-symbol`, T1 `?sym=` deep
// links, design-ui.md §5) ----------------------------------------------------
//
// Mirrors `crates/kb-code-server/src/symbol_addr.rs` field-for-field. A miss
// is a `found: false` VALUE at `200`, never a `404` (see that module's doc)
// — every field below `found`/`sym`/`schema` is `#[serde(skip_serializing_if
// = "Option::is_none")]` server-side, so it's simply ABSENT on the wire
// rather than `null` — hence `?:`, not `| null`, here.

export interface ResolveSymbolOut {
  schema: string;
  sym: string;
  found: boolean;
  path?: string;
  line?: number;
  kind?: string;
  container?: string;
  name?: string;
  /// `"exact"` | `"fuzzy"` | `"rails"` — present iff `found`.
  via?: string;
  /// Present iff `found` is `false` — a human-readable reason, never a bare
  /// `404` (see the module doc).
  reason?: string;
}

// --- V3.1-H2 impact analysis + Code Vision lenses (SPA H3b consumer) --------
//
// Mirrors `crates/kb-code-server/src/{impact_analysis,lenses}.rs`, field-
// for-field. `pain` is ALWAYS null in this wave (no failure signal on the
// wire) — the SPA must not invent a pain chip when null.

export interface ImpactSymbol {
  name: string;
  kind: string | null;
  path: string;
  line: number;
}

export interface ImpactRow {
  path: string;
  line: number;
  col: number;
  /// Trust class — `"exact"` | `"likely"` | `"candidate"`.
  class: string;
  /// `"usage"` | `"caller"` | `"implementor"` | `"import"` | `"transitive"`.
  kind: string;
  name?: string | null;
  /// Present on `transitive` rows only.
  depth?: number | null;
}

export interface ImpactProvenanceSample {
  session_id: string;
  path: string;
  line: number;
}

export interface ImpactBucketProvenance {
  rows_with_session: number;
  distinct_sessions: number;
  sample: ImpactProvenanceSample[];
}

export interface ImpactProvenance {
  direct_exact?: ImpactBucketProvenance | null;
  direct_likely?: ImpactBucketProvenance | null;
}

export interface ImpactTruncated {
  direct_exact: boolean;
  direct_likely: boolean;
  transitive: boolean;
  imports: boolean;
  tests: boolean;
}

/// `GET /api/impact/analysis` body (`impact/1`).
export interface ImpactAnalysisOut {
  schema: string;
  symbol: ImpactSymbol;
  direct_exact: ImpactRow[];
  direct_likely: ImpactRow[];
  transitive: ImpactRow[];
  imports: ImpactRow[];
  tests: ImpactRow[];
  truncated: ImpactTruncated;
  /// `null` when nothing could be attributed (sessionless / degraded).
  provenance: ImpactProvenance | null;
  note: string;
}

export interface LensUsageCounts {
  exact: number;
  likely: number;
  candidate: number;
}

export interface LensAuthor {
  /// `"human"` | `"agent"` | `"mixed"`.
  kind: string;
  label: string;
}

export interface LensSession {
  id: string;
  short: string;
}

export interface LensDeclaration {
  line: number;
  name: string;
  kind: string;
  usages: LensUsageCounts;
  /// Types only; `null` for callables/other.
  implementors: number | null;
  author: LensAuthor | null;
  session: LensSession | null;
  /// ALWAYS null in V3.1-H2 — do not render a pain chip while null.
  pain: boolean | null;
}

/// `GET /api/lenses` body (`lenses/1`).
export interface LensesOut {
  schema: string;
  path: string;
  declarations: LensDeclaration[];
  truncated: boolean;
  total: number;
}

// --- Review workflow (Phase G-server — merge-check/range-diff/repo-state/
// GitHub read overlay) ------------------------------------------------------
//
// Mirrors `crates/kb-code-server/src/{history/merge_check,history/
// range_diff,repo_state,github}.rs` + `routes.rs`'s four wrapper response
// structs, field-for-field (see this file's header comment).

/// `history::merge_check::ConflictEntry` — a bare wrapper (not a plain
/// `string`) so the wire shape can grow per-path detail later without a
/// breaking change, per that struct's own Rust doc.
export interface ConflictEntry {
  path: string;
}

/// `routes::MergeCheckResponse` — `GET /api/merge-check`'s body
/// (`merge-check/1`).
export interface MergeCheckResponse {
  schema: string;
  repo: string;
  from: string;
  to: string;
  resolved: CompareResolved;
  clean: boolean;
  conflicts: ConflictEntry[];
  ahead: number;
  behind: number;
}

/// `history::range_diff::RangeDiffPair.disposition` — `"equal"` (both sides
/// provably identical, subject included) | `"modified"` (present on both
/// sides but differs) | `"added"` (new in `new`) | `"removed"` (dropped
/// from `old`). See `history::range_diff`'s module doc for why a
/// `"modified"` pair only ever carries `old_subject`.
export type RangeDiffDisposition = "equal" | "modified" | "added" | "removed";

/// `history::range_diff::RangeDiffPair`.
export interface RangeDiffPair {
  old_sha?: string;
  new_sha?: string;
  disposition: RangeDiffDisposition;
  old_subject?: string;
  new_subject?: string;
}

/// `routes::RangeDiffResponse` — `GET /api/range-diff`'s body
/// (`range-diff/1`).
export interface RangeDiffResponse {
  schema: string;
  repo: string;
  old: string;
  new: string;
  pairs: RangeDiffPair[];
  truncated: boolean;
}

/// `mirror::gate::RepoOp` (`#[serde(rename_all = "kebab-case")]`) — which of
/// the five marker-file operations (if any) is currently in flight.
export type RepoOp = "none" | "rebase" | "merge" | "cherry-pick" | "bisect";

/// `mirror::gate::OpDetail` — every field independently optional (which
/// apply depends on `op`: `rebase` populates `step`/`total`; `merge`/
/// `cherry-pick` populate `head_sha`; `bisect`/`none` populate nothing,
/// serializing to `{}`).
export interface OpDetail {
  step?: number;
  total?: number;
  head_sha?: string;
}

/// `routes::RepoStateResponse` — `GET /api/repo-state`'s body
/// (`repo-state/1`).
export interface RepoStateResponse {
  schema: string;
  repo: string;
  op: RepoOp;
  detail: OpDetail;
  conflicted: string[];
  dirty: boolean;
}

/// `github::PrOut`.
export interface PrOut {
  number: number;
  title: string;
  author: string;
  head_ref: string;
  base_ref: string;
  updated_at: string;
  draft: boolean;
}

/// `routes::PrsResponse` — `GET /api/prs`'s body (`prs/1`).
export interface PrsResponse {
  schema: string;
  prs: PrOut[];
  truncated: boolean;
  /// Present only when the GitHub call itself failed (rate-limited,
  /// unreachable, a bad response) — `prs` is then always empty and the HTTP
  /// status is still 200, never a 5xx (`github.rs`'s module doc).
  unavailable_reason?: string;
}

/// `github::PrCommentOut` — `path`'s presence distinguishes an inline review
/// comment from a general issue/discussion comment.
export interface PrCommentOut {
  author: string;
  body: string;
  path?: string;
  line?: number;
  created_at: string;
  in_reply_to?: number;
}

/// `routes::PrCommentsResponse` — `GET /api/prs/{number}/comments`'s body
/// (`pr-comments/1`).
export interface PrCommentsResponse {
  schema: string;
  comments: PrCommentOut[];
  truncated: boolean;
  unavailable_reason?: string;
}

/// `routes::PrFetchResponse` — `POST /api/prs/fetch`'s body. LOOPBACK-ONLY
/// (`router.rs`'s transcripts sub-router family) — see `api/client.ts`'s
/// `postPrFetch` doc for how a non-loopback caller's bare 404 is told apart
/// from an ordinary "no such repo" 404.
export interface PrFetchResponse {
  repo: string;
  number: number;
  ref: string;
  sha: string;
}

// --- Reading sets (Phase E4 — "kb-code v2 — The Operable Reader") ---------
//
// Mirrors `crates/kb-code-server/src/reading_sets.rs`, field-for-field (see
// this file's header comment). `SetSpanOut`'s `line_start`/`line_end`/`ref`/
// `note` are `#[serde(skip_serializing_if = "Option::is_none")]` server-side
// — OMITTED from the wire when absent (never sent as `null`), same
// convention `CommitEntryOut.trailers`/`files` document above — every reader
// must treat a missing key as genuinely absent, not a zero/empty-string
// default.

/// `reading_sets::SetSummary` — one row of `GET /api/sets`'s list.
export interface SetSummary {
  id: string;
  name: string;
  description: string | null;
  span_count: number;
  /// V70-A10 — `COUNT(*)` of `annotations.set_id = this set's id`.
  note_count: number;
  created_at: number;
  updated_at: number;
  /// V70-A10 — `"set"` | `"workspace"`.
  kind: string;
  /// V70-A10 — wire key is literally `ref`; omitted when absent.
  ref?: string;
}

/// `GET /api/sets`'s body (`sets/1`), alphabetical by name.
export interface SetsListOut {
  schema: string;
  sets: SetSummary[];
}

/// V70-A10 — one `ref`-grouped bucket (`GET /api/sets?kind=workspace&
/// group=ref`'s data source).
export interface SetGroupOut {
  ref: string | null;
  workspaces: SetSummary[];
}

/// V70-A10 — `GET /api/sets?...&group=ref`'s body, in place of
/// `SetsListOut`'s flat `sets` array.
export interface SetGroupsListOut {
  schema: string;
  groups: SetGroupOut[];
}

/// `reading_sets::SpanOut` — one ordered span inside a set.
export interface SetSpanOut {
  ordinal: number;
  path: string;
  line_start?: number;
  line_end?: number;
  /// A pinned git ref, when the span was captured against one — the wire
  /// key is literally `ref` (`reading_sets::SpanOut`'s `#[serde(rename =
  /// "ref")]` on its own `git_ref` field).
  ref?: string;
  note?: string;
}

/// One reading set's full view — `GET /api/sets/{id}`, `POST /api/sets`,
/// `PATCH /api/sets/{id}`, `POST /api/sets/{id}/spans`, `POST /api/sets/
/// from-session`, and `POST /api/sets/from-doc` all return this same shape
/// (`sets/1`).
export interface SetView {
  schema: string;
  id: string;
  repo: string;
  name: string;
  description: string | null;
  created_at: number;
  updated_at: number;
  spans: SetSpanOut[];
  /// DCB W3.C — doc-materialization provenance
  /// (`reading_sets::SetView`'s Rust doc). `null` on every set NOT created
  /// via `POST /api/sets/from-doc`. R25 — deliberately REQUIRED keys typed
  /// `string | null`, NOT `?:` — the server wires these as an explicit
  /// `null` on purpose (no `skip_serializing_if`, unlike every other
  /// optional field above) so `SetDetail.tsx`'s "doc changed since" check
  /// never has to guess an omitted-vs-null distinction (R14's three-state
  /// banner semantics: null = unknown, never "changed").
  source_kb: string | null;
  source_doc_id: string | null;
  source_doc_path: string | null;
  source_doc_hash: string | null;
  /// V70-A10 — `"set"` | `"workspace"`.
  kind: string;
  /// V70-A10 — the workspace's opaque `DeskState` snapshot, verbatim.
  /// `skip_serializing_if` server-side (omitted, not `null`, when absent —
  /// unlike the `source_*` quartet above).
  desk_json?: string;
  /// V70-A10 — wire key is literally `ref`; omitted when absent.
  ref?: string;
  /// V70-A10 — free-text Markdown; omitted when absent.
  description_md?: string;
}

// --- Phase N ("Navigate") — bookmarks + todos + scopes -------------------
//
// Hand-typed against `crates/kb-code-server/src/bookmarks.rs` and
// `routes::{TodoItemOut,TodosListOut,ScopesOut}`. Field names match the
// wire (serde default snake_case); optional `mnemonic`/`note` are omitted
// when absent on the list/create responses (`skip_serializing_if`).

/// One durable place bookmark (`bookmarks/1`).
export interface Bookmark {
  id: number;
  repo: string;
  path: string;
  line: number;
  mnemonic?: string;
  note?: string;
  created_at: number;
  updated_at: number;
}

/// `GET /api/bookmarks?repo=` body.
export interface BookmarksListOut {
  schema: string;
  bookmarks: Bookmark[];
}

/// One TODO/FIXME/… marker hit from the index.
export interface TodoItem {
  path: string;
  line: number;
  marker: string;
  text: string;
}

/// `GET /api/todos` body.
export interface TodosListOut {
  items: TodoItem[];
  total: number;
  truncated: boolean;
}

/// `GET /api/scopes` body — configured `[scopes]` name → glob patterns.
export interface ScopesOut {
  scopes: Record<string, string[]>;
}

// --- V3.R1 / V3.R2 — local Gerrit-lite review sessions --------------------
//
// Wire shapes mirror `crates/kb-code-server/src/reviews.rs` (`reviews/1`).
// Reads are bearer-gated; mutations are LOOPBACK-ONLY (same sub-router as
// checkout / prs/fetch — bare 404 for a non-loopback caller).

export type ReviewState = "open" | "closed";

/// `PUT /api/reviews/{id}/verdict` body + the stamped block on list/get.
export type ReviewVerdictState = "comment" | "approve" | "request-changes";

export interface ReviewVerdict {
  state: ReviewVerdictState;
  note: string | null;
  at: number | null;
  ps: number | null;
}

/// One row of `GET /api/reviews?repo=` (`list_reviews`).
export interface ReviewSummary {
  id: number;
  repo: string;
  title: string | null;
  base_ref: string;
  head_ref: string;
  session_id: string | null;
  state: ReviewState;
  created_at: number;
  updated_at: number;
  latest_ps: number | null;
  files_count: number;
  viewed_count: number;
  open_annotations: number;
  /// V4.C2 — present on list/get; `null` when unset.
  verdict: ReviewVerdict | null;
  verdict_stale: boolean;
}

/// `GET /api/reviews?repo=` body.
export interface ReviewsListOut {
  schema: string;
  reviews: ReviewSummary[];
}

/// One patchset on a review (`get_review`'s `patchsets[]`).
export interface ReviewPatchset {
  ps_number: number;
  tip_sha: string;
  tip_sha_full: string;
  base_sha: string;
  base_sha_full: string;
  captured_at: number;
  commit_count: number;
}

/// `GET /api/reviews/{id}` body.
export interface ReviewDetail {
  schema: string;
  id: number;
  repo: string;
  title: string | null;
  base_ref: string;
  head_ref: string;
  session_id: string | null;
  state: ReviewState;
  created_at: number;
  updated_at: number;
  patchsets: ReviewPatchset[];
  /// V4.C2 — same block as the list row.
  verdict: ReviewVerdict | null;
  verdict_stale: boolean;
}

/// One file row on `GET /api/reviews/{id}/files`.
export interface ReviewFileRow {
  path: string;
  old_path: string | null;
  status: string;
  additions: number;
  deletions: number;
  blob_sha: string;
  viewed: boolean;
  viewed_stale: boolean;
  open_annotations: number;
}

/// V73-K2a — one `review_hunk_viewed` row (migration V0031), as it rides
/// `GET /api/reviews/{id}/files`. `hunk_id` is the SPA's OWN content
/// address (`lib/diffHunks.ts`, `kbc-hunkid/1`) — the daemon stores it
/// opaquely, so this type is the only place the two halves meet.
export interface ReviewHunkViewedRow {
  hunk_id: string;
  path: string;
}

/// `GET /api/reviews/{id}/files?ps=` body.
export interface ReviewFilesOut {
  schema: string;
  review_id: number;
  ps_number: number;
  base_sha: string;
  tip_sha: string;
  files: ReviewFileRow[];
  /// V73-K2a — ADDITIVE and therefore OPTIONAL: a daemon built before
  /// V0031 does not send it, and every reader treats absence as "no
  /// per-hunk marks recorded", never as an error. NOT scoped to `ps` — a
  /// hunk id is content-addressed, so a mark follows the change across
  /// patchsets by construction.
  hunks_viewed?: ReviewHunkViewedRow[];
}

/// One interdiff file row (no viewed/annotation fields).
export interface ReviewInterdiffFile {
  path: string;
  old_path: string | null;
  status: string;
  additions: number;
  deletions: number;
}

/// `GET /api/reviews/{id}/interdiff?from=&to=` body.
export interface ReviewInterdiffOut {
  schema: string;
  review_id: number;
  from: number;
  to: number;
  from_tip: string;
  to_tip: string;
  files: ReviewInterdiffFile[];
  range_diff: {
    pairs: RangeDiffPair[];
    truncated: boolean;
  };
}

/// One open annotation on the review change set.
export interface ReviewAnnotationItem {
  id: string;
  path: string;
  intent: string;
  body: string;
  author: string;
  created_at: number;
  orphaned: boolean;
}

/// Path-grouped open annotations (`GET /api/reviews/{id}/annotations`).
export interface ReviewAnnotationGroup {
  path: string;
  annotations: ReviewAnnotationItem[];
}

export interface ReviewAnnotationsOut {
  schema: string;
  review_id: number;
  repo: string;
  ps_number: number;
  groups: ReviewAnnotationGroup[];
}

// --- V4.C1 / C4 — review-scoped comment threads (`review-comments/1`) ------

export interface ReviewCommentResolvedAgainst {
  ps: number;
  sha: string;
}

export interface ReviewCommentOriginal {
  ps: number;
  side: string;
  line: number;
  snippet: string;
}

/// Per-request resolution against the TARGET patchset. `line`/`line_end`
/// are `null` when orphaned (a wrong-line number is worse than a hole).
export interface ReviewCommentResolution {
  line: number | null;
  line_end?: number | null;
  orphaned: boolean;
  resolved_against: ReviewCommentResolvedAgainst;
  original?: ReviewCommentOriginal;
}

export interface ReviewCommentSuggestion {
  replacement: string;
  original: string;
  applied: boolean;
  applied_at: number | null;
}

/// `PUT /api/annotations/{id}/suggestion` success body.
export interface AnnotationSuggestionOut {
  annotation_id: string;
  replacement: string;
  original: string;
  base_blob_sha: string;
  applied: boolean;
  applied_at: number | null;
  applied_head_sha: string | null;
}

/// `POST /api/annotations/{id}/apply` 200 — the splice landed.
export interface ApplySuggestionChanged {
  applied: true;
  changed: true;
  already_applied?: undefined;
  path: string;
  line: number;
  line_end?: number;
}

/// `POST /api/annotations/{id}/apply` 200 — working-tree range already
/// equals `replacement`. No write, `applied` is not flipped.
export interface ApplySuggestionAlready {
  already_applied: true;
  changed: false;
  applied?: boolean;
}

export type ApplySuggestionOut = ApplySuggestionChanged | ApplySuggestionAlready;

/// `POST /api/annotations/{id}/apply` 409 body — exact-match miss.
/// Tree left untouched.
export interface ApplySuggestionConflict {
  error: string;
  expected: string;
  found: string;
  resolved_line: number;
}

export interface ReviewCommentReply {
  id: string;
  parent_id: string | null;
  path: string;
  intent: string;
  body: string;
  author: string;
  created_at: number;
  updated_at: number;
  resolved: boolean;
}

/// One top-level review comment + nested replies (`GET …/comments`).
export interface ReviewComment {
  id: string;
  path: string;
  intent: string;
  body: string;
  author: string;
  created_at: number;
  updated_at: number;
  resolved: boolean;
  anchor_kind: string;
  side: string | null;
  ps_number: number | null;
  resolution: ReviewCommentResolution;
  suggestion: ReviewCommentSuggestion | null;
  replies: ReviewCommentReply[];
}

export interface ReviewCommentGroup {
  path: string;
  comments: ReviewComment[];
}

/// `GET /api/reviews/{id}/comments?ps=latest|N&all=true` body.
export interface ReviewCommentsOut {
  schema: string;
  review_id: number;
  repo: string;
  ps: number;
  groups: ReviewCommentGroup[];
}

/// `PUT`/`DELETE /api/reviews/{id}/verdict` body.
export interface SetVerdictOut {
  changed: boolean;
}

/// `POST /api/reviews` success body (CREATED).
export interface CreateReviewOut {
  schema: string;
  id: number;
  repo: string;
  title: string | null;
  base_ref: string;
  head_ref: string;
  session_id: string | null;
  state: ReviewState;
  created_at: number;
  updated_at: number;
  latest_ps: number;
  tip_sha: string;
  base_sha: string;
}

/// `POST /api/reviews/{id}/snapshot` body.
export interface SnapshotReviewOut {
  schema: string;
  review_id: number;
  ps_number: number;
  tip_sha: string;
  base_sha: string;
  captured_at: number;
}

// --- V3.2-B1 / B2 — behavioral attention signals ---------------------------
//
// Wire shapes mirror `crates/kb-code-server/src/behavioral/routes.rs` and
// `reviews::review_risk_route`. These rank ATTENTION only — never quality.
// Missing inputs are absent/`null` on the wire, never a default 0.

/// Decomposed hotspot score terms (`hotspot.terms`).
export interface HotspotTerms {
  churn_rank: number;
  complexity_rank: number;
  /** Present only when `weight=pain` (B2). */
  pain?: number;
}

export interface HotspotScore {
  score: number;
  terms: HotspotTerms;
}

export interface ComplexityOut {
  loc: number;
  indent_sum: number;
}

/// One row of `GET /api/behavioral/hotspots`.
export interface HotspotRow {
  path: string;
  revisions: number;
  churn: number;
  complexity: ComplexityOut;
  hotspot: HotspotScore;
  last_touch_unix: number | null;
  age_days: number | null;
}

/// `GET /api/behavioral/hotspots` body.
export interface HotspotsOut {
  schema: string;
  repo: string;
  weight: string;
  items: HotspotRow[];
  total: number;
  truncated: boolean;
}

export interface CouplingPartner {
  path: string;
  co_commits: number;
  support: number;
  confidence: number;
}

/// `GET /api/behavioral/coupling` body.
export interface CouplingOut {
  schema: string;
  repo: string;
  path: string;
  partners: CouplingPartner[];
  total: number;
  truncated: boolean;
}

export interface AuthorShare {
  author: string;
  commits: number;
  share: number;
}

/** B2 dual-author session row; empty when no join data. */
export interface AgentShare {
  session_id: string;
  commits: number;
  share: number;
}

/// `GET /api/behavioral/ownership` body.
export interface OwnershipOut {
  schema: string;
  repo: string;
  path: string;
  total_commits: number;
  authors: AuthorShare[];
  /** B2 — dual-author session rows (may be empty). */
  agents?: AgentShare[];
  /**
   * B2 — fraction of human commits that also resolved to a session.
   * Absent/`null` when no session data at all (unknown ≠ none / 0.0).
   */
  agent_share?: number | null;
  major: number;
  minor: number;
  ownership: number;
  fragmentation: number;
}

export interface AgeBucketOut {
  label: string;
  lines: number;
}

/// `GET /api/behavioral/age` body.
export interface AgeOut {
  schema: string;
  repo: string;
  path: string;
  lines: number;
  oldest_unix: number | null;
  newest_unix: number | null;
  median_age_days: number;
  buckets: AgeBucketOut[];
}

// --- V3.4-C1 time-series (request-time; no storage) -----------------------
//
// Mirrors `crates/kb-code-server/src/behavioral::{TimeseriesBucket,routes::TimeseriesOut}`.
// Attention signal only — never a health grade. Empty window ⇒ buckets:[] + note.

/// One week bucket of activity (`week_start_unix` = Monday 00:00 UTC).
export interface TimeseriesBucket {
  week_start_unix: number;
  commits: number;
  /** `adds + dels` over the scoped files in the bucket. */
  churn: number;
  authors: number;
}

/// `GET /api/behavioral/timeseries?repo=&path=&weeks=` body.
export interface TimeseriesOut {
  schema: string;
  repo: string;
  path?: string | null;
  weeks: number;
  since_unix: number;
  buckets: TimeseriesBucket[];
  truncated: boolean;
  note: string;
}

// --- GET /api/symbols (per-file or repo-wide substring) --------------------

/** Per-file form of `GET /api/symbols?repo=&path=`. */
export interface SymbolsFileOut {
  repo: string;
  path: string;
  ref: string | null;
  lang: string | null;
  symbols: Symbol[];
}

/** One hit in the repo-wide form (`matches[]`). */
export interface SymbolMatch {
  path: string;
  ordinal: number;
  name: string;
  kind: string;
  line_start: number;
  line_end: number;
  col_start: number;
  col_end: number;
  container: string | null;
  signature: string | null;
  doc: string | null;
}

/** Repo-wide form of `GET /api/symbols?repo=&q=`. */
export interface SymbolsSearchOut {
  repo: string;
  q: string;
  matches: SymbolMatch[];
}

/// Per-term inputs for one review file's attention composite.
export interface ReviewRiskTerms {
  relative_churn: number | null;
  ownership_minor: number | null;
  hotspot_rank: number | null;
  agent_first_touch: boolean | null;
  session_pain: number | null;
}

export interface ReviewRiskScore {
  score: number;
  terms: ReviewRiskTerms;
}

/// One file on `GET /api/reviews/{id}/risk`.
export interface ReviewRiskFile {
  path: string;
  /** `null` when nothing computable — never a default 0. */
  risk: ReviewRiskScore | null;
  inputs_missing: string[];
}

/// `GET /api/reviews/{id}/risk` body (B2; 404 when not landed).
export interface ReviewRiskOut {
  schema: string;
  review_id: number;
  repo: string;
  ps_number: number;
  files: ReviewRiskFile[];
  note?: string;
}

// --- V3.3-Q1 — named deterministic recipes ---------------------------------
//
// Wire shapes mirror `crates/kb-code-server/src/recipes.rs`. Catalog is pure
// (no repo); run items are per-recipe JSON with decomposed `terms`.

export interface RecipeParamMeta {
  name: string;
  required: boolean;
}

export interface RecipeCatalogEntry {
  name: string;
  recipe_version: number;
  params: RecipeParamMeta[];
  description: string;
  needs: string[];
}

/// `GET /api/recipes` body (`recipes/1` catalog).
export interface RecipesCatalogOut {
  schema: string;
  recipes: RecipeCatalogEntry[];
}

/**
 * One row of a recipe run. Fields vary by recipe (path/symbol/score/class…);
 * every scored/ranked row carries `terms` when the server emits them.
 */
export interface RecipeItem {
  path?: string;
  symbol?: string;
  kind?: string;
  line?: number;
  score?: number;
  class?: string;
  terms?: Record<string, number | string | boolean | null>;
  [key: string]: unknown;
}

/// `GET /api/recipes/{name}` body.
export interface RecipeRunOut {
  schema: string;
  recipe: string;
  recipe_version: number;
  repo: string;
  items: RecipeItem[];
  inputs_missing: string[];
  note?: string | null;
  total: number;
  truncated: boolean;
}

// --- V3.3-S1 — review map + reading order ----------------------------------
//
// Mirror `crates/kb-code-server/src/review_map.rs`. `agent_touched` is
// `true|null` only (never false).

export interface ReviewMapSymbol {
  name: string;
  kind: string;
  class: string;
}

export interface ReviewMapNode {
  path: string;
  status: string;
  symbols_changed: ReviewMapSymbol[];
  /** `true` = agent-touched marker; `null` = no marker (unknown). Never false. */
  agent_touched: true | null;
}

export interface ReviewMapEdge {
  from: string;
  to: string;
  kind: "import" | "call" | string;
  class: string;
}

/// `GET /api/reviews/{id}/map` body (`review-map/1`).
export interface ReviewMapOut {
  schema: string;
  review_id?: number;
  repo?: string;
  ps_number: number;
  nodes: ReviewMapNode[];
  edges: ReviewMapEdge[];
  inputs_missing: string[];
  note?: string | null;
}

export interface ReviewReadingStop {
  path: string;
  reason: string;
  cycle: boolean;
}

/// `GET /api/reviews/{id}/reading-order` body (`review-reading-order/1`).
export interface ReviewReadingOrderOut {
  schema: string;
  review_id?: number;
  repo?: string;
  ps_number: number;
  stops: ReviewReadingStop[];
  inputs_missing: string[];
  note?: string | null;
}

// --- V3.3-S2 — dependent-branch stacks -------------------------------------
//
// Mirror `crates/kb-code-server/src/history/stacks.rs` + `routes.rs`
// stacks response wrappers. Layer-diff flattens the compare payload.

export interface StackLayerTip {
  sha: string;
  subject: string;
  date: number;
}

export interface StackLayer {
  branch: string;
  /** Empty when unresolved — never a guessed base. */
  base: string;
  ahead: number;
  behind: number;
  stale: boolean;
  tip_shared: boolean;
  unresolved: boolean;
  tip: StackLayerTip;
}

export interface Stack {
  layers: StackLayer[];
}

/// `GET /api/stacks` body (`stacks/1`).
export interface StacksOut {
  schema: string;
  repo: string;
  default_branch: string | null;
  stacks: Stack[];
  truncated: boolean;
}

/// `GET /api/stacks/layer-diff` body (`stacks-layer-diff/1`) — compare-shaped
/// payload plus layer identity so the SPA reuses the compare file renderer.
export interface StacksLayerDiffOut {
  schema: string;
  repo: string;
  branch: string;
  base: string;
  base_tip: string;
  tip: string;
  stale: boolean;
  resolved: CompareResolved;
  commits: CommitSummary[];
  commits_truncated: boolean;
  files: FileChange[];
  totals: FileTotals;
  truncated: boolean;
}

// --- V3.4-C1 — canvas-set persistence (SPA working-set canvas durability) ---
//
// Mirrors `crates/kb-code-server/src/canvas.rs` field-for-field. The payload
// is opaque JSON owned by the SPA (`lib/canvasPayload.ts`); the server only
// stores + size-caps it (256 KiB hard 413).

/// `GET /api/canvas?repo=` list row — no payload body.
export interface CanvasSummary {
  id: number;
  name: string;
  review_id: number | null;
  updated_unix: number;
  payload_bytes: number;
}

/// `GET /api/canvas?repo=` body (`canvas/1`).
export interface CanvasListOut {
  schema: string;
  repo: string;
  items: CanvasSummary[];
}

/// `GET /api/canvas/{id}` / create / update body (`canvas/1`).
export interface CanvasView {
  schema: string;
  id: number;
  repo: string;
  name: string;
  review_id: number | null;
  /** Opaque SPA payload — shape owned by `lib/canvasPayload.ts`. */
  payload: unknown;
  created_unix: number;
  updated_unix: number;
}

// --- DCB W2.B — the doc↔code lens (`codelens/1` + `codelens-scorecard/1`) --
//
// Hand-typed mirrors of `crates/kb-code-server/src/doclens/wire.rs`'s
// `Serialize` structs, field-for-field — the LANDED wire as of W1.C + W2.A
// (this track, W2.B, is the SPA's first consumer of it): W2.A's additive
// blame-remap fields (`doc_code_rev`/`rev_remap`/`resolved_line_end`/
// `remap`/`spans`) are included from day one, so this file never needs a
// second pass to catch up with a wire that landed one phase earlier.

export type LensRepoState = "ready" | "indexing" | "error";

export type CodeLensPathState = "present" | "ambiguous" | "absent" | "external";
export type CodeLensLineState = "confirmed" | "drifted" | "unverifiable" | "absent";
export type CodeLensLineEvidence = "rev_remap" | "context_token" | "none";
/// doc-lens's OWN symbol vocabulary — NEVER `/api/resolve`'s
/// `exact`/`likely`/`candidate` trust classes (amendment 6). Render these
/// literal values as labels; never invent a mapping back to the resolve
/// vocabulary.
export type CodeLensSymbolState =
  | "hit_unique"
  | "hit_container_matched"
  | "hit_ambiguous"
  | "no_symbol";

/// R10's frozen set: `path_list` IN, `dir` OUT.
export type CodeLensKind =
  | "path"
  | "path_line"
  | "path_range"
  | "path_list"
  | "symbol_method"
  | "symbol_const"
  | "issue"
  | "external";

export interface CodeLensGroup {
  key: string;
  label: string;
  anchor: string;
  ordinal: number;
  ref_count: number;
}

/// R11 — a GitHub issue citation, server-rebuilt from the parts (never
/// `raw`, which may carry a `#issuecomment-…` suffix).
export interface CodeLensIssueRef {
  owner: string;
  repo: string;
  number: number;
  href: string;
}

/// The ONE place the fuzzy lane appears in the whole design — as a LINK,
/// never a verdict. Build `/search?q={q}&repo={repo}` from this, verbatim.
export interface CodeLensSearchLink {
  q: string;
  repo: string;
}

export interface CodeLensSymbolHit {
  path: string;
  line_start: number;
  line_end: number;
  kind: string;
  container: string | null;
}

/// `RefOut.reader` — THE deep-link source (built from the RESOLUTION —
/// `resolved_path`/`resolved_line` — never from `path_hint`/`line_hint`,
/// the doc's own unverified citation). Non-null whenever `path_state ===
/// "present"`; `line` (== `resolved_line`) is nullable even then — a
/// present-path ref with no line hint at all (a bare `path`-kind ref)
/// still gets a non-null `reader`, just with `line: null`.
export interface CodeLensReaderTarget {
  repo: string;
  path: string;
  line: number | null;
}

/// W2.A — one span's line outcome (`RefOut.spans[]`). `[]` for
/// `path`/`symbol_*`/`issue`/`external` refs and for any ref whose path
/// did not resolve; a `path_line` ref has exactly one span.
export interface CodeLensSpanOutcome {
  line_hint: number;
  line_hint_end: number | null;
  line_state: CodeLensLineState;
  line_evidence: CodeLensLineEvidence;
  confirm_token: string | null;
  token_line: number | null;
  resolved_line: number | null;
  resolved_line_end: number | null;
  line_hint_delta: number | null;
  line_reason: string | null;
}

/// CT-F2 — "was this ref's citation true AT THE REV THE DOC DECLARED?",
/// additive alongside (never instead of) the ref's ever-present
/// current-tree verdict (`path_state`/`line_state` on `CodeLensRef`).
/// Reuses that SAME vocabulary rather than minting a new one — evaluated
/// with no remap involved (the doc's own declared rev IS the coordinate
/// space its line numbers were counted in). Present on a ref only when
/// `CodeLensOut.era === "declared"`.
export interface CodeLensWhenWritten {
  path_state_at_rev: CodeLensPathState;
  line_state_at_rev: CodeLensLineState;
}

export interface CodeLensRef {
  ordinal: number;
  group: string | null;
  kind: CodeLensKind;
  raw: string;
  declared: boolean;

  // --- what the document said (hints) — NEVER build a link/target from
  // these directly; they are the doc's own unverified citation. ----------
  path_hint: string | null;
  line_hint: number | null;
  line_hint_end: number | null;
  symbol_container: string | null;
  symbol_member: string | null;
  context: string | null;

  // --- what this daemon verified -----------------------------------------
  /// `null` for a ref with no `path_hint` (D5) and for `kind === "issue"`.
  path_state: CodeLensPathState | null;
  resolved_path: string | null;
  candidate_count: number;
  /// `[]` when `candidate_count > 3` (`AMBIGUITY_INLINE_MAX`) — tier on
  /// `candidate_count`, NEVER `candidates.length`: a `>3` ambiguous ref has
  /// `candidates: []`, which is not "zero candidates".
  candidates: string[];
  issue: CodeLensIssueRef | null;

  line_state: CodeLensLineState;
  line_evidence: CodeLensLineEvidence;
  confirm_token: string | null;
  token_line: number | null;
  resolved_line: number | null;
  /// W2.A ranges — the MAPPED end of a `path:5-19` citation. `null` unless
  /// the ref is a range `rev_remap` mapped end-to-end.
  resolved_line_end: number | null;
  line_hint_delta: number | null;
  file_lines: number;
  line_reason: string | null;
  /// W2.A — this ref's remap outcome: `null | "applied" | "inside_change" |
  /// "path_absent_at_rev" | "budget_exhausted" | "diff_failed"`.
  remap: string | null;

  symbol_state: CodeLensSymbolState;
  symbol_hit_count: number;
  symbol_hits: CodeLensSymbolHit[];

  spans: CodeLensSpanOutcome[];

  reader: CodeLensReaderTarget | null;
  search: CodeLensSearchLink | null;
  /// `"declared but absent"` | `"unusable path"` | `"unusable issue ref"`.
  note: string | null;
  /// CT-F2 — `null` unless the response's `era === "declared"` (an honest
  /// absence, never a guess): the doc carried no usable `kb-code-rev`, or
  /// the caller never asked via `?at=declared` in the first place.
  when_written: CodeLensWhenWritten | null;
}

export interface CodeLensCodeRev {
  repo_label: string;
  sha: string;
  dirty: boolean;
}

/// W2.A — the blame-remap outcome for the WHOLE document, reported once.
/// `null` whenever the doc declared no `kb-code-rev` (then
/// `CodeLensOut.doc_code_rev` is also null) — but NOT only then: the
/// per-request `spawn_blocking` closure returns `(facts, None, None)`
/// BEFORE the remap even runs when the selected repo's own `facts.state`
/// isn't `"ready"` (indexing/error), so a doc that DOES declare a
/// `kb-code-rev` can still carry a null `rev_remap` here. Never key UI
/// logic on "`rev_remap` null ⇔ `doc_code_rev` null" — check `repo.state`
/// separately when that distinction matters.
export interface CodeLensRevRemap {
  /// `"applied" | "skipped" | "unavailable"`.
  state: string;
  /// `null | "doc_rev_dirty" | "rev_label_mismatch" | "rev_unknown"`.
  reason: string | null;
  repo_label: string | null;
  /// As WRITTEN in the doc (possibly abbreviated).
  sha: string | null;
  /// Full 40-hex, when it resolved in the selected checkout.
  resolved_sha: string | null;
  /// The DOC's `+dirty` marker — never the selected repo's own dirtiness
  /// (`CodeLensRepo.dirty` reports that separately).
  dirty: boolean;
  paths_mapped: number;
  budget_exhausted: boolean;
}

/// The checkout a lens was computed against. `state` is a per-REPO state
/// and is NEVER a `path_state`.
export interface CodeLensRepo {
  name: string;
  root: string;
  state: LensRepoState;
  head_sha: string | null;
  head_branch: string | null;
  dirty: boolean | null;
  /// `"param"` (an explicit `?repo=`) or `"pin"` — never "guessed"
  /// (Decision 1: a checkout is never auto-selected).
  source: "param" | "pin";
}

export interface CodeLensCounts {
  /// The FULL feed count, even when `refs` was truncated.
  total: number;
  resolved: number;
  present: number;
  ambiguous: number;
  absent: number;
  external: number;
  confirmed: number;
  drifted: number;
  unverifiable: number;
  line_absent: number;
  declared_but_absent: number;
}

/// `GET /api/doc-lens?kb=&doc=&repo=` body (`codelens/1`).
export interface CodeLensOut {
  schema: string;
  kb: string;
  doc_id: string;
  /// The id the caller ASKED for, when kb's moves chain answered a
  /// different one (the pin was re-keyed to `doc_id`).
  moved_from: string | null;
  doc_path: string | null;
  /// The ONE kb link-out, built SERVER-SIDE (`doclens::doc_href`); `null`
  /// when `[kb_daemon]` has no usable public base. Consume this directly —
  /// never rebuild a client-side equivalent (R1/minor-108).
  doc_href: string | null;
  doc_hash: string | null;
  doc_title: string | null;
  doc_extracted_at: number | null;
  doc_code_rev: CodeLensCodeRev | null;
  rev_remap: CodeLensRevRemap | null;
  /// kb has NO extraction row for this doc — the THIRD state, distinct
  /// from `refs: []`. Render "not scanned yet — reindex to populate",
  /// NEVER "no code refs".
  never_scanned: boolean;
  repo: CodeLensRepo;
  resolved_unix: number;
  truncated: boolean;
  partial: boolean;
  partial_reason: string | null;
  counts: CodeLensCounts;
  ungrouped_count: number;
  groups: CodeLensGroup[];
  refs: CodeLensRef[];
  /// CT-F2 — whether every ref's `when_written` was actually computed
  /// against a declared rev this request. `"none"` both when the caller
  /// never passed `?at=declared` AND when they did but the doc had no
  /// usable `kb-code-rev` (a future `"session"` era is a recorded
  /// follow-up, not yet built).
  era: "none" | "declared";
  note: string;
}

export interface ScorecardRepoRow {
  name: string;
  root: string;
  state: LensRepoState;
  head_sha: string | null;
  head_branch: string | null;
  dirty: boolean | null;
  /// All four are `null` on a non-`ready` repo — an unscored repo reports
  /// nothing rather than a zero that reads like a verdict.
  present: number | null;
  ambiguous: number | null;
  absent: number | null;
  external: number | null;
  partial: boolean;
  reason: string | null;
}

/// `GET /api/doc-lens/repos?kb=&doc=` body (`codelens-scorecard/1`).
export interface ScorecardOut {
  schema: string;
  kb: string;
  doc_id: string;
  doc_hash: string | null;
  doc_title: string | null;
  never_scanned: boolean;
  resolved_unix: number;
  /// What a reader pre-selects its repo picker with; `null` when the doc
  /// has no pin.
  pinned_repo: string | null;
  counted_refs: number;
  truncated: boolean;
  /// Config order — the same submission-order determinism kb-server
  /// invariant #28 requires.
  repos: ScorecardRepoRow[];
  note: string;
}

/// `PUT`/`DELETE /api/doc-lens/pin` response body (`codelens-pin/1`).
export interface DocLensPinOut {
  schema: string;
  kb: string;
  doc_id: string;
  repo: string;
  /// The filesystem root the pin was CHOSEN against — what makes "same
  /// name, re-pointed at a different checkout" detectable at read time.
  repo_root: string;
  doc_hash: string | null;
  pinned_at: number;
}

/// `GET /api/doc-lens/resolve-path?kb=&path=` response body (R2/R20's
/// path-addressed lens entry ramp).
export interface DocLensResolvePathOut {
  doc_id: string;
}

// --- DCB W3.A/B — the `doc_refs` reverse "cited by" index -------------------

/// One claim (`doc-refs/1`'s `DocRefClaim`, `crates/kb-code-server/src/
/// doclens/sync.rs`) — a document that cited the file named by the
/// surrounding `DocRefsOut.path`. Deliberately NOT the full stored row:
/// `head_sha`/`dirty`/`doc_hash`/`kind` are persisted server-side but unread
/// by any v1 consumer (that server-side doc comment's own rule: "a wire
/// field with no reader is a field that drifts").
export interface DocRefClaim {
  kb: string;
  doc_id: string;
  doc_title: string;
  doc_path: string;
  /// Built server-side by `doclens::doc_href` — the ONE kb link-out builder
  /// in kb-code (R1/m29). `null` when `[kb_daemon]` has no usable public
  /// base; render the title as plain text rather than a dead `href` (never
  /// re-derive a URL client-side). Links to the CITING DOC in kb, not to
  /// this file — unaffected by `DocRefsOut.live`, so it stays clickable even
  /// when the cited path has rotted (the doc that mentioned this file is
  /// still a real, live kb artifact either way).
  doc_public_href: string | null;
  group_label: string | null;
  raw_hint: string;
  line_start: number | null;
  line_end: number | null;
  line_state: string | null;
  seen_at: number;
}

/// `GET /api/doc-refs?repo=&path=` body (`doc-refs/1`).
export interface DocRefsOut {
  schema: string;
  repo: string;
  path: string;
  /// `Store::get_file(repo_id, path).is_some()`, checked NOW (not at sync
  /// time) — the SOLE "rotted claim" signal. Every claim in one response is
  /// a claim about the SAME path, so liveness is a property of the
  /// response, never of one row. `false` ⇒ render "path no longer present";
  /// per-claim doc links stay live regardless (see `DocRefClaim.doc_public_href`).
  live: boolean;
  claims: DocRefClaim[];
}

// ── PRR-U2 ── kb v0.39 "The PR Room," unit U2 (Room cockpit + Report tab) ──
//
// Hand-typed against `crates/kb-code-server/src/reviews.rs` (PRR-R2 block,
// report/artifact/PR-binding routes), `review_findings.rs` (PRR-R3,
// `finding_json`'s wire builder — the ONE JSON shape every findings route
// returns through), `github.rs` (`PrDetailOut`/`CheckRunOut`/`PrReviewsOut`/
// `ReviewerStateOut`), and `routes.rs`'s `pr_route`/`pr_checks_route`/
// `pr_reviews_route` envelopes. See `/tmp/design-server.md` §1–2 for the
// route table this mirrors.

/// `review_findings.rs`'s 3-value severity vocab — NOT the mock/design-ui.md
/// draft's `blocker|concern|nit|praise|info` (a plan arbitration override:
/// the server's actual vocab wins, see `store::is_valid_severity`).
export type FindingSeverity = "blocker" | "concern" | "ok";

/// `store::is_valid_disposition` — NOT the mock's `agree|dispute|waive|
/// fixed|follow-up` (same override: `fixed`→`fix-later` is the server's
/// real 4th state, and there is no separate `follow-up`).
export type FindingDispositionState = "agree" | "dispute" | "waive" | "fix-later";

export type FindingLocationKind = "single" | "range" | "multi" | "whole_file";

export interface FindingLocation {
  kind: FindingLocationKind;
  path: string;
  lines: number[] | null;
  removed: boolean;
}

export interface FindingEvidence {
  lang: string | null;
  source: string | null;
}

export interface FindingDisposition {
  state: FindingDispositionState;
  note: string | null;
  by: string | null;
  at: number | null;
}

/// `origin` — `"import"` (agent, `findings/import`, default author
/// `"claude"`) vs `"manual"` (human-authored, default author `"you"`) —
/// `store::FINDING_ORIGIN_*`. This is the field `FindingCard`'s author-mark
/// branch (✳ agent vs a plain "you" chip) keys on, not `author` itself
/// (an import batch can set any `author` string).
export type FindingOrigin = "import" | "manual";

export type FindingResolutionConfidence = "exact" | "fuzzy" | "orphaned";

export interface FindingResolution {
  line: number | null;
  line_end: number | null;
  orphaned: boolean;
  confidence: FindingResolutionConfidence;
}

/// The ONE finding wire shape (`review_findings::finding_json`) — shared,
/// byte-identical, by `GET .../findings`'s list rows, `POST .../findings`
/// (manual create), and both disposition routes' single-finding response.
export interface ReviewFinding {
  slug: string;
  severity: FindingSeverity;
  category: string;
  location: FindingLocation;
  title: string;
  rationale: string;
  recommendation: string | null;
  evidence: FindingEvidence | null;
  origin: FindingOrigin;
  author: string;
  disposition: FindingDisposition | null;
  published_state: string;
  published_at: number | null;
  published_url: string | null;
  superseded: boolean;
  superseded_reason: string | null;
  content_updated_at: number | null;
  annotation_id: string;
  import_batch_id: string;
  created_at: number;
  updated_at: number;
  resolution: FindingResolution;
  thread_count: number;
  unresolved_count: number;
}

/// `GET /api/reviews/{id}/findings?ps=&disposition=&include_superseded=`
/// body (`review-findings/1` — the house "every list route wraps its array"
/// convention; `review_findings.rs`'s own doc flags this as a deliberate
/// deviation from the design doc's bare-`[...]` row-9 sketch).
export interface ReviewFindingsOut {
  schema: string;
  review_id: number;
  repo: string;
  ps: number;
  findings: ReviewFinding[];
}

/// `PUT /api/reviews/{id}/findings/{slug}/disposition` body.
export interface SetFindingDispositionInput {
  disposition: FindingDispositionState;
  note?: string;
  author?: string;
}

// --- PRR-R2 report / artifact-hint / PR-binding ----------------------------

/// `reviews::get_review_report`'s stats sub-object — a best-effort, TOLERANT
/// projection. `report_json` is an OPAQUE, agent-authored, wholesale-replaced
/// blob (design-server.md §1.2: "one owner writes this WHOLESALE") — the
/// route table (§2 row 4) only pins FIVE key names (`summary`, `risk_score`,
/// `stats`, `authored_by`, `session_id`) plus `schema`/`ps_number`/
/// `generated_at`; the internal shape of `stats` and the verdict
/// headline/body/deck fields §4.1 describes (generator writes "verdict
/// headline/body/risk-score/stats … deck + .v-body") are NOT nailed down by
/// any landed route or test. Every field here is therefore OPTIONAL — a
/// report authored before a field existed, or one omitting a field this
/// unit assumed, must render as an honest absence, never a crash (§8 "the
/// room never lies").
export interface ReviewReportStats {
  blockers?: number;
  concerns?: number;
  verified?: number;
}

/// `GET /api/reviews/{id}/report` when a report exists — the raw stored
/// JSON object, echoed back verbatim (never re-shaped server-side). See this
/// interface's own file-level doc for why every field is optional.
export interface ReviewReport {
  schema?: string;
  /// One-line synopsis rendered under the header (mock's `.deck`).
  deck?: string;
  /// Section 01 markdown body — rendered via `lib/markdownLite.ts`.
  summary?: string;
  risk_score?: number;
  verdict?: FindingSeverity;
  verdict_headline?: string;
  verdict_body?: string;
  stats?: ReviewReportStats;
  authored_by?: string;
  session_id?: string;
  ps_number?: number;
  /// Server-STAMPED on every `PUT` (`put_review_report`'s doc) — always
  /// present on a report that has ever been written, never client-supplied.
  generated_at?: number;
  /// PRR-R2 §4.2 — the artifact↔review join hint, when the report itself
  /// carries one (distinct from `reviews.artifact_hint_*`, which rides
  /// `PATCH /api/reviews/{id}` — see `ReviewArtifactOut` below).
  artifact_ref?: { kb?: string; id?: string } | null;
}

/// `GET /api/reviews/{id}/report`'s NO-REPORT shape — `{report: null}`
/// verbatim (`get_review_report`'s doc). Every OTHER shape from that route
/// is a bare `ReviewReport` object with no `report` key at all (the stored
/// blob is echoed as-is) — so `"report" in x && x.report === null` is the
/// only reliable presence test; see `hasReviewReport` in
/// `components/reviews/ReportPanel.tsx`.
export interface ReviewReportEmpty {
  report: null;
}

export type ReviewReportOut = ReviewReport | ReviewReportEmpty;

/// `GET /api/reviews/{id}/artifact` body (design doc §2 row 7).
export interface ReviewArtifactOut {
  hint: { kb: string; id: string } | null;
  verified: boolean;
  doc?: {
    title: string;
    source_relative: string;
    kb_tags: string[];
    kb_category: string | null;
  } | null;
  unavailable_reason?: string;
}

/// PRR-R2's PR-binding fields on a review row (design-server.md §1.2's
/// additive `reviews` columns). **Not yet on the wire** as of this unit's
/// base commit (051bf227) — `GET /api/reviews/{id}` doesn't surface them
/// yet; a concurrent unit (R4) is landing that. Every field here is
/// therefore OPTIONAL by construction, and every consumer in this unit
/// (`PrChip`, the header's PR row) must render nothing when `pr_number` is
/// absent — never assume presence. Intersect onto `ReviewDetail` at the call
/// site (`review as ReviewDetailPr`) rather than editing that shared
/// interface directly.
export interface ReviewPrBinding {
  pr_number?: number;
  pr_repo_slug?: string;
  pr_head_sha?: string;
  pr_meta?: PrBindingMeta | null;
  pr_meta_unavailable_reason?: string | null;
}

/// The `pr_meta_json` snapshot shape (design-server.md §1.2's jsonc block) —
/// a point-in-time capture, not a live mirror (GitHub stays the source of
/// truth; see `CiChecksCard`/`PrChip`'s own docs for the live-vs-snapshot
/// split).
export interface PrBindingMeta {
  title?: string;
  author?: string;
  head_ref?: string;
  base_ref?: string;
  draft?: boolean;
  state?: string;
  merged?: boolean;
  labels?: string[];
  merge_state_status?: string | null;
}

export type ReviewDetailPr = ReviewDetail & ReviewPrBinding;

// --- github.rs raw PR reads (PRR-R2 row 2/3, addendum-2 §A) ---------------

/// `github::PrDetailOut` — `GET /api/prs/{number}`'s `pr` field. A SEPARATE
/// shape from the existing `PrOut` (list row) above — see that Rust struct's
/// own doc for why (fields only the single-PR endpoint populates).
export interface PrDetailOut {
  number: number;
  title: string;
  author: string;
  head_sha: string;
  head_ref: string;
  base_ref: string;
  updated_at: string;
  draft: boolean;
  state: string;
  merged: boolean;
  labels: string[];
  merge_state_status: string | null;
  /// V70-A3X — the PR's description body (raw markdown), `null` when
  /// GitHub reports an empty description.
  body: string | null;
}

/// `GET /api/prs/{number}` body (`pr-detail/1`) — `pr: null` on any
/// GitHub-side failure, honestly named via `unavailable_reason` (200, never
/// a 5xx — same degrade posture as `PrsResponse`/`PrCommentsResponse`).
export interface PrDetailResponseOut {
  schema: string;
  pr: PrDetailOut | null;
  unavailable_reason?: string;
}

/// `github::CheckRunOut` — one normalized GitHub Checks API run.
export interface CheckRunOut {
  name: string;
  /// `"pass"` | `"fail"` | `"warn"` | `"pending"` — `normalize_check_status`.
  status: string;
  note?: string;
  duration?: number;
}

/// `GET /api/prs/{number}/checks` body (`pr-checks/1`).
export interface PrChecksOut {
  schema: string;
  checks: CheckRunOut[];
  truncated: boolean;
  unavailable_reason?: string;
}

/// `github::ReviewerStateOut` — one reviewer's latest submitted state.
export interface ReviewerStateOut {
  reviewer: string;
  /// `APPROVED | CHANGES_REQUESTED | COMMENTED | DISMISSED | PENDING`.
  state: string;
  submitted_at: string | null;
}

/// `GET /api/prs/{number}/reviews` body (`pr-reviews/1`, addendum-2 §A).
export interface PrReviewsOut {
  schema: string;
  reviewers: ReviewerStateOut[];
  requested_reviewers: string[];
  /// Locally-computed `reviewDecision` approximation — never
  /// `"REVIEW_REQUIRED"` (see the Rust struct's own doc: this REST-only
  /// client can't see branch-protection rules).
  review_decision?: string | null;
  unavailable_reason?: string;
}

// ── PRR-U3 ── review findings — diff-overlay aliases -----------------------
//
// U3 and U2 landed concurrently and both typed the kbc-findings/1 wire.
// U2's declarations above are the canonical (stricter) ones; U3's names
// survive as aliases so its diff-surface consumers keep compiling, plus its
// one genuinely unique shape (the manual-create input, addendum §E).

export type ReviewFindingLocation = FindingLocation;
export type ReviewFindingEvidence = FindingEvidence;
export type ReviewFindingDispositionState = FindingDisposition;
export type ReviewFindingResolution = FindingResolution;

/// `POST /api/reviews/{id}/findings` body (addendum §E, LOOPBACK-ONLY) — one
/// human-authored finding, code-anchored (no path-less/general finding —
/// enforced client-side by the composer's copy, not a server 400).
export interface CreateManualFindingInput {
  slug?: string;
  severity: FindingSeverity;
  category: string;
  location: FindingLocation;
  title: string;
  rationale: string;
  recommendation?: string;
  author?: string;
}

// ── PRR-U1 ── kb v0.39 "The PR Room," unit U1 (Review Room landing) ────────
//
// Hand-typed against `crates/kb-code-server/src/review_inbox.rs`
// (`review-inbox/1`) + `reviews.rs`'s `create_review_pr` (PRR-R2) + the R4
// `pr_binding_and_report_fields` splice onto `list_reviews`'s rows.

/// One row of `GET /api/reviews/inbox` (`review-inbox/1`,
/// `review_inbox::InboxRow`'s JSON composition — see that module's own doc
/// for the full scoring/derivation contract). `verdict` here is the
/// review's own HUMAN pass verdict (`verdict_block`'s shape, reused
/// verbatim — same `ReviewVerdict` as `ReviewSummary.verdict`), **not** an
/// agent report verdict: the inbox route deliberately never re-derives or
/// echoes the report's `blocker|concern|ok` enum (see `pr_binding_and_
/// report_fields`'s doc — only `has_report`/`report_risk_score` are cheap
/// enough to surface on a list route). `lib/reviewInbox.ts` joins this row
/// against the reviews list (`ReviewSummaryPr`) client-side for those
/// report-summary fields. `pr_number`/`pr_head_drift` are `null` for a
/// non-PR-bound review — never coerced to `false`/absent.
export interface ReviewInboxRow {
  review_id: number;
  repo: string;
  pr_number: number | null;
  title: string | null;
  unresolved_findings: number;
  unanswered_questions: number;
  verdict: ReviewVerdict | null;
  verdict_stale: boolean;
  pr_head_drift: boolean | null;
  updated_at: number;
}

/// `GET /api/reviews/inbox?repo=&state=&limit=` body.
export interface ReviewInboxOut {
  schema: string;
  reviews: ReviewInboxRow[];
}

/// `ReviewSummary` (`GET /api/reviews` list row) intersected with the R4
/// additive PR-binding + report-summary fields — the SAME "intersect at the
/// call site" convention `ReviewDetailPr` already established for the
/// detail route (see that type's own doc), just for the list route
/// instead. Every field is optional: an older daemon's list response
/// simply won't carry them, and every consumer must treat absence as
/// "unknown", never as a false/zero value.
export type ReviewSummaryPr = ReviewSummary &
  ReviewPrBinding & {
    has_report?: boolean;
    report_risk_score?: number | null;
  };

/// `POST /api/reviews/pr` body (`create_review_pr`'s `CreateReviewPrBody`,
/// design doc §2 row 1 / PRR-R2). LOOPBACK-ONLY.
export interface CreateReviewPrInput {
  repo: string;
  pr_number: number;
  base_ref?: string;
  title?: string;
  session_id?: string;
}

/// `POST /api/reviews/pr`'s 201 body — deliberately its OWN shape (not
/// `ReviewDetailPr`/`CreateReviewOut`): `create_review_pr` composes this
/// JSON object by hand and it carries `tip_sha`/`base_sha`, which `GET
/// /api/reviews/{id}` does not. A 409 (already bound to a review) instead
/// returns `{error, existing_review_id}` — surfaced to callers only as
/// `ApiError.message` (see `createReviewPr`'s doc in `api/client.ts` for
/// why `existing_review_id` isn't separately typed here).
export interface CreateReviewPrOut {
  schema: string;
  id: number;
  repo: string;
  title: string | null;
  base_ref: string;
  head_ref: string;
  session_id: string | null;
  state: ReviewState;
  created_at: number;
  updated_at: number;
  latest_ps: number;
  tip_sha: string;
  base_sha: string;
  pr_number: number;
  pr_repo_slug: string;
  pr_head_sha: string;
  pr_meta: PrBindingMeta | null;
  pr_meta_unavailable_reason: string | null;
}

// ── PRR-U56 ── kb v0.39 "The PR Room," combined unit U5+U6 (publish
// preview + suggestions batch card + timeline tab). Hand-typed against
// `crates/kb-code-server/src/review_github_export.rs` (`GET .../export/
// github`, `POST .../findings/{slug}/published`, `POST .../verdict/
// published`), `review_timeline.rs` (`GET .../timeline`), and
// `suggestions.rs`'s `POST /api/annotations/apply-batch` (addendum-2 §F).
// Appended as its own block — never edits an existing interface — per this
// unit's file-ownership note (design-ui.md §2 S5 / §2 S4).

export type GithubExportEvent = "APPROVE" | "REQUEST_CHANGES" | "COMMENT";

export interface GithubExportComment {
  path: string;
  line: number | null;
  line_end: number | null;
  side: "LEFT" | "RIGHT";
  body: string;
  finding_slug: string;
  orphaned: false;
}

/// `classify_for_export`'s `SkipReason::wire()` vocab — WIDER than
/// `resolution.orphaned` alone (see that Rust module's doc): a `whole_file`/
/// `multi` finding also lands here even though its anchor itself resolved.
export type GithubExportSkipReason = "orphaned" | "whole_file" | "multi_line";

export interface GithubExportGeneralComment {
  body: string;
  finding_slug: string;
  reason: GithubExportSkipReason;
}

export interface GithubExportSkipped {
  finding_slug: string;
  reason: GithubExportSkipReason;
  original: { ps: number; line: number | null };
}

/// `GET /api/reviews/{id}/export/github?finding_slugs=&include_waived=
/// &include_orphaned_as_general=` body (`kbc-github-export/1`). Bearer,
/// pure computation — the daemon never calls GitHub for this route.
export interface ReviewGithubExportOut {
  schema: string;
  review_id: number;
  repo: string;
  ps_number: number;
  event: GithubExportEvent | null;
  event_reason: string | null;
  body: string | null;
  commit_id: string;
  comments: GithubExportComment[];
  general_comments: GithubExportGeneralComment[];
  skipped_orphaned: GithubExportSkipped[];
  stale_export: boolean;
}

/// `POST /api/reviews/{id}/findings/{slug}/published` / `.../verdict/
/// published` bodies — every field optional + purely advisory
/// (`github_comment_id`/`github_review_id` are accepted but NOT persisted,
/// per those routes' own doc — this client never sends them).
export interface PublishFindingInput {
  github_comment_url?: string;
  published_at?: number;
}
export interface PublishVerdictInput {
  github_review_url?: string;
  published_at?: number;
}
export interface PublishVerdictOut {
  id: number;
  verdict_published_at: number | null;
  verdict_published_url: string | null;
}

/// `GET /api/reviews/{id}/timeline` body (`review-timeline/1`). Event
/// payloads vary per `kind` (the server's own closed vocab —
/// `review_timeline.rs`'s module doc); typed as a loose record here —
/// `lib/reviewTimeline.ts`'s `timelineRow` is the ONE place that narrows
/// per-kind, so an unrecognized future kind degrades to a plain label
/// instead of a shape mismatch anywhere a consumer reads this type.
export interface ReviewTimelineEvent {
  at: number;
  kind: string;
  [key: string]: unknown;
}
export interface ReviewTimelineOut {
  schema: string;
  review_id: number;
  repo: string;
  events: ReviewTimelineEvent[];
}

// --- PRR-R10 — multi-file suggestion batch apply (addendum-2 §F) ----------

export interface ApplyBatchVerdictError {
  kind: string;
  detail: string;
}
export interface ApplyBatchVerdict {
  id: string;
  ok: boolean;
  error?: ApplyBatchVerdictError;
}
/// `POST /api/annotations/apply-batch` 409 body — the verify-phase failed
/// for at least one id; NOTHING was written to any file.
export interface ApplyBatchConflictOut {
  verdicts: ApplyBatchVerdict[];
}
export interface AppliedBatchItem {
  id: string;
  path: string;
  line: number;
  line_end?: number;
}
/// `POST /api/annotations/apply-batch` 200 body (every file landed) OR the
/// rarer mid-batch-IO-failure 500 body (`applied`/`restored` populated,
/// `failed` non-null) — see `suggestions.rs`'s `restore_and_report` doc.
export interface ApplyBatchResultOut {
  applied: AppliedBatchItem[];
  restored: string[];
  failed: { id: string; path: string; error: string } | null;
}

// ── PRR-U9 — diagnostics through lip (design-addendum-2.md §D's UI unit) ──
//
// Wire mirrors of `crates/kb-code-server/src/lip.rs`'s `RepoIntelStatus`
// (`GET /api/repos`'s per-repo `intel` field) and `DiagnosticOut`/
// `DiagnosticsOut` (`GET /api/diagnostics`). `RepoListEntry` is
// declaration-merged (TS interfaces merge across `export interface` blocks
// with the same name) rather than edited in place, so this unit's file
// ownership stays append-only here.

/// PRR-L2 — this repo's configured lip/1 provider status
/// (`crate::lip::LipRegistry::status_for_repo`), or `null` when no
/// `[[intel.providers]]` entry names this repo. Computed from the cached
/// handshake state server-side — never a fresh network probe, so `alive`
/// can lag a provider's real live status by up to one handshake interval.
export interface RepoIntelStatus {
  provider: string;
  langs: string[];
  alive: boolean;
  server_version: string | null;
}

/// Declaration-merged onto `RepoListEntry` (declared above) — additive
/// field, always present on the wire (no `skip_serializing_if` server-side),
/// `null` when no provider covers this repo.
export interface RepoListEntry {
  intel: RepoIntelStatus | null;
}

/// One `lip.rs::DiagnosticOut` row — 1-based `line`/`col`; `end_line`/
/// `end_col` are `0` when the provider didn't report a range end (never
/// treat `0` as a real line number — see `lib/diagnostics.ts`'s
/// `diagnosticGutterMarks` for how that's handled). `severity` is the RAW
/// LSP `DiagnosticSeverity` int (1=Error, 2=Warning, 3=Information, 4=Hint;
/// `null` = the provider didn't report one) — `lib/diagnostics.ts`'s
/// `severityLabel` is the ONE place that maps it to a display label; no
/// other module re-derives that mapping.
export interface DiagnosticRow {
  line: number;
  col: number;
  end_line: number;
  end_col: number;
  severity: number | null;
  /// `Option<serde_json::Value>` server-side — an opaque diagnostic code
  /// (usually a string or number). Rendered via `String()`, never matched on.
  code: string | number | boolean | null;
  source: string | null;
  message: string;
}

/// `GET /api/diagnostics?repo=&path=` response (`lip.rs::DiagnosticsOut`/
/// `DIAGNOSTICS_SCHEMA`). `diagnostics: null` ≠ `[]` — `null` means no
/// provider/refused (see `unavailable_reason`'s closed vocabulary below),
/// `[]` means the provider ran and reports the file clean.
/// `unavailable_reason`: `"unknown_language"` | `"no_provider_configured"` |
/// `"file_unreadable"` | `"provider_unavailable"` | `"blob_stale"` — an
/// unrecognized value (a newer daemon) degrades to its own verbatim text,
/// same forward-compat posture `lib/diffFindings.ts`'s `severityLabel` uses.
export interface DiagnosticsOut {
  schema: string;
  path: string;
  diagnostics: DiagnosticRow[] | null;
  provider: string | null;
  fetched: boolean;
  unavailable_reason: string | null;
}

// --- PRR-F — kb v0.39 T2 frontier unit: GitHub threads, recurring-finding
// memory, reviewer X-ray (design-ui.md §12 items 1/2/4, design-addendum-2
// §A). Own block, appended at the file's tail per this file's own "own
// import statement / own block" precedent (PRR-U1/U2/U3 above) — never
// touches a line a sibling builder might also be editing.

/// `GET /api/reviews/{id}/github-threads` (`kbc-github-threads/1`,
/// design-addendum-2 §A). ONE of `general`/`orphaned`/`resolved` is present
/// — never more than one, per `review_github_threads.rs`'s `attach_position`
/// doc. `replies` is always present (possibly empty) on a ROOT thread only;
/// a reply itself carries no `replies`/position fields at all (GitHub's
/// `in_reply_to_id` nests exactly one level — the route's own doc).
export interface GithubThreadComment {
  /// `Option<u64>` server-side, but composed via a manual `serde_json::json!`
  /// call (`comment_json`) rather than the struct's own derived
  /// `Serialize` — every field below is ALWAYS a present key, `null` when
  /// absent (never omitted), unlike `PrCommentOut`'s OTHER consumer
  /// (`GET /api/prs/{n}/comments`, which DOES omit `None` fields via
  /// `skip_serializing_if`). Don't assume the two routes share a wire shape.
  id: number | null;
  author: string;
  body: string;
  created_at: string;
  html_url: string | null;
  path: string | null;
  side: string | null;
  line: number | null;
  original_line: number | null;
  in_reply_to: number | null;
}

export interface GithubThread extends GithubThreadComment {
  /// `true` for an issue-style (non-inline) comment — never anchored.
  general?: boolean;
  /// `true` when a `path`-carrying comment's line couldn't be re-resolved
  /// against the review's latest patchset (an honest miss, never a guessed
  /// line — see the route's own doc).
  orphaned?: boolean;
  /// Present only when the comment resolved cleanly onto the latest ps.
  resolved?: { line: number; confidence: string };
  replies: GithubThreadComment[];
}

export interface GithubThreadsOut {
  schema: string;
  review_id: number;
  repo: string;
  pr_number: number;
  ps: number;
  threads: GithubThread[];
  truncated: boolean;
  fetched_at: number;
  unavailable_reason: string | null;
}

/// `GET /api/reviews/{id}/findings/recurrence` (design-ui.md §12.4).
export interface FindingRecurrencePriorReview {
  review_id: number;
  pr_number?: number;
  title: string | null;
  created_at: number;
}
export interface FindingRecurrenceRow {
  slug: string;
  seen_in_reviews: number;
  prior: FindingRecurrencePriorReview[];
}
export interface FindingsRecurrenceOut {
  schema: string;
  review_id: number;
  repo: string;
  findings: FindingRecurrenceRow[];
}

/// `GET /api/reviews/{id}/impact?path=` (design-ui.md §12.2, "Reviewer
/// X-ray"; `crates/kb-code-server/src/review_impact.rs`'s own module doc
/// has the full "why a new small aggregate" rationale). `callers_total` /
/// `callers_in_diff` / `callers_out_of_diff` count EXACT/LIKELY call sites
/// only — see that route's `SUPPORTED_LANG_NOTE`.
export interface ReviewImpactChangedSymbol {
  name: string;
  kind: string;
  line: number;
  col: number;
  callers_total: number;
  callers_in_diff: number;
  callers_out_of_diff: number;
  truncated: boolean;
}
export interface ReviewImpactFileOut {
  schema: string;
  review_id: number;
  repo: string;
  ps_number: number;
  path: string;
  lang_supported: boolean;
  changed_symbols: ReviewImpactChangedSymbol[];
  callers_total: number;
  callers_in_diff: number;
  callers_out_of_diff: number;
  symbols_truncated: boolean;
  note: string;
}

// ── PRR-U8 (design-addendum-2.md §C) — the disposition CALIBRATION
// instrument, `GET /api/reviews/analytics?repo=&from=&to=`
// (`crates/kb-code-server/src/review_analytics.rs`'s own module doc has the
// full aggregation rationale — every term named, no fabricated zero: a
// `rate`/`median_secs`/`p90_secs` is `null`, never `0`, when its own
// denominator is zero). Deterministic, decomposed, never a quality verdict.
export interface AnalyticsSeverityDispositionCell {
  severity: FindingSeverity;
  disposition: string;
  count: number;
}
export interface AnalyticsAcceptanceRow {
  severity: FindingSeverity;
  accepted: number;
  rejected: number;
  risk_accepted: number;
  undecided: number;
  total: number;
  /// `null` — never a fabricated `0` — when nothing has been decided yet
  /// (the module doc's own "decided-only" denominator reading).
  rate: number | null;
}
export interface AnalyticsCategoryRow {
  category: string;
  count: number;
  accepted: number;
  rejected: number;
  risk_accepted: number;
  undecided: number;
}
export interface AnalyticsWeekBucket {
  week_start_unix: number;
  created: number;
  disposed: number;
  disputed: number;
}
export interface AnalyticsLatencyOut {
  n: number;
  median_secs: number | null;
  p90_secs: number | null;
}
export interface AnalyticsPublishOut {
  published: number;
  unpublished: number;
}
/// The SAME shared `Store::recurrence_pairs` query `GET /api/reviews/{id}/
/// findings/recurrence`'s `FindingRecurrenceRow` uses, at the corpus level
/// (every `(category, location_path)` pair recurring across `>=
/// store::RECURRENCE_MIN_REVIEWS` distinct reviews) rather than one
/// review's own findings.
export interface AnalyticsRecurrenceRow {
  category: string;
  location_path: string;
  review_count: number;
  finding_count: number;
  review_ids: number[];
}
export interface ReviewAnalyticsOut {
  schema: string;
  repo: string | null;
  from: number | null;
  to: number | null;
  total_findings: number;
  superseded_count: number;
  by_severity_disposition: AnalyticsSeverityDispositionCell[];
  acceptance: AnalyticsAcceptanceRow[];
  by_category: AnalyticsCategoryRow[];
  weekly: AnalyticsWeekBucket[];
  latency: AnalyticsLatencyOut;
  publish: AnalyticsPublishOut;
  recurrence: AnalyticsRecurrenceRow[];
}

// ── S2-A: unified inbox (kb-code v6.0 "One Inbox", design-s2.md §S2-A) ────
//
// `GET /api/inbox` (`unified-inbox/1`, NEW `crates/kb-code-server/src/
// unified_inbox.rs`) — three lanes, never a merged cross-lane score
// (surfaced-never-scored: incommensurable units, each keeps its own source
// ordering). Hand-typed against the design doc's PINNED wire shape — this
// worktree forked before the server route landed, so there is no Rust
// source to type against yet.

/// One row of the "working-tree questions" lane — an open (unresolved,
/// `review_id IS NULL`) annotation with `intent` in `("question",
/// "flag-for-agent")`, across every configured repo, newest `updated_at`
/// first. Distinct from `OpenAnnotationEntry` (that route's full
/// `AnnotationView` splice) — the unified inbox row is deliberately a
/// smaller, cross-repo-cheap projection (an `excerpt`, not the full body).
export interface UnifiedInboxAnnotationRow {
  id: string;
  repo: string;
  path: string;
  /// Absent/`null` when the annotation's anchor carries no single line
  /// (defensive — every v1 annotation kind does today, but the row must
  /// not assume it forever).
  line?: number | null;
  intent: string;
  author: string;
  /// First 200 chars of the annotation body.
  excerpt: string;
  reply_count: number;
  updated_at: number;
}

/// Closed vocabulary for `kb.reason` — mapped server-side from
/// `KbClientError` (`BadStatus`/`Parse` → `"unreachable"`). An unknown
/// string (a future daemon's new reason) still round-trips as a plain
/// `string` at the type level; `lib/unifiedInbox.ts`'s `kbLaneState`
/// degrades any value outside this list to a generic label rather than
/// throwing.
export type KbLaneReason = "disabled" | "unreachable" | "sibling_mismatch";

/// kb's `GET /api/desk` item (`web/src/api/generated/DeskListItem.ts`),
/// relayed VERBATIM by kb-code — kb-code does NOT re-model kb's own type,
/// so every field is optional here and an index signature absorbs any
/// additive kb-side field without a kb-code release (design doc: "a
/// relay, so kb-side additive fields flow through without a kb-code
/// release"). The fields below are the ones this unit's UI actually
/// reads.
export interface KbDeskItemRelay {
  kb?: string;
  id?: string;
  source_relative?: string;
  title?: string;
  updated_unix?: number;
  comments_open?: number;
  comments_total?: number;
  read_state?: string;
  changed_since_read?: boolean;
  [key: string]: unknown;
}

export interface UnifiedInboxKbDesk {
  items: KbDeskItemRelay[];
  attention: number;
  /// Present + `true` only when kb-code truncated the relay after fetch
  /// (design doc: "kb desk/comments relayed at most 50 items each").
  truncated?: boolean;
}

/// kb's `GET /api/inbox` item (`web/src/api/generated/InboxItem.ts`),
/// relayed verbatim — same optional-fields-plus-index-signature posture
/// as `KbDeskItemRelay` above.
export interface KbCommentItemRelay {
  kb?: string;
  artifact_id?: string;
  source_relative?: string;
  title?: string;
  comment_id?: string;
  excerpt?: string;
  author?: string;
  reply_count?: number;
  updated_at?: number;
  [key: string]: unknown;
}

export interface UnifiedInboxKbComments {
  items: KbCommentItemRelay[];
  total_open: number;
  truncated?: boolean;
}

/// The "From kb" lane. `available: false` degrades HONESTLY and partial-
/// tolerant — `reason` is set, `desk`/`comments` are `null`, and the route
/// never 500s because kb is down (design doc's own wording).
export interface UnifiedInboxKb {
  available: boolean;
  reason: KbLaneReason | null;
  desk: UnifiedInboxKbDesk | null;
  comments: UnifiedInboxKbComments | null;
}

export interface UnifiedInboxOut {
  schema: string;
  /// `review-inbox/1` rows, ALL repos, server order (`sort_inbox_rows`)
  /// preserved — never re-sorted client-side, same convention as
  /// `ReviewInboxOut.reviews` above. Capped at 50.
  reviews: ReviewInboxRow[];
  annotations: UnifiedInboxAnnotationRow[];
  kb: UnifiedInboxKb;
}

// ── S2-C/S2-B/S2-D (B4) — quick fixes + remote_mutations chip +
// intel_providers read (design-s2.md §S2-C/§S2-B/§S2-D + Addendum). Own
// block, own additive declaration-merges — never touches a line a sibling
// builder might also edit (same "own block" precedent every PRR-* section
// above already establishes; B3 also appends to this file, elsewhere).

/// One translated `WorkspaceEdit` range (`kb-lip`'s `/lip/code-actions`
/// wire codec: 1-based lines, 0-based BYTE columns — the SAME codec every
/// lip/1 endpoint uses, `DiagnosticRow`'s own doc above). `new_text` may
/// itself contain embedded `\n` for a multi-line replacement.
export interface CodeActionEdit {
  start_line: number;
  start_col: number;
  end_line: number;
  end_col: number;
  new_text: string;
}

/// One file's worth of edits within a single code action.
export interface CodeActionFileEdit {
  path: string;
  edits: CodeActionEdit[];
}

/// One `POST /api/code-actions` result row (`code-actions/1`).
export interface CodeActionRow {
  title: string;
  kind: string | null;
  is_preferred: boolean;
  edits: CodeActionFileEdit[];
}

export interface CodeActionsDropped {
  command_only: number;
  unsupported: number;
}

/// `POST /api/code-actions` (`code-actions/1`, design-s2.md Addendum —
/// EXACT pin, B1 emits/B4 consumes). `available:false` carries `reason` in
/// the SAME closed vocabulary `DiagnosticsOut.unavailable_reason` uses PLUS
/// `"capability_absent"` (a configured provider without code-actions
/// support) — `lib/codeActions.ts`'s `unavailableCodeActionsReasonLabel` is
/// the one place that maps it to display text, same forward-compat posture
/// `lib/diagnostics.ts`'s `unavailableReasonLabel` uses for its own closed
/// vocabulary.
export interface CodeActionsOut {
  schema: string;
  available: boolean;
  verified: boolean;
  reason: string | null;
  provider: string | null;
  actions: CodeActionRow[];
  dropped: CodeActionsDropped;
}

/// `GET /api/identity` additive field (design-s2.md §S2-B) — declaration-
/// merged onto `IdentityOut` (declared above) rather than edited in place,
/// so this unit's addition stays append-only here. Absent on a daemon built
/// before this landed OR when the operator never set `[review]
/// remote_mutations` — the SPA treats "absent" and "false" identically (the
/// capability chip's own doc, `routes/Home.tsx`).
export interface IdentityOut {
  remote_mutations?: boolean;
}

/// `GET /api/repos` additive field (design-s2.md §S2-D) — declaration-
/// merged onto `RepoListEntry` (declared above, and ALREADY merged once by
/// PRR-U9's `intel: RepoIntelStatus | null` block) for the SAME reason:
/// every matching `[[intel.providers]]` entry for this repo, config order.
/// `intel` (first match) stays verbatim for back-compat. Absent or `[]` ⇒
/// callers fall back to `intel` (`lib/diagnostics.ts`'s
/// `effectiveIntelProviders`).
export interface RepoListEntry {
  intel_providers?: RepoIntelStatus[];
}

// --- V71-E2 — `usages/2` (D4) + `kbc-actions/1` (D5) ----------------------
//
// Mirrors `crates/kb-code-server/src/usages2.rs` and `src/actions.rs`. Both
// are computed per request and never persisted (root CLAUDE.md #2); nothing
// here is derived client-side that the server already states — above all a
// COUNT: `Usages2Out.totals`/`kind_totals` are the TRUE totals before the
// per-class cap, and `capped[]` names any class that hid rows. The census
// strip renders those numbers verbatim, which is what makes "a count never
// changes without an on-screen reason" enforceable rather than aspirational.

/// D4's CLOSED kind vocabulary, all 34 names in D4's own order. A consumer
/// may switch exhaustively; `unclassified` is a first-class outcome and is
/// never coerced into `call`.
export type UsageKind =
  | "def" | "decl" | "call" | "read" | "write" | "mutate" | "import"
  | "include" | "extend" | "prepend" | "inherit" | "override" | "alias"
  | "instantiate" | "typed" | "rescue" | "yield_to"
  | "symbol_mention" | "string_mention" | "send_dynamic" | "comment_mention"
  | "route" | "view_render" | "layout" | "helper" | "i18n_key"
  | "association" | "callback" | "job_enqueue" | "migration" | "fixture"
  | "factory" | "config_key"
  | "unclassified";

export interface UsageEnclosingSymbol {
  name: string;
  kind: string;
  line: number;
  container?: string | null;
}

export interface UsageRow2 {
  path: string;
  line: number;
  col: number;
  col_end?: number;
  blob_sha?: string;
  kind: UsageKind;
  /// SCIP's `SymbolRole` bitset (plus kb-code's `vendor` bit). Decoded
  /// names ride beside it — never re-derive the decode client-side.
  roles: number;
  role_names: string[];
  trust: string;
  precision: string;
  access?: "read" | "write";
  enclosing?: UsageEnclosingSymbol;
  context: string;
}

/// One class's cap report. Present in `capped[]` only when rows were hidden
/// — never a bare boolean, always with the TRUE total and the reason.
export interface UsagesCapped {
  group: string;
  returned: number;
  total: number;
  reason: string;
}

export interface UsagesTotals {
  exact: number;
  likely: number;
  candidate: number;
  all: number;
}

export interface RubyStrictNote {
  exact: boolean;
  verdict: string;
  hierarchy: string[];
  sites: number;
}

/// `GET /api/usages/2`'s body (`usages2::Usages2Out`).
export interface Usages2Out {
  schema: string;
  symbol: { name: string; kind?: string | null; container?: string | null };
  class_of_definition: string;
  exact: UsageRow2[];
  likely: UsageRow2[];
  candidate: UsageRow2[];
  totals: UsagesTotals;
  capped: UsagesCapped[];
  /// Count per kind over the TRUE totals, not the returned page.
  kind_totals: Record<string, number>;
  ruby_strict?: RubyStrictNote | null;
}

// --- kbc-actions/1 --------------------------------------------------------

/// D5's segmented control, as a CLOSED vocabulary.
export type ActionTargetKind = "symbol" | "range" | "text" | "path" | "enclosing";

export interface ActionTarget {
  kind: ActionTargetKind;
  label: string;
  path: string;
  line?: number;
  end_line?: number;
  col?: number;
  end_col?: number;
  name?: string;
  container?: string;
  text?: string;
  exists?: boolean;
  blob_sha?: string;
  note?: string;
}

/// What the CLIENT does for a row. CLOSED — `lib/actionOps.ts`'s handler is
/// exhaustive over it (a `never` check), so a new server-side variant fails
/// the SPA build instead of silently doing nothing.
export type ActionOp =
  | { op: "open"; path: string; line?: number; pane: number }
  | { op: "peek"; kind: string }
  | { op: "dock"; dock: string }
  | { op: "search"; query: string }
  | { op: "copy"; what: string; value?: string }
  | { op: "compose"; surface: string }
  | { op: "collect"; sink: string };

export interface ActionRequestSpec {
  method: string;
  path: string;
  query: [string, string][];
}

export interface ActionRow {
  id: string;
  version: number;
  group: string;
  key?: string;
  title: string;
  doc: string;
  enabled: boolean;
  disabled_reason?: string;
  mutating: boolean;
  /// Always `false` in v1 — `/api/actions` never runs the resolve ladder,
  /// so it cannot prove `exact`, and D5's rule is that candidate/likely
  /// never auto-navigate.
  auto_navigate: boolean;
  op: ActionOp;
  cli?: string;
  request?: ActionRequestSpec;
}

export interface ActionGroup {
  id: string;
  title: string;
  actions: ActionRow[];
}

/// Why the mutating group is (or is not) present — a property of the
/// CALLER, stated rather than left as a silent gap.
export interface ActionsMutationsNote {
  available: boolean;
  reason: string;
}

/// `GET /api/actions`'s body (`actions::ActionsOut`).
export interface ActionsOut {
  schema: string;
  repo: string;
  targets: ActionTarget[];
  active: number;
  groups: ActionGroup[];
  mutations: ActionsMutationsNote;
  notes: string[];
}

// --- V72-G1.1/G1.2 — `entity/1`, the entity DOSSIER --------------------------
//
// The TS mirror of `crates/kb-code-server/src/entities/dossier.rs`'s
// `DossierOut` tree (`GET /api/entity/dossier`). Hand-maintained in lock-step
// with that module, the same discipline every other wire type in this file
// takes; `lib/dossier.test.ts` walks the CHECKED-IN Rust golden
// (`crates/kb-code-server/tests/fixtures/entity-dossier.golden.json`) through
// these types, so a field renamed on the Rust side fails a test here by name
// rather than surfacing as an `undefined` on screen.
//
// Every `skip_serializing_if = "Option::is_none"` field is optional here and
// NOTHING substitutes a zero for an absent value — an absent `blob_sha` means
// "the file is no longer in the mirror", which is not the same fact as an
// empty string.

/// A census of trust classes. Never an aggregate verdict — `entities/1`'s
/// posture, kept (crate invariant 13).
export interface TrustCounts {
  exact: number;
  likely: number;
  candidate: number;
}

/// `dossier.rs`'s `ENTITY_KINDS`.
export type EntityKind = "class" | "module" | "constant" | "unknown";

/// `resolve.rs`'s `CLASS_*` vocabulary, plus `dossier.rs`'s honest fourth
/// outcome for a reference that resolves to nothing this index holds.
export type TrustClass = "exact" | "likely" | "candidate";
export type ResolvedClass = TrustClass | "unresolved";

export interface EntitySummary {
  fqn: string;
  kind: EntityKind;
  /// The enclosing constant path — absent at the top level.
  namespace?: string;
  /// EVERY file that reopens this entity, in response order.
  files: string[];
  trust_counts: TrustCounts;
}

/// `ruby_body.rs`'s `OPENER_*`.
export type OpenerForm = "top-level" | "nested" | "compact" | "mixed" | "unknown";

export interface DefinitionBlock {
  path: string;
  line_start: number;
  line_end: number;
  /// The blob these lines were read from — absent when the file is no longer
  /// in the mirror.
  blob_sha?: string;
  /// `class` | `module` | `reopen` | `constant`.
  kind: string;
  /// Position in the response's definition order (0-based).
  reopening_index: number;
  /// The literal opener chain, read off the source.
  opener: string;
  opener_form: OpenerForm;
  trust: TrustClass;
  matched_via: string;
  nesting: string;
  stale: boolean;
  /// `true` when the path carries no live bytes any more.
  missing: boolean;
}

/// `ruby_body.rs`'s `MEMBER_KINDS`.
export type MemberKind =
  | "instance_method"
  | "singleton_method"
  | "attr_reader"
  | "attr_writer"
  | "attr_accessor"
  | "constant"
  | "alias";

/// `ruby_body.rs`'s `VISIBILITIES`.
export type Visibility = "public" | "protected" | "private" | "module_function" | "unknown";

/// How a member row was FOUND — surfaced rather than folded into `trust`.
export type MemberVia = "tree" | "macro" | "assignment";

export interface MemberRow {
  name: string;
  kind: MemberKind;
  visibility: Visibility;
  /// The FQN whose body defines this member.
  defining_type: string;
  /// `true` only for rows that came from an ancestor or a mixin, and only
  /// when the request asked for them (`?inherited=1`).
  inherited: boolean;
  path: string;
  line: number;
  blob_sha?: string;
  via: MemberVia;
  trust: TrustClass;
}

export interface Ancestor {
  /// The constant AS WRITTEN at the reference site.
  written: string;
  fqn?: string;
  resolved: ResolvedClass;
  depth: number;
  from_path: string;
  from_line: number;
}

export interface Mixin {
  kind: string;
  written: string;
  fqn?: string;
  resolved: ResolvedClass;
  from_path: string;
  from_line: number;
}

export interface RelatedEntity {
  fqn?: string;
  via: string;
  path: string;
  line: number;
  trust: TrustClass;
}

export interface Hierarchy {
  ancestors: Ancestor[];
  mixins: Mixin[];
  descendants: RelatedEntity[];
  implementors: RelatedEntity[];
  notes: string[];
}

export interface UsageGroup {
  kind: string;
  /// The TRUE total for this kind, before `usages_per_kind` cut `rows`.
  total: number;
  truncated: boolean;
  trust_census: TrustCounts;
  /// What `trust_census` counted — `"returned"`, i.e. the rows in hand.
  census_basis: string;
  rows: UsageRow2[];
}

export interface UsageAnchor {
  path: string;
  line: number;
  col: number;
}

export interface UsagesSection {
  /// `ok` | `empty` | `error`.
  state: string;
  reason?: string;
  anchor?: UsageAnchor;
  groups: UsageGroup[];
  total: number;
  truncated: boolean;
}

export interface UnknownMember {
  /// One of `ruby_body.rs`'s `UNKNOWN_MECHANISMS`.
  mechanism: string;
  name_hint?: string;
  path: string;
  line: number;
  blob_sha?: string;
  context: string;
}

export interface NamespaceChild {
  fqn: string;
  /// The direct child SEGMENT under the addressed entity.
  segment: string;
  /// `class` | `module` | `namespace`.
  kind: string;
  definitions: number;
  descendants: number;
  trust_counts: TrustCounts;
}

/// Rows the BUDGET dropped, by lane. Every key is always present.
export interface DossierDropped {
  definitions: number;
  members: number;
  ancestors: number;
  mixins: number;
  descendants: number;
  implementors: number;
  unknown_members: number;
  namespace_tree: number;
  usages: number;
}

export interface BudgetReport {
  requested: number;
  spent: number;
  dropped: DossierDropped;
  /// The order the budget was spent in (`LANE_PRIORITY`).
  order: string[];
}

/// `ok` | `partial` | `empty`. An `error` is the route's own `ApiError`,
/// never a 200 with a plausible-looking body — so it has no name here.
export type HonestyState = "ok" | "partial" | "empty";

export interface DossierHonesty {
  state: HonestyState;
  reason?: string;
  budget: BudgetReport;
  /// Every caption this response owes its reader. Always present, possibly
  /// empty — never a silent absence.
  notes: string[];
}

export interface ZeitwerkOut {
  state: string;
  roots: string[];
  acronyms: string[];
  collapse: string[];
  reason?: string;
}

/// `GET /api/entity/dossier`'s body (`entities::dossier::DossierOut`).
export interface DossierOut {
  schema: string;
  repo: string;
  /// The address as it was asked.
  ent: string;
  entity: EntitySummary;
  /// Non-empty ONLY when the address is ambiguous.
  candidates: string[];
  definitions: DefinitionBlock[];
  members: MemberRow[];
  hierarchy: Hierarchy;
  usages: UsagesSection;
  unknown_members: UnknownMember[];
  namespace_tree: NamespaceChild[];
  zeitwerk: ZeitwerkOut;
  honesty: DossierHonesty;
}
