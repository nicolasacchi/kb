// Fetch wrapper for the kb-code HTTP API. Same-origin only — kb-code is a
// SINGLE-daemon SPA (no per-project daemon switching, unlike kb's own
// `web/src/api/base.ts` D7-prep multi-daemon primitive), so there's no
// `currentDaemonBase()` equivalent: every request is a plain relative
// `/api/...` fetch, proxied to 127.0.0.1:4747 in dev (`vite.config.ts`) and
// same-origin once `web-code/dist` is served by the daemon itself.

import type {
  AnchorKind,
  AnnotationIntent,
  AnnotationSuggestionOut,
  AnnotationsListResponse,
  AnnotationView,
  ApplySuggestionConflict,
  ApplySuggestionOut,
  BlameResponse,
  BlameTimelineResponse,
  BranchesResponse,
  BranchConflictsResponse,
  BranchFactsResponse,
  BranchFavouritesResponse,
  BranchReviewOut,
  BranchView,
  SetBranchFavouriteOut,
  CheckoutDirtyBody,
  CheckoutResponse,
  CommitPageResponse,
  ComparePageResponse,
  DefsOut,
  DiffResponse,
  FileHistoryResponse,
  FileResponse,
  DossierOut,
  IdentityOut,
  LineWhyOut,
  MergeCheckResponse,
  OpenAnnotationsResponse,
  PrCommentsResponse,
  PrFetchResponse,
  PrsResponse,
  RangeDiffResponse,
  ReposResponse,
  RefsOut,
  RefsResponse,
  RepoStateResponse,
  ResolveOut,
  HierarchyCalleesOut,
  HoverOut,
  HierarchyCallersOut,
  HierarchyTypesOut,
  ImpactAnalysisOut,
  LensesOut,
  SearchFilesResponse,
  SemanticSearchResponse,
  SessionDiff,
  StoryOut,
  SetsListOut,
  SetGroupsListOut,
  SetView,
  WorkspaceNotesResponse,
  Bookmark,
  BookmarksListOut,
  CreateReviewOut,
  ReviewAnnotationsOut,
  ReviewCommentsOut,
  ReviewDetail,
  ReviewFilesOut,
  ReviewInterdiffOut,
  ReviewVerdictState,
  ReviewsListOut,
  SetVerdictOut,
  ScopesOut,
  SnapshotReviewOut,
  CommentsListOut,
  CommentsFileOut,
  CommentsSummaryOut,
  CommentKeywordsOut,
  LanesOut,
  FactsOut,
  LanesSummaryOut,
  TodosListOut,
  TreeResponse,
  TreeV2Response,
  UnifiedSearchResponse,
  AgeOut,
  CouplingOut,
  HotspotsOut,
  OwnershipOut,
  ReviewRiskOut,
  RecipesCatalogOut,
  RecipeRunOut,
  KbcCatalogOut,
  KbcShowOut,
  KbcLintOut,
  KbcRunOut,
  KbcMaterialiseOut,
  KbcTrustOut,
  ReviewDocLintOut,
  ReviewDocOut,
  ReviewMapOut,
  ReviewReadingOrderOut,
  StacksOut,
  StacksLayerDiffOut,
  CanvasListOut,
  CanvasView,
  TimeseriesOut,
  SymbolsFileOut,
  SymbolsSearchOut,
  CodeLensOut,
  ScorecardOut,
  DocLensPinOut,
  DocLensResolvePathOut,
  DocRefsOut,
  FrameworkEdgesOut,
  ResolveSymbolOut,
  // ── PRR-U2 ──
  ReviewReportOut,
  ReviewFindingsOut,
  ReviewFinding,
  SetFindingDispositionInput,
  ReviewArtifactOut,
  PrDetailResponseOut,
  PrChecksOut,
  PrReviewsOut,
  // ── PRR-U9 ──
  DiagnosticsOut,
  // ── V74-L2 (kbc-canvas/1) ──
  BoardApplyOut,
  BoardOut,
  BoardStatusOut,
  BoardSweepOut,
  BoardsListOut,
  ToursListOut,
  TourOut,
  TourApplyOut,
  TrailStateOut,
  TrailsListOut,
  TrailOut,
  TrailPurgeOut,
  TrailCreatedOut,
} from "./types";

// V70-A2 (SEC-02) — every mutating request carries `X-Kbc-Request: 1`.
// It is a NON-SIMPLE header, which is the whole point: sending it
// cross-origin forces a CORS preflight, and the daemon answers preflights
// only on its operator-configured doc-lens routes. A drive-by page on
// another localhost port therefore cannot drive a mutation — it can neither
// omit the header (403 `urn:kb:errors:missing-request-header`) nor send it
// (no preflight answer). See `crates/kb-code-server/src/security/origin.rs`
// for the full admission table and for why no CSRF token accompanies it.
const KBC_REQUEST_HEADERS = { "X-Kbc-Request": "1" } as const;

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

/// `fetch`'s own `AbortError` is thrown whenever a request is cancelled via
/// its `signal` (a superseded debounced search, an unmounted hook, …) —
/// callers that race requests (`useOmniSearch`) treat this as "ignore,
/// don't surface an error", same convention as kb's own
/// `web/src/api/client.ts`'s `isAbortError`.
export function isAbortError(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}

async function getJson<T>(
  path: string,
  params: Record<string, string | undefined>,
  signal?: AbortSignal,
): Promise<T> {
  const qs = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined) qs.set(k, v);
  }
  const suffix = qs.toString();
  const url = suffix ? `${path}?${suffix}` : path;
  const res = await fetch(url, { headers: { Accept: "application/json" }, signal });
  if (!res.ok) {
    let message = res.statusText;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // Non-JSON error body (e.g. a proxy 502) — fall back to statusText.
    }
    throw new ApiError(res.status, message);
  }
  return (await res.json()) as T;
}

export function fetchRepos(): Promise<ReposResponse> {
  return getJson<ReposResponse>("/api/repos", {});
}

/// `GET /api/identity` — the one-shot boot fetch `hooks/useIdentity.ts`
/// drives; see `IdentityOut`'s doc for the field this call exists for.
export function fetchIdentity(): Promise<IdentityOut> {
  return getJson<IdentityOut>("/api/identity", {});
}

export function fetchTree(repo: string, path: string, ref?: string): Promise<TreeResponse> {
  return getJson<TreeResponse>("/api/tree", { repo, path, ref });
}

/// V71-F1 — `GET /api/tree/2` (kbc-tree/1): the PROJECTED tree. Every
/// param is optional but `repo`; absent params mean "the daemon's own
/// default", never a value invented here.
export function fetchTreeV2(params: {
  repo: string;
  view?: string;
  root?: string;
  depth?: number;
  expand?: string;
  scope?: string;
  filter?: string;
  mode?: string;
  decorate?: string;
  base?: string;
  review?: number;
  limit?: number;
}): Promise<TreeV2Response> {
  const q: Record<string, string | undefined> = { repo: params.repo };
  if (params.view) q.view = params.view;
  if (params.root) q.root = params.root;
  if (params.depth !== undefined) q.depth = String(params.depth);
  if (params.expand) q.expand = params.expand;
  if (params.scope) q.scope = params.scope;
  if (params.filter) q.filter = params.filter;
  if (params.mode) q.mode = params.mode;
  if (params.decorate) q.decorate = params.decorate;
  if (params.base) q.base = params.base;
  if (params.review !== undefined) q.review = String(params.review);
  if (params.limit !== undefined) q.limit = String(params.limit);
  return getJson<TreeV2Response>("/api/tree/2", q);
}

export function fetchFile(repo: string, path: string, ref?: string): Promise<FileResponse> {
  return getJson<FileResponse>("/api/file", { repo, path, ref });
}

export function fetchRefs(repo: string): Promise<RefsResponse> {
  return getJson<RefsResponse>("/api/refs", { repo });
}

export function fetchDiff(
  repo: string,
  path: string,
  from: string,
  to?: string,
): Promise<DiffResponse> {
  return getJson<DiffResponse>("/api/diff", { repo, path, from, to });
}

export interface SearchParams {
  q: string;
  repo?: string;
  limit?: number;
  signal?: AbortSignal;
}

/// `GET /api/search?q=&repo=&limit=` — the Search-Everywhere box (W4.3).
/// See `crates/kb-code-server/src/search/unified.rs`'s module doc for the
/// full response contract; `hooks/useOmniSearch.ts` is the sole caller.
export function fetchSearch({ q, repo, limit, signal }: SearchParams): Promise<UnifiedSearchResponse> {
  return getJson<UnifiedSearchResponse>(
    "/api/search",
    { q, repo, limit: limit !== undefined ? String(limit) : undefined },
    signal,
  );
}

/// `GET /api/search/files?repo=&q=&limit=` — the files lane's OWN standalone
/// endpoint (`routes::search_files`), distinct from the unified `/api/
/// search` this file's `fetchSearch` calls. `q: ""` returns
/// `search::files::FileIndex::recent`'s opened-at-ordered list (bumped by
/// every successful `GET /api/file`, see that module's doc) rather than a
/// fuzzy/frecency blend — Home's "Recent files" card section (F4,
/// `hooks/useRecentFiles.ts`) always calls it this way, scoped to one
/// card's own repo.
export function fetchSearchFiles(repo: string, q: string, limit?: number): Promise<SearchFilesResponse> {
  return getJson<SearchFilesResponse>("/api/search/files", {
    repo,
    q,
    limit: limit !== undefined ? String(limit) : undefined,
  });
}

/// `GET /api/search/semantic?q=&repo=&limit=` — the staged follow-up a
/// `pending: true` semantic section triggers (see `useOmniSearch`'s doc).
export function fetchSearchSemantic({
  q,
  repo,
  limit,
  signal,
}: SearchParams): Promise<SemanticSearchResponse> {
  return getJson<SemanticSearchResponse>(
    "/api/search/semantic",
    { q, repo, limit: limit !== undefined ? String(limit) : undefined },
    signal,
  );
}

// --- W4.4 — blame + provenance ---------------------------------------------

/// `GET /api/blame?repo=&path=[&ref=][&start=&end=]` — see
/// `crates/kb-code-server/src/blame/mod.rs`'s module doc for the clean/dirty/
/// caching contract. `start`/`end` (both required together) narrow the
/// response without changing what's cached.
export function fetchBlame(
  repo: string,
  path: string,
  ref?: string,
  start?: number,
  end?: number,
): Promise<BlameResponse> {
  return getJson<BlameResponse>("/api/blame", {
    repo,
    path,
    ref,
    start: start !== undefined ? String(start) : undefined,
    end: end !== undefined ? String(end) : undefined,
  });
}

/// `GET /api/blame/timeline?repo=&path=&line=&max=` — the bounded,
/// newest-first set of commits that have ever touched `line`.
export function fetchBlameTimeline(
  repo: string,
  path: string,
  line: number,
  max?: number,
): Promise<BlameTimelineResponse> {
  return getJson<BlameTimelineResponse>("/api/blame/timeline", {
    repo,
    path,
    line: String(line),
    max: max !== undefined ? String(max) : undefined,
  });
}

/// `GET /api/why?repo=&path=&line=` (line-grade only — the file-grade
/// no-`line` form has no SPA caller yet). See `provenance::why`'s module
/// doc for the uncommitted-live vs. join-ladder split.
export function fetchWhyLine(repo: string, path: string, line: number): Promise<LineWhyOut> {
  return getJson<LineWhyOut>("/api/why", { repo, path, line: String(line) });
}

/// `GET /api/story?repo=&path=` — the file's chronological session
/// timeline (whole-file form only — `?symbol=` has no SPA caller yet).
/// CT-E2 attention-gap beats arrive pre-collapsed server-side; the SPA
/// only renders them (`lib/storyBeats.ts`) — see `provenance::story`'s
/// module doc.
export function fetchStory(repo: string, path: string): Promise<StoryOut> {
  return getJson<StoryOut>("/api/story", { repo, path });
}

/// `GET /api/session-diff?session=&repo=` — LOOPBACK-ONLY (`router.rs`'s
/// transcripts sub-router); a non-loopback caller gets a 404 (never a
/// 401/403 that would confirm the route's existence), which every caller
/// here treats as "unavailable", never a hard failure (see
/// `hooks/useSessionDiff.ts`).
export function fetchSessionDiff(session: string, repo?: string): Promise<SessionDiff> {
  return getJson<SessionDiff>("/api/session-diff", { session, repo });
}

// --- B1 — defs + xrefs (the peek panel's tier-0 clickable code) ------------

/// `GET /api/defs?repo=&symbol=[&limit=]` — `repo` OMITTED searches every
/// configured repo (`agentview::xref::resolve_defs`'s "all repos by
/// default" scope, mirroring the search-everywhere lanes' own default —
/// see that module's doc). The peek panel's `gd`/`K` handlers call with
/// `repo` `undefined` on purpose, so cross-repo matches surface (a hit from
/// a different repo than the one currently open is still a valid landing).
export function fetchDefs(repo: string | undefined, symbol: string, limit?: number): Promise<DefsOut> {
  return getJson<DefsOut>("/api/defs", {
    repo,
    symbol,
    limit: limit !== undefined ? String(limit) : undefined,
  });
}

/// `GET /api/xrefs?repo=&symbol=[&limit=]` — `repo` is REQUIRED: text-grep
/// over ONE repo's working tree, no multi-repo fan-out (mirrors
/// `search::text`'s own scope limit — see `agentview::xref`'s module doc).
export function fetchXrefs(repo: string, symbol: string, limit?: number): Promise<RefsOut> {
  return getJson<RefsOut>("/api/xrefs", {
    repo,
    symbol,
    limit: limit !== undefined ? String(limit) : undefined,
  });
}

// --- B3 — position-based resolve (the peek's PRIMARY path) -----------------

export interface ResolveQuery {
  repo: string;
  path: string;
  /// 1-based.
  line: number;
  /// 0-based byte offset — tree-sitter's own convention, matching
  /// `WordPos.col` (`editor/vimReader.ts`) exactly, so `gd`/`K` pass a
  /// `WordPos` straight through with no translation.
  col: number;
  /// Omitted by every current caller (B1's own "no `?ref=` carried over"
  /// reasoning — see `Reader.tsx`'s B1 doc comment) — kept here only
  /// because the server route accepts it.
  ref?: string;
}

/// `GET /api/resolve?repo=&path=&line=&col=[&ref=]` (B3) — position-based
/// identifier lookup; `crate::resolve`'s module doc has the full file-local/
/// tags-approx/other-repo ranking + precision contract. A 404 (an older
/// daemon without this route) or any other failure is the caller's cue to
/// fall back to the name-based `fetchDefs` path — see `Reader.tsx`'s
/// `handleGotoDef`.
export function fetchResolve({ repo, path, line, col, ref }: ResolveQuery): Promise<ResolveOut> {
  return getJson<ResolveOut>("/api/resolve", {
    repo,
    path,
    line: String(line),
    col: String(col),
    ref,
  });
}

// --- hover/1 — the identifier tooltip's source (V70-A6) --------------------

/// `GET /api/hover?repo=&path=&line=&col=` — the composed, single-view
/// tooltip shape (`crates/kb-code-server/src/hover.rs`). Distinct from
/// `/api/resolve` on purpose: that one answers with a ranked candidate LIST
/// and its contract must not change; this one is already flattened for a
/// tooltip and additionally overlays a live LSP provider's signature/doc when
/// one is configured (PRR-L2), which is what lets a tooltip carry an
/// `lsp-live` trust badge.
///
/// The route has existed since PRR-N5 with no SPA consumer at all; V70-A6's
/// `editor/hoverTooltip.ts` is the first.
export function fetchHover({
  repo,
  path,
  line,
  col,
  ref,
}: ResolveQuery): Promise<HoverOut> {
  return getJson<HoverOut>("/api/hover", {
    repo,
    path,
    line: String(line),
    col: String(col),
    ref,
  });
}

// --- PRR-N5 — framework edges (T1 rails-lens reader surface) ---------------

/// `GET /api/framework/edges?repo=&path=[&kind=]` — every `rails_edges` row
/// where `path` is the `src_path` OR `dst_path` (direction distinguished
/// on each row), optionally narrowed to one edge `kind`. Degrades to an
/// empty `edges` list for a non-Rails repo or a path this lens has nothing
/// to say about — never an error (`framework_edges.rs`'s own doc).
export function fetchFrameworkEdges(repo: string, path: string, kind?: string): Promise<FrameworkEdgesOut> {
  return getJson<FrameworkEdgesOut>("/api/framework/edges", { repo, path, kind });
}

// --- PRR-N5 — resolve-symbol (T1 `?sym=` deep links) ------------------------

/// `GET /api/resolve-symbol?repo=&sym=` — see `symbol_addr.rs`'s module doc
/// for the full `<namespace>:<container>:<name>[:<kind>]` grammar (built
/// client-side by `lib/codeUrl.ts`'s `buildSym`). A miss is a `found: false`
/// VALUE, never a thrown `ApiError` — `Reader.tsx`'s on-load effect always
/// gets a normal resolved promise to branch on.
export function fetchResolveSymbol(repo: string, sym: string): Promise<ResolveSymbolOut> {
  return getJson<ResolveSymbolOut>("/api/resolve-symbol", { repo, sym });
}

// --- V3.1-H1 — call/type hierarchy (single-level; SPA expands lazily) ------

export interface HierarchyPosQuery {
  repo: string;
  path: string;
  line: number;
  col: number;
  ref?: string;
}

/// `GET /api/hierarchy/callees?repo=&path=&line=&col=`
export function fetchHierarchyCallees({
  repo,
  path,
  line,
  col,
  ref,
}: HierarchyPosQuery): Promise<HierarchyCalleesOut> {
  return getJson<HierarchyCalleesOut>("/api/hierarchy/callees", {
    repo,
    path,
    line: String(line),
    col: String(col),
    ref,
  });
}

/// `GET /api/hierarchy/callers?repo=&path=&line=&col=`
export function fetchHierarchyCallers({
  repo,
  path,
  line,
  col,
  ref,
}: HierarchyPosQuery): Promise<HierarchyCallersOut> {
  return getJson<HierarchyCallersOut>("/api/hierarchy/callers", {
    repo,
    path,
    line: String(line),
    col: String(col),
    ref,
  });
}

/// `GET /api/hierarchy/types?repo=&name=[&path=]`
export function fetchHierarchyTypes(
  repo: string,
  name: string,
  path?: string,
): Promise<HierarchyTypesOut> {
  return getJson<HierarchyTypesOut>("/api/hierarchy/types", {
    repo,
    name,
    path,
  });
}

// --- V3.1-H2 — impact analysis + Code Vision lenses (SPA H3b) --------------

export interface ImpactAnalysisQuery {
  repo: string;
  path: string;
  line: number;
  col: number;
  ref?: string;
  limit?: number;
}

/// `GET /api/impact/analysis?repo=&path=&line=&col=` — compositional impact
/// (`impact/1`). Distinct from the legacy co-change `GET /api/impact`.
export function fetchImpactAnalysis({
  repo,
  path,
  line,
  col,
  ref,
  limit,
}: ImpactAnalysisQuery): Promise<ImpactAnalysisOut> {
  return getJson<ImpactAnalysisOut>("/api/impact/analysis", {
    repo,
    path,
    line: String(line),
    col: String(col),
    ref,
    limit: limit != null ? String(limit) : undefined,
  });
}

/// `GET /api/lenses?repo=&path=` — per-declaration Code Vision chips.
export function fetchLenses(
  repo: string,
  path: string,
  ref?: string,
): Promise<LensesOut> {
  return getJson<LensesOut>("/api/lenses", { repo, path, ref });
}

// --- Phase C — time-first-class (commit/compare/branches/file-history) ----

/// `GET /api/commit?repo=&sha=` (Phase C1) — the commit page hub.
export function fetchCommit(repo: string, sha: string): Promise<CommitPageResponse> {
  return getJson<CommitPageResponse>("/api/commit", { repo, sha });
}

/// `GET /api/compare?repo=&from=&to=&three_dot=[&attribution=true]` (Phase
/// C2 + Phase G-server's `attribution` flag). `three_dot`/`attribution`
/// both follow `history::compare`'s/`routes::CompareParams`'s own
/// `#[serde(default)]` contract — omitted (not `"false"`) when falsy, so a
/// caller relies on the server's own default rather than this client
/// asserting one.
export function fetchCompare(
  repo: string,
  from: string,
  to: string,
  threeDot?: boolean,
  attribution?: boolean,
): Promise<ComparePageResponse> {
  return getJson<ComparePageResponse>("/api/compare", {
    repo,
    from,
    to,
    three_dot: threeDot ? "true" : undefined,
    attribution: attribution ? "true" : undefined,
  });
}

/// `GET /api/branches?repo=` (Phase C3) — every branch's ahead/behind +
/// tip attribution vs. the default branch. `sort=suggested` (V4.L1) adds
/// per-row `suggest` terms and ranks non-deterministically-looking but
/// closed-form; omit `sort` to keep the server's name default.
export function fetchBranches(
  repo: string,
  sort?: "name" | "suggested",
): Promise<BranchesResponse> {
  return getJson<BranchesResponse>("/api/branches", { repo, sort });
}

// --- V75-M3 (D15) — `branch-facts/1`, the radar, favourites ---------------

/// Every `GET /api/branches/facts` knob. A struct, not eight positional
/// arguments — `Branches.tsx` passes most of them from the URL and getting
/// two `string | undefined`s the wrong way round is silent.
export interface BranchFactsQuery {
  view?: BranchView;
  /// A kbcq/1 query — the `/` filter plus `branch:`/`touches:`/`by:`/
  /// `agent:`. Sent verbatim; the SERVER owns the grammar.
  q?: string;
  prefix?: string;
  fav?: boolean;
  limit?: number;
  offset?: number;
  pr?: boolean;
  ci?: boolean;
  patchId?: boolean;
}

/// `GET /api/branches/facts` (`branch-facts/1`).
export function fetchBranchFacts(
  repo: string,
  query: BranchFactsQuery = {},
): Promise<BranchFactsResponse> {
  return getJson<BranchFactsResponse>("/api/branches/facts", {
    repo,
    view: query.view,
    q: query.q || undefined,
    prefix: query.prefix || undefined,
    // The server reads `1`/`true`/`yes`; send the flag only when ON, so an
    // off toggle leaves the URL and the request byte-identical to never
    // having touched it.
    fav: query.fav ? "1" : undefined,
    limit: query.limit !== undefined ? String(query.limit) : undefined,
    offset: query.offset ? String(query.offset) : undefined,
    pr: query.pr ? "1" : undefined,
    ci: query.ci ? "1" : undefined,
    patch_id: query.patchId ? "1" : undefined,
  });
}

/// `GET /api/branches/conflicts` (`branch-conflicts/1`). `q` accepts only
/// the atoms computable from the one ref pass — the server REFUSES
/// `touches:` with a 400 rather than ignoring it.
export function fetchBranchConflicts(
  repo: string,
  against: string,
  limit?: number,
  q?: string,
): Promise<BranchConflictsResponse> {
  return getJson<BranchConflictsResponse>("/api/branches/conflicts", {
    repo,
    against,
    limit: limit !== undefined ? String(limit) : undefined,
    q: q || undefined,
  });
}

export function fetchBranchFavourites(repo: string): Promise<BranchFavouritesResponse> {
  return getJson<BranchFavouritesResponse>("/api/branches/favourites", { repo });
}

/// Star/unstar one FULL ref. Idempotent in both directions server-side, so
/// a double click is a no-op with `changed: false`, never a 409.
export function setBranchFavourite(
  repo: string,
  ref: string,
  on: boolean,
): Promise<SetBranchFavouriteOut> {
  return sendJson<SetBranchFavouriteOut>("POST", "/api/branches/favourites", { repo, ref, on });
}

/// `POST /api/branches/review` — "compare with common base". LOOPBACK-only
/// server-side, so this is offered behind `GET /api/repos`'s own `loopback`
/// bool exactly like every other mutation affordance in this SPA.
export function startBranchReview(
  repo: string,
  ref: string,
  base: string,
  title?: string,
): Promise<BranchReviewOut> {
  return sendJson<BranchReviewOut>("POST", "/api/branches/review", { repo, ref, base, title });
}

/// `GET /api/file-history?repo=&path=&limit=&before=` (Phase C4) — one
/// file's history, newest-first.
export function fetchFileHistory(
  repo: string,
  path: string,
  limit?: number,
  before?: number,
): Promise<FileHistoryResponse> {
  return getJson<FileHistoryResponse>("/api/file-history", {
    repo,
    path,
    limit: limit !== undefined ? String(limit) : undefined,
    before: before !== undefined ? String(before) : undefined,
  });
}

// --- W4.6 — annotations (Phase D adds anchor kinds/threads/intents) ---------

async function sendJson<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: {
      Accept: "application/json",
      ...KBC_REQUEST_HEADERS,
      ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let message = res.statusText;
    try {
      const errBody = (await res.json()) as { error?: string };
      if (errBody.error) message = errBody.error;
    } catch {
      // Non-JSON error body — fall back to statusText, same as `getJson`.
    }
    throw new ApiError(res.status, message);
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export function fetchAnnotations(repo: string, path: string): Promise<AnnotationsListResponse> {
  return getJson<AnnotationsListResponse>("/api/annotations", { repo, path });
}

/// V70-A10 — `GET /api/annotations?set_id=`: a workspace's notes (general
/// path-less notes AND code-anchored comments alike). A second, mutually
/// exclusive query shape on the SAME route as `fetchAnnotations` above
/// (`routes::list_annotations`'s doc).
export function fetchWorkspaceNotes(setId: string): Promise<WorkspaceNotesResponse> {
  return getJson<WorkspaceNotesResponse>("/api/annotations", { set_id: setId });
}

export interface CreateAnnotationInput {
  repo: string;
  path: string;
  /// 1-based. Required for every top-level kind (`line`/`range`/`symbol`/
  /// `diff` — the server builds the anchor from this line's own live
  /// content, `annotations::anchor_for_line`; the client never constructs
  /// or supplies an anchor itself). Omitted for a REPLY (`parent_id` set) —
  /// a reply has no anchor of its own.
  line?: number;
  body: string;
  author?: string;
  /// `"line"` (default, omit to get it) | `"range"` | `"symbol"` | `"diff"`.
  /// Ignored server-side for a reply.
  anchor_kind?: AnchorKind;
  /// `range` only — the inclusive end line (working-tree, current).
  line_end?: number;
  /// `diff` only — any revspec shape `join::ladder::is_plausible_sha`
  /// accepts; the server resolves it to the full sha.
  sha?: string;
  /// `"note"` (default, omit to get it) | `"question"` | `"todo"` |
  /// `"flag-for-agent"` | `"tour-stop"`.
  intent?: AnnotationIntent;
  /// `Some` makes this a REPLY to an existing (non-reply) annotation — one
  /// level of nesting only (`routes::create_annotation`'s doc).
  parent_id?: string;
  /// V4.C1 — optional review scope. Replies inherit from the parent (omit).
  review_id?: number;
  /// Patchset number; omitted → latest. Review-scoped creates only.
  ps?: number;
  /// `"old"` | `"new"` (default `"new"`). Review-scoped creates only.
  side?: "old" | "new";
  /// V70-A10 — optional workspace scope (`reading_sets.id`). Independent
  /// of `review_id` above. `anchor_kind: "set"` REQUIRES this; every other
  /// kind treats it as an optional extra tag. Replies inherit from the
  /// parent (omit).
  set_id?: string;
}

/// `POST /api/annotations` — mirrors the exact body shape
/// `routes::create_annotation`'s `CreateAnnotationBody` accepts (see that
/// route's doc): the server builds the `review::Anchor`, this call never
/// does. Callers almost never build this object by hand — `lib/
/// annotations.ts`'s `buildCreatePayload`/`buildReplyPayload` are the
/// validated entry points every composer in this crate goes through.
export function createAnnotation(input: CreateAnnotationInput): Promise<AnnotationView> {
  return sendJson<AnnotationView>("POST", "/api/annotations", input);
}

export interface PatchAnnotationInput {
  body?: string;
  resolved?: boolean;
  /// Validated against `annotations::is_valid_intent` server-side.
  intent?: AnnotationIntent;
}

/// `PATCH /api/annotations/{id}` — whichever of `body`/`resolved`/`intent`
/// is given is updated, the others left untouched (`routes::
/// patch_annotation`'s doc).
export function patchAnnotation(id: string, input: PatchAnnotationInput): Promise<AnnotationView> {
  return sendJson<AnnotationView>("PATCH", `/api/annotations/${encodeURIComponent(id)}`, input);
}

/// `DELETE /api/annotations/{id}` — hard delete, no tombstone. Deleting a
/// parent cascades to every reply nested under it.
export function deleteAnnotation(id: string): Promise<void> {
  return sendJson<void>("DELETE", `/api/annotations/${encodeURIComponent(id)}`);
}

/// `PUT /api/annotations/{id}/suggestion` — BEARER. Body `{replacement}`.
/// Server captures `original` + `base_blob_sha`; a re-PUT resets `applied`.
export function putAnnotationSuggestion(
  id: string,
  replacement: string,
): Promise<AnnotationSuggestionOut> {
  return sendJson<AnnotationSuggestionOut>(
    "PUT",
    `/api/annotations/${encodeURIComponent(id)}/suggestion`,
    { replacement },
  );
}

/// `DELETE /api/annotations/{id}/suggestion` — BEARER. 404 when no row.
export function deleteAnnotationSuggestion(id: string): Promise<void> {
  return sendJson<void>("DELETE", `/api/annotations/${encodeURIComponent(id)}/suggestion`);
}

/// Structured 409 from `POST /api/annotations/{id}/apply` — expected vs
/// found ranges, tree untouched. Distinct from a bare `ApiError` so the
/// thread can toast a compact hint rather than the raw body.
export class ApplyConflictError extends ApiError {
  expected: string;
  found: string;
  resolved_line: number;
  constructor(body: ApplySuggestionConflict) {
    super(409, body.error || "working-tree range no longer matches");
    this.name = "ApplyConflictError";
    this.expected = body.expected;
    this.found = body.found;
    this.resolved_line = body.resolved_line;
  }
}

function isApplyConflictBody(body: unknown): body is ApplySuggestionConflict {
  if (!body || typeof body !== "object") return false;
  const b = body as Record<string, unknown>;
  return typeof b.expected === "string" && typeof b.found === "string";
}

/// `POST /api/annotations/{id}/apply` — LOOPBACK-ONLY (bare 404 off
/// loopback, same contract as `createReview` / `putReviewVerdict`).
/// Body `{resolve?: bool}`. A 409 with `expected`/`found` throws
/// `ApplyConflictError`; every other failure is a plain `ApiError`.
export async function applyAnnotationSuggestion(
  id: string,
  resolve = false,
): Promise<ApplySuggestionOut> {
  const res = await fetch(`/api/annotations/${encodeURIComponent(id)}/apply`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      ...KBC_REQUEST_HEADERS,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ resolve }),
  });
  let body: unknown = null;
  try {
    body = await res.json();
  } catch {
    // Non-JSON (loopback 404 is a bare "Not Found") — fall through.
  }
  if (res.status === 409 && isApplyConflictBody(body)) {
    throw new ApplyConflictError({
      error: body.error || "working-tree range no longer matches",
      expected: body.expected,
      found: body.found,
      resolved_line: typeof body.resolved_line === "number" ? body.resolved_line : 0,
    });
  }
  if (!res.ok) {
    const message = (body as { error?: string } | null)?.error ?? res.statusText;
    throw new ApiError(res.status, message);
  }
  return body as ApplySuggestionOut;
}

/// `GET /api/annotations/open?repo=[&intent=][&path_prefix=]` (Phase D) —
/// every unresolved, top-level annotation across the WHOLE repo, newest
/// first. No SPA screen consumes this yet (the agent hook's + a future
/// dashboard's query surface — `routes::list_open_annotations`'s doc); kept
/// here so the next caller doesn't have to invent the fetch shape.
export function fetchOpenAnnotations(
  repo: string,
  opts?: { intent?: AnnotationIntent; path_prefix?: string },
): Promise<OpenAnnotationsResponse> {
  return getJson<OpenAnnotationsResponse>("/api/annotations/open", {
    repo,
    intent: opts?.intent,
    path_prefix: opts?.path_prefix,
  });
}

// --- Phase G-server — review workflow (merge-check/range-diff/repo-state/
// GitHub read overlay) ------------------------------------------------------

/// `GET /api/merge-check?repo=&from=&to=` (Phase G1) — dry-run merge
/// readiness (`crate::history::merge_check`'s module doc has the full
/// `git merge-tree --write-tree` contract).
export function fetchMergeCheck(repo: string, from: string, to: string): Promise<MergeCheckResponse> {
  return getJson<MergeCheckResponse>("/api/merge-check", { repo, from, to });
}

/// `GET /api/range-diff?repo=&old=&new=` (Phase C6) — `old`/`new` are RANGE
/// strings (e.g. `main..topic@{1}` vs `main..topic`), not plain revspecs —
/// see `crate::history::range_diff`'s module doc.
export function fetchRangeDiff(repo: string, old: string, newRange: string): Promise<RangeDiffResponse> {
  return getJson<RangeDiffResponse>("/api/range-diff", { repo, old, new: newRange });
}

/// `GET /api/repo-state?repo=` (Phase G2) — this repo's current git
/// operation state (`crate::repo_state`'s module doc).
export function fetchRepoState(repo: string): Promise<RepoStateResponse> {
  return getJson<RepoStateResponse>("/api/repo-state", { repo });
}

/// `GET /api/prs?repo=` (Phase G4) — every open PR on `repo`'s GitHub
/// origin. A non-GitHub-origin/no-origin repo is a hard 400 (`getJson`
/// throws an `ApiError`); every OTHER GitHub-side failure (rate-limited,
/// unreachable, a bad response) instead degrades to the response's own
/// `unavailable_reason` — see `github.rs`'s module doc.
export function fetchPrs(repo: string): Promise<PrsResponse> {
  return getJson<PrsResponse>("/api/prs", { repo });
}

/// `GET /api/prs/{number}/comments?repo=` (Phase G4) — one PR's review +
/// issue comments, already merged server-side (`github::GithubClient::
/// list_pull_comments`'s doc). Same 400-vs-degrade split as `fetchPrs`.
export function fetchPrComments(repo: string, number: number): Promise<PrCommentsResponse> {
  return getJson<PrCommentsResponse>(`/api/prs/${number}/comments`, { repo });
}

/// `POST /api/prs/fetch` (Phase G4) — body `{repo, number}`, runs `git
/// fetch origin +refs/pull/<n>/head:refs/kbc/pr/<n>`
/// (`github::fetch_pr_ref`'s doc). Mounted on the LOOPBACK-ONLY sub-router
/// (`router.rs`'s `transcripts_api`/`transcripts::search::loopback_only`,
/// the same gate `fetchSessionDiff` rides) — a non-loopback caller gets a
/// BARE `404` with NO JSON body (the middleware short-circuits before this
/// route's own handler runs), structurally distinct from an ordinary "no
/// such repo" 404 (`routes::find_repo`), which always carries a
/// `{"error": ...}` body. `getJson`/`sendJson`'s shared error path falls
/// back to `res.statusText` ("Not Found") when the body isn't JSON — so
/// `ApiError.message === "Not Found"` at `status === 404` is this route's
/// specific "unreachable from here, try from the machine running
/// kb-code-server" signal; callers (`routes/Prs.tsx`) special-case it into
/// an explanatory toast rather than a generic error.
export function postPrFetch(repo: string, number: number): Promise<PrFetchResponse> {
  return sendJson<PrFetchResponse>("POST", "/api/prs/fetch", { repo, number });
}

// --- W4.7 — confirmed checkout -----------------------------------------------

export type CheckoutResult =
  | { kind: "ok"; data: CheckoutResponse }
  | { kind: "dirty"; data: CheckoutDirtyBody }
  | { kind: "error"; error: string };

/// `POST /api/checkout` — deliberately NOT routed through `getJson`/
/// `sendJson`: a dirty working tree is a STRUCTURED 409 (`dirty_paths`), not
/// a bare `{"error": ...}` `ApiError` (`routes::checkout_route`'s doc), so
/// this reads the body itself and returns a discriminated result instead of
/// throwing — the caller (`CheckoutDialog`) needs the path list to render
/// the honest refusal, not just a rejected promise.
export async function postCheckout(repo: string, target: string): Promise<CheckoutResult> {
  let res: Response;
  try {
    res = await fetch("/api/checkout", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json",
        ...KBC_REQUEST_HEADERS,
      },
      body: JSON.stringify({ repo, ref: target }),
    });
  } catch (e) {
    return { kind: "error", error: e instanceof Error ? e.message : String(e) };
  }
  let body: unknown = null;
  try {
    body = await res.json();
  } catch {
    // No/invalid JSON body — fall through to the statusText-based error below.
  }
  if (res.ok) return { kind: "ok", data: body as CheckoutResponse };
  if (res.status === 409) return { kind: "dirty", data: body as CheckoutDirtyBody };
  const message = (body as { error?: string } | null)?.error ?? res.statusText;
  return { kind: "error", error: message };
}

// --- Phase E4 — reading sets -------------------------------------------

/// One span as the CLIENT sends it — mirrors `reading_sets::SpanInput`
/// field-for-field (the wire key for `ref` is literally `ref`, same
/// `#[serde(rename = "ref")]` convention `SetSpanOut.ref` documents).
export interface SetSpanInput {
  path: string;
  line_start?: number;
  line_end?: number;
  ref?: string;
  note?: string;
}

/// `POST /api/sets`'s body (`reading_sets::CreateSetBody`). V70-A10 adds
/// `kind`/`desk_json`/`ref`/`description_md` — see [`createSet`]'s doc.
export interface CreateSetInput {
  repo: string;
  name: string;
  description?: string;
  spans?: SetSpanInput[];
  /// `"set"` (default) | `"workspace"`.
  kind?: string;
  /// Opaque `DeskState` JSON, stored verbatim — the server never
  /// interprets it (`reading_sets::validate_desk_json`'s doc).
  desk_json?: string;
  /// Wire key is literally `ref` (same convention `SetSpanInput.ref`
  /// already uses).
  ref?: string;
  description_md?: string;
}

/// `PATCH /api/sets/{id}`'s body (`reading_sets::PatchSetBody`) — `spans`,
/// when given, is a FULL replacement (see `patchSet`'s own doc below), never
/// a partial edit. V70-A10 adds the same four workspace fields as
/// [`CreateSetInput`].
export interface PatchSetInput {
  name?: string;
  description?: string;
  spans?: SetSpanInput[];
  kind?: string;
  desk_json?: string;
  ref?: string;
  description_md?: string;
}

/// `POST /api/sets/from-session`'s body (`reading_sets::FromSessionBody`).
export interface FromSessionInput {
  repo: string;
  session_id: string;
  name?: string;
}

/// `GET /api/sets?repo=[&kind=][&group=]` — every reading set (or, with
/// `kind: "workspace"`, every workspace) in `repo`, alphabetical by name.
/// `group: "ref"` returns [`SetGroupsListOut`] instead — callers that pass
/// it should type their own `await` as that shape (this fn's return type
/// stays `SetsListOut` for the far more common flat-list callers; see
/// [`fetchWorkspaceGroups`] for the grouped shape).
export function fetchSets(repo: string, kind?: string): Promise<SetsListOut> {
  return getJson<SetsListOut>("/api/sets", { repo, kind });
}

/// V70-A10 — `GET /api/sets?repo=&kind=workspace&group=ref`: workspaces
/// grouped by their own `ref` label.
export function fetchWorkspaceGroups(repo: string): Promise<SetGroupsListOut> {
  return getJson<SetGroupsListOut>("/api/sets", { repo, kind: "workspace", group: "ref" });
}

/// `GET /api/sets/{id}` — one set's full ordered spans. `404` unknown id.
export function fetchSet(id: string): Promise<SetView> {
  return getJson<SetView>(`/api/sets/${encodeURIComponent(id)}`, {});
}

/// `POST /api/sets` — `409` on a `(repo, name)` collision
/// (`reading_sets::create_set`'s doc). V70-A10: `400` on an invalid `kind`,
/// an oversized/malformed `desk_json`, an oversized `description_md`, or a
/// shape-invalid `ref`.
export function createSet(input: CreateSetInput): Promise<SetView> {
  return sendJson<SetView>("POST", "/api/sets", input);
}

/// `PATCH /api/sets/{id}` — `spans`, when given, REPLACES the whole span
/// list in one transaction (`reading_sets::patch_set`'s doc) — callers that
/// want to reorder/remove one row must resend the full array, not a delta.
export function patchSet(id: string, input: PatchSetInput): Promise<SetView> {
  return sendJson<SetView>("PATCH", `/api/sets/${encodeURIComponent(id)}`, input);
}

/// `POST /api/sets/{id}/spans` — append ONE span after the set's current
/// last ordinal. Returns the full updated set.
export function appendSetSpan(id: string, input: SetSpanInput): Promise<SetView> {
  return sendJson<SetView>("POST", `/api/sets/${encodeURIComponent(id)}/spans`, input);
}

/// `DELETE /api/sets/{id}` — cascades to spans in one transaction.
export function deleteSet(id: string): Promise<void> {
  return sendJson<void>("DELETE", `/api/sets/${encodeURIComponent(id)}`);
}

/// `POST /api/sets/from-session` — LOOPBACK-ONLY (mounted on the same
/// sub-router as `session-diff`/`checkout`, see `reading_sets::
/// from_session_route`'s doc) — a non-loopback caller's `ApiError.status ===
/// 403`, the same contract `fetchSessionDiff` callers already treat as
/// "unavailable" rather than a hard failure. `404` an unknown session; `409`
/// the resulting name already exists in the repo.
export function fromSessionSet(input: FromSessionInput): Promise<SetView> {
  return sendJson<SetView>("POST", "/api/sets/from-session", input);
}

/// `POST /api/sets/from-doc`'s body (`reading_sets::FromDocBody`, DCB
/// W3.C/R24). `doc` is a kb artifact id (R2), never a source-relative path.
export interface FromDocInput {
  repo: string;
  kb: string;
  doc: string;
  name?: string;
}

/// `POST /api/sets/from-doc` — LOOPBACK-ONLY (mounted on the same
/// sub-router as `from-session`/`checkout`, see `reading_sets::
/// from_doc_route`'s doc) — same non-loopback-caller contract as
/// `fromSessionSet` above. `404` unknown repo, or an unresolved `(kb, doc)`
/// pair (kb daemon disabled/unreachable, the doc 404s on kb's side, …);
/// `409` the resulting name already exists in the repo.
export function fromDocSet(input: FromDocInput): Promise<SetView> {
  return sendJson<SetView>("POST", "/api/sets/from-doc", input);
}

// --- Phase N ("Navigate") — bookmarks + todos + scopes --------------------

/// `GET /api/bookmarks?repo=` — every bookmark in `repo`, mnemonic-first
/// then `created_at` (server order).
export function fetchBookmarks(repo: string): Promise<BookmarksListOut> {
  return getJson<BookmarksListOut>("/api/bookmarks", { repo });
}

export interface CreateBookmarkInput {
  repo: string;
  path: string;
  line: number;
  mnemonic?: string;
  note?: string;
}

/// `POST /api/bookmarks` — create. A taken mnemonic MOVES to the new row.
export function createBookmark(input: CreateBookmarkInput): Promise<Bookmark> {
  return sendJson<Bookmark>("POST", "/api/bookmarks", input);
}

/// `PATCH /api/bookmarks/{id}` — `mnemonic: null` / `note: null` clear those
/// fields (double-option wire shape; absent keys leave the field alone).
export interface PatchBookmarkInput {
  line?: number;
  note?: string | null;
  mnemonic?: string | null;
}

export function patchBookmark(id: number, input: PatchBookmarkInput): Promise<Bookmark> {
  return sendJson<Bookmark>("PATCH", `/api/bookmarks/${id}`, input);
}

/// `DELETE /api/bookmarks/{id}`.
export function deleteBookmark(id: number): Promise<void> {
  return sendJson<void>("DELETE", `/api/bookmarks/${id}`);
}

export interface FetchTodosParams {
  repo: string;
  marker?: string;
  path_prefix?: string;
  /// Scope name to include, or `!<name>` to exclude.
  scope?: string;
  limit?: number;
}

/// `GET /api/todos?repo=[&marker=][&path_prefix=][&scope=][&limit=]`.
export function fetchTodos(params: FetchTodosParams): Promise<TodosListOut> {
  return getJson<TodosListOut>("/api/todos", {
    repo: params.repo,
    marker: params.marker,
    path_prefix: params.path_prefix,
    scope: params.scope,
    limit: params.limit !== undefined ? String(params.limit) : undefined,
  });
}

/// `GET /api/scopes` — configured `[scopes]` map (name → globs).
export function fetchScopes(): Promise<ScopesOut> {
  return getJson<ScopesOut>("/api/scopes", {});
}

// --- comments/1 (V72-J1 server, V72-J2 SPA client) --------------------------

export interface FetchCommentsParams {
  repo: string;
  path?: string;
  kind?: string;
  keyword?: string;
  state?: string;
  limit?: number;
  offset?: number;
}

/// `GET /api/comments?repo=[&path=][&kind=][&keyword=][&state=][&limit=]
/// [&offset=]` — the paged index; the `~comments` dashboard's feed. Server
/// paging always — this call never fetches "everything" and slices client-
/// side (kb-code's own `?limit=`/`?offset=` convention, same as `fetchTodos`
/// above).
export function fetchComments(params: FetchCommentsParams): Promise<CommentsListOut> {
  return getJson<CommentsListOut>("/api/comments", {
    repo: params.repo,
    path: params.path,
    kind: params.kind,
    keyword: params.keyword,
    state: params.state,
    limit: params.limit !== undefined ? String(params.limit) : undefined,
    offset: params.offset !== undefined ? String(params.offset) : undefined,
  });
}

/// `GET /api/comments/file?repo=&path=` — every comment block in one file,
/// in line order. The per-file comment gutter's feed.
export function fetchCommentsFile(repo: string, path: string): Promise<CommentsFileOut> {
  return getJson<CommentsFileOut>("/api/comments/file", { repo, path });
}

/// `GET /api/comments/summary?repo=` — exact per-kind/per-keyword counts
/// plus the two blame-free state lanes; the dashboard's bounds captions.
export function fetchCommentsSummary(repo: string): Promise<CommentsSummaryOut> {
  return getJson<CommentsSummaryOut>("/api/comments/summary", { repo });
}

/// `GET /api/comments/keywords` — the effective annotation vocabulary
/// (daemon-wide, `[comments] keywords` is not per-repo, so no `repo` param).
export function fetchCommentKeywords(): Promise<CommentKeywordsOut> {
  return getJson<CommentKeywordsOut>("/api/comments/keywords", {});
}

// --- V3.R1 / V3.R2 — local review sessions --------------------------------

/// `GET /api/reviews?repo=[&state=]` — bearer read.
export function fetchReviews(repo: string, state?: "open" | "closed"): Promise<ReviewsListOut> {
  return getJson<ReviewsListOut>("/api/reviews", { repo, state });
}

/// `GET /api/reviews/{id}` — review + patchsets.
export function fetchReview(id: number): Promise<ReviewDetail> {
  return getJson<ReviewDetail>(`/api/reviews/${id}`, {});
}

/// `GET /api/reviews/{id}/files?ps=latest|N`.
export function fetchReviewFiles(id: number, ps?: string): Promise<ReviewFilesOut> {
  return getJson<ReviewFilesOut>(`/api/reviews/${id}/files`, { ps });
}

/// `GET /api/reviews/{id}/interdiff?from=&to=`.
export function fetchReviewInterdiff(id: number, from: number, to: number): Promise<ReviewInterdiffOut> {
  return getJson<ReviewInterdiffOut>(`/api/reviews/${id}/interdiff`, {
    from: String(from),
    to: String(to),
  });
}

/// `GET /api/reviews/{id}/annotations` — open annotations grouped by path.
export function fetchReviewAnnotations(id: number): Promise<ReviewAnnotationsOut> {
  return getJson<ReviewAnnotationsOut>(`/api/reviews/${id}/annotations`, {});
}

/// `GET /api/reviews/{id}/comments?ps=latest|N&all=true` — threaded
/// review-scoped comments with per-request resolution (V4.C1/C4).
export function fetchReviewComments(
  id: number,
  ps?: string,
  all?: boolean,
): Promise<ReviewCommentsOut> {
  return getJson<ReviewCommentsOut>(`/api/reviews/${id}/comments`, {
    ps,
    all: all ? "true" : undefined,
  });
}

/// `PUT /api/reviews/{id}/verdict` — LOOPBACK-ONLY (bare 404 off-loopback).
export function putReviewVerdict(
  id: number,
  input: { state: ReviewVerdictState; note?: string },
): Promise<SetVerdictOut> {
  return sendJson<SetVerdictOut>("PUT", `/api/reviews/${id}/verdict`, input);
}

/// `DELETE /api/reviews/{id}/verdict` — LOOPBACK-ONLY.
export function deleteReviewVerdict(id: number): Promise<SetVerdictOut> {
  return sendJson<SetVerdictOut>("DELETE", `/api/reviews/${id}/verdict`);
}

export interface CreateReviewInput {
  repo: string;
  head_ref: string;
  base_ref?: string;
  title?: string;
  session_id?: string;
}

/// `POST /api/reviews` — LOOPBACK-ONLY (same bare-404 contract as
/// `postPrFetch` / `postCheckout`: a non-loopback caller gets status 404
/// with `message === "Not Found"`, not a structured JSON error).
export function createReview(input: CreateReviewInput): Promise<CreateReviewOut> {
  return sendJson<CreateReviewOut>("POST", "/api/reviews", input);
}

/// `POST /api/reviews/{id}/snapshot` — LOOPBACK-ONLY.
export function snapshotReview(id: number): Promise<SnapshotReviewOut> {
  return sendJson<SnapshotReviewOut>("POST", `/api/reviews/${id}/snapshot`);
}

export interface PatchReviewInput {
  title?: string;
  state?: "open" | "closed";
}

/// `PATCH /api/reviews/{id}` — LOOPBACK-ONLY.
export function patchReview(id: number, input: PatchReviewInput): Promise<ReviewDetail> {
  return sendJson<ReviewDetail>("PATCH", `/api/reviews/${id}`, input);
}

/// `DELETE /api/reviews/{id}` — LOOPBACK-ONLY.
export function deleteReview(id: number): Promise<void> {
  return sendJson<void>("DELETE", `/api/reviews/${id}`);
}

/// `PUT /api/reviews/{id}/viewed` — LOOPBACK-ONLY.
export function putReviewViewed(id: number, path: string, blob_sha: string): Promise<unknown> {
  return sendJson("PUT", `/api/reviews/${id}/viewed`, { path, blob_sha });
}

/// `DELETE /api/reviews/{id}/viewed/{path}` — path is a single URL-encoded
/// segment. LOOPBACK-ONLY.
export function deleteReviewViewed(id: number, path: string): Promise<void> {
  return sendJson<void>("DELETE", `/api/reviews/${id}/viewed/${encodeURIComponent(path)}`);
}

/// V73-K2a — `PUT /api/reviews/{id}/hunk-viewed`. LOOPBACK-ONLY, the same
/// unconditional gate its per-file twin above rides (`review_gate`'s own
/// doc names `viewed` among the mutations `[review] remote_mutations`
/// never reaches). `hunk_id` is `lib/diffHunks.ts`'s content address.
export function putReviewHunkViewed(
  id: number,
  hunk_id: string,
  path: string,
): Promise<unknown> {
  return sendJson("PUT", `/api/reviews/${id}/hunk-viewed`, { hunk_id, path });
}

/// `DELETE /api/reviews/{id}/hunk-viewed/{hunk_id}` — LOOPBACK-ONLY. 404s
/// an id that was never marked (the server's own "nothing to unmark is a
/// miss" rule), which callers surface as a toast rather than swallow.
export function deleteReviewHunkViewed(id: number, hunk_id: string): Promise<void> {
  return sendJson<void>("DELETE", `/api/reviews/${id}/hunk-viewed/${encodeURIComponent(hunk_id)}`);
}

// --- V3.2-B1 / B2 — behavioral attention signals --------------------------

export interface FetchHotspotsParams {
  repo: string;
  limit?: number;
  /** Scope name to include, or `!<name>` to exclude. */
  scope?: string;
  /** B2: `pain` weights by session pain; omit for default. */
  weight?: string;
}

/// `GET /api/behavioral/hotspots?repo=[&limit=][&scope=][&weight=]`.
export function fetchHotspots(params: FetchHotspotsParams): Promise<HotspotsOut> {
  return getJson<HotspotsOut>("/api/behavioral/hotspots", {
    repo: params.repo,
    limit: params.limit !== undefined ? String(params.limit) : undefined,
    scope: params.scope,
    weight: params.weight,
  });
}

/// `GET /api/behavioral/coupling?repo=&path=[&limit=]`.
export function fetchCoupling(
  repo: string,
  path: string,
  limit?: number,
): Promise<CouplingOut> {
  return getJson<CouplingOut>("/api/behavioral/coupling", {
    repo,
    path,
    limit: limit !== undefined ? String(limit) : undefined,
  });
}

/// `GET /api/behavioral/ownership?repo=&path=`.
export function fetchOwnership(repo: string, path: string): Promise<OwnershipOut> {
  return getJson<OwnershipOut>("/api/behavioral/ownership", { repo, path });
}

/// `GET /api/behavioral/age?repo=&path=`.
export function fetchAge(repo: string, path: string): Promise<AgeOut> {
  return getJson<AgeOut>("/api/behavioral/age", { repo, path });
}

export interface FetchTimeseriesParams {
  repo: string;
  /** Exact final path (post -M rename); omit for whole-repo buckets. */
  path?: string;
  /** Lookback weeks (server default 26, hard cap 104). */
  weeks?: number;
}

/// `GET /api/behavioral/timeseries?repo=&path=&weeks=` — per-week activity
/// (commits/churn/authors). Derived at request time; attention only.
export function fetchTimeseries(params: FetchTimeseriesParams): Promise<TimeseriesOut> {
  return getJson<TimeseriesOut>("/api/behavioral/timeseries", {
    repo: params.repo,
    path: params.path,
    weeks: params.weeks !== undefined ? String(params.weeks) : undefined,
  });
}

/// `GET /api/symbols?repo=&path=` — per-file symbol list (store lookup).
export function fetchSymbolsFile(
  repo: string,
  path: string,
  ref?: string,
): Promise<SymbolsFileOut> {
  return getJson<SymbolsFileOut>("/api/symbols", { repo, path, ref });
}

/// `GET /api/symbols?repo=&q=` — repo-wide case-insensitive substring (cap 200).
/// Empty `q` matches every indexed symbol name (server `contains("")`).
export function fetchSymbolsSearch(repo: string, q: string): Promise<SymbolsSearchOut> {
  return getJson<SymbolsSearchOut>("/api/symbols", { repo, q });
}

/// `GET /api/reviews/{id}/risk` — B2 review-risk composite. 404 when the
/// surface is not present on the daemon (SPA degrades silently).
export function fetchReviewRisk(id: number): Promise<ReviewRiskOut> {
  return getJson<ReviewRiskOut>(`/api/reviews/${id}/risk`, {});
}

// --- V3.3-Q1 — recipes ----------------------------------------------------

/// `GET /api/recipes` — pure catalog (no repo).
export function fetchRecipesCatalog(): Promise<RecipesCatalogOut> {
  return getJson<RecipesCatalogOut>("/api/recipes", {});
}

export interface FetchRecipeRunParams {
  name: string;
  repo: string;
  since?: string;
  limit?: number;
  scope?: string;
}

/// `GET /api/recipes/{name}?repo=&since=&limit=`.
export function fetchRecipeRun(params: FetchRecipeRunParams): Promise<RecipeRunOut> {
  return getJson<RecipeRunOut>(`/api/recipes/${encodeURIComponent(params.name)}`, {
    repo: params.repo,
    since: params.since,
    limit: params.limit !== undefined ? String(params.limit) : undefined,
    scope: params.scope,
  });
}

// --- V74-L3a/c — `kbc-recipe/1`, the typed recipe runner --------------------

/// `GET /api/recipe?repo=` — every built-in, server-stored and
/// repo-versioned recipe for this repo, home + trust state + CLI line.
export function fetchRecipeCatalog(repo: string): Promise<KbcCatalogOut> {
  return getJson<KbcCatalogOut>("/api/recipe", { repo });
}

/// `GET /api/recipe/{slug}?repo=` — one recipe's full document (params,
/// steps, views, source, and — for a changed repo file — the trust diff).
export function fetchRecipeShow(slug: string, repo: string): Promise<KbcShowOut> {
  return getJson<KbcShowOut>(`/api/recipe/${encodeURIComponent(slug)}`, { repo });
}

/// `GET /api/recipe/{slug}/lint?repo=` — would it load, would it run; a
/// LIST of problems, never a first error.
export function fetchRecipeLint(slug: string, repo: string): Promise<KbcLintOut> {
  return getJson<KbcLintOut>(`/api/recipe/${encodeURIComponent(slug)}/lint`, { repo });
}

/// The dynamic `p.<name>=`/`ctx.<field>=` keys ride straight through — same
/// grammar the browser URL already carries (`lib/recipeUrl.ts`), so a
/// caller can pass `parseRecipeSearch(location.search)`'s `params`/`ctx`
/// maps verbatim.
export interface RecipeRunQuery {
  slug: string;
  repo: string;
  scope?: string;
  limit?: number;
  params?: Record<string, string>;
  ctx?: Record<string, string>;
}

function recipeRunQueryParams(q: RecipeRunQuery): Record<string, string | undefined> {
  const params: Record<string, string | undefined> = { repo: q.repo };
  if (q.scope) params.scope = q.scope;
  if (q.limit !== undefined) params.limit = String(q.limit);
  for (const [k, v] of Object.entries(q.params ?? {})) {
    if (v !== "") params[`p.${k}`] = v;
  }
  for (const [k, v] of Object.entries(q.ctx ?? {})) {
    if (v !== "") params[`ctx.${k}`] = v;
  }
  return params;
}

/// `GET /api/recipe/{slug}/run?repo=&p.<name>=&ctx.<field>=&scope=&limit=`
/// — mutates nothing. Every view is fetched at once (no `view=` sent) so
/// the client-side view switcher (`Recipes.tsx`) never re-fetches on a
/// switch — see that file's doc.
export function fetchRecipeRunV2(q: RecipeRunQuery): Promise<KbcRunOut> {
  return getJson<KbcRunOut>(`/api/recipe/${encodeURIComponent(q.slug)}/run`, recipeRunQueryParams(q));
}

/// `GET /api/recipe/runs/{id}` — replay a materialised run. A stale
/// snapshot (the corpus has since re-indexed) says so via `replay.stale`
/// rather than silently reading as live.
export function fetchRecipeReplay(id: string): Promise<KbcRunOut> {
  return getJson<KbcRunOut>(`/api/recipe/runs/${encodeURIComponent(id)}`, {});
}

/// `POST /api/recipe/{slug}/materialise?…` — LOOPBACK ONLY. Same query
/// shape as `fetchRecipeRunV2`; the daemon re-runs and stores a replayable
/// snapshot rather than returning one, so the caller navigates to
/// `recipeReplayUrl(repo, run_id)` on success.
export async function postRecipeMaterialise(q: RecipeRunQuery): Promise<KbcMaterialiseOut> {
  const qs = new URLSearchParams();
  for (const [k, v] of Object.entries(recipeRunQueryParams(q))) {
    if (v !== undefined) qs.set(k, v);
  }
  return sendJson<KbcMaterialiseOut>(
    "POST",
    `/api/recipe/${encodeURIComponent(q.slug)}/materialise?${qs.toString()}`,
  );
}

/// `POST /api/recipe/{slug}/trust` — LOOPBACK ONLY. Accepts the repo file's
/// bytes RIGHT NOW; a later change re-arms the prompt with a fresh diff.
export function postRecipeTrust(slug: string, repo: string): Promise<KbcTrustOut> {
  return sendJson<KbcTrustOut>("POST", `/api/recipe/${encodeURIComponent(slug)}/trust`, { repo });
}

// --- V3.3-S1 — review map + reading order ---------------------------------

/// `GET /api/reviews/{id}/map` — 404 when the surface is absent (older server).
export function fetchReviewMap(id: number): Promise<ReviewMapOut> {
  return getJson<ReviewMapOut>(`/api/reviews/${id}/map`, {});
}

// --- V73-K1/K2b — the kbc-review/1 document -------------------------------

/// `GET /api/reviews/{id}/doc[?ps=&resolve=true]` (`kbc-review/1`) — 404 when
/// the review has no composed document (or on an older server), which is what
/// lets the cockpit hide the Document tab rather than show empty chrome.
///
/// `resolve` is OFF by default on the wire: card resolution reads git blobs
/// and the symbol index, and a caller that only wants the prose should not
/// pay for it. The SPA always asks for `true` — the whole point of the tab is
/// the live cards — but the param is spelled here rather than hardcoded so
/// the cost is visible at the call site.
export function fetchReviewDoc(
  id: number,
  params: { ps?: string; resolve?: boolean } = {},
): Promise<ReviewDocOut> {
  return getJson<ReviewDocOut>(`/api/reviews/${id}/doc`, {
    ps: params.ps,
    resolve: params.resolve ? "true" : undefined,
  });
}

/// `GET /api/reviews/{id}/doc/lint[?ps=]` — lints the STORED document.
/// Read-only: composing stays loopback-only (D22), so the SPA shows the
/// `kb-code review compose` line rather than offering to run it.
export function fetchReviewDocLint(
  id: number,
  params: { ps?: string } = {},
): Promise<ReviewDocLintOut> {
  return getJson<ReviewDocLintOut>(`/api/reviews/${id}/doc/lint`, { ps: params.ps });
}

/// `GET /api/reviews/{id}/reading-order`.
export function fetchReviewReadingOrder(id: number): Promise<ReviewReadingOrderOut> {
  return getJson<ReviewReadingOrderOut>(`/api/reviews/${id}/reading-order`, {});
}

// --- V3.3-S2 — stacks -----------------------------------------------------

export interface FetchStacksParams {
  repo: string;
  /** When true, include single-layer stacks. Default API false. */
  all?: boolean;
}

/// `GET /api/stacks?repo=&all=`.
export function fetchStacks(params: FetchStacksParams): Promise<StacksOut> {
  return getJson<StacksOut>("/api/stacks", {
    repo: params.repo,
    // CLI uses `all=true`; routes doc also accepts all=1. serde bool → true.
    all: params.all ? "true" : undefined,
  });
}

/// `GET /api/stacks/layer-diff?repo=&branch=`.
export function fetchStacksLayerDiff(repo: string, branch: string): Promise<StacksLayerDiffOut> {
  return getJson<StacksLayerDiffOut>("/api/stacks/layer-diff", { repo, branch });
}

// --- V3.4-C1 — canvas sets (reads ordinary auth; mutations LOOPBACK-ONLY) --

/// `GET /api/canvas?repo=` — list (no payload body).
export function fetchCanvasList(repo: string): Promise<CanvasListOut> {
  return getJson<CanvasListOut>("/api/canvas", { repo });
}

/// `GET /api/canvas/{id}` — full row incl. opaque payload.
export function fetchCanvas(id: number): Promise<CanvasView> {
  return getJson<CanvasView>(`/api/canvas/${id}`, {});
}

export interface CreateCanvasInput {
  repo: string;
  name: string;
  review_id?: number | null;
  payload: unknown;
}

/// `POST /api/canvas` — LOOPBACK-ONLY; 409 name collision; 413 over 256 KiB.
export function createCanvas(input: CreateCanvasInput): Promise<CanvasView> {
  return sendJson<CanvasView>("POST", "/api/canvas", input);
}

export interface UpdateCanvasInput {
  payload: unknown;
  name?: string;
}

/// `PUT /api/canvas/{id}` — LOOPBACK-ONLY; replace payload (optional rename).
export function updateCanvas(id: number, input: UpdateCanvasInput): Promise<CanvasView> {
  return sendJson<CanvasView>("PUT", `/api/canvas/${id}`, input);
}

/// `DELETE /api/canvas/{id}` — LOOPBACK-ONLY.
export function deleteCanvas(id: number): Promise<void> {
  return sendJson<void>("DELETE", `/api/canvas/${id}`);
}

// --- DCB W2.B — the doc↔code lens ------------------------------------------
//
// `GET /api/doc-lens`/`GET /api/doc-lens/repos` ride the SAME plain
// `auth_bearer` gate as every other browsing-class read here — the CORS
// layer `crates/kb-code-server/src/router.rs`'s `doclens_read` sub-router
// adds is there for kb's OWN (cross-origin) reader, not for web-code, which
// is this daemon's own same-origin frontend (see `hooks/useDocLens.ts`'s
// header doc). `PUT /api/doc-lens/pin` is likewise a plain same-origin
// mutation for the same reason (§3 of the W2.B spec).

/// `GET /api/doc-lens?kb=&doc=&repo=&at=declared` — full per-ref resolution
/// against ONE repo. CT-F2's `at=declared` is always passed: it is a pure
/// opt-in ADDITION on the wire (a doc with no usable `kb-code-rev` degrades
/// to `era: "none"` + every `when_written` null, byte-identical to the
/// pre-CT-F2 shape), so there is no reason for this, the daemon's own
/// same-origin frontend, not to ask for it on every load.
export function fetchDocLens(kb: string, doc: string, repo: string): Promise<CodeLensOut> {
  return getJson<CodeLensOut>("/api/doc-lens", { kb, doc, repo, at: "declared" });
}

/// `GET /api/doc-lens/repos?kb=&doc=` — the scorecard (every configured
/// repo, cheaper columns than the full lens above).
export function fetchDocLensRepos(kb: string, doc: string): Promise<ScorecardOut> {
  return getJson<ScorecardOut>("/api/doc-lens/repos", { kb, doc });
}

/// `PUT /api/doc-lens/pin` — remember a checkout for this document
/// (Decision 1: the pin IS the remembered read-time choice). `docHash` is
/// REQUIRED on the wire (`doc_hash: string | null`, never omitted) — pass
/// `null` explicitly when the caller has no hash on hand yet.
export function putDocLensPin(
  kb: string,
  doc: string,
  repo: string,
  docHash: string | null,
): Promise<DocLensPinOut> {
  return sendJson<DocLensPinOut>("PUT", "/api/doc-lens/pin", { kb, doc, repo, doc_hash: docHash });
}

/// `GET /api/doc-lens/resolve-path?kb=&path=` (R2/R20) — the path-addressed
/// lens entry ramp: resolves a source-relative path to kb's artifact id
/// through kb-code's own same-origin route (never a direct cross-origin
/// call into kb — see that route's own doc). 404s (as an `ApiError`) when
/// kb has no doc at that path.
export function resolveDocByPath(kb: string, path: string): Promise<DocLensResolvePathOut> {
  return getJson<DocLensResolvePathOut>("/api/doc-lens/resolve-path", { kb, path });
}

// --- DCB W3.B — the reverse "cited by" lookup -------------------------------

/// `GET /api/doc-refs?repo=&path=` — same-origin, plain `auth_bearer` read
/// (deliberately NOT the CORS'd `doclens_read` set — `crates/kb-code-server/
/// src/router.rs`'s own doc on this route: web-code's `CitedBy` strip is
/// this daemon's own frontend, always same-origin to it).
export function fetchDocRefs(repo: string, path: string): Promise<DocRefsOut> {
  return getJson<DocRefsOut>("/api/doc-refs", { repo, path });
}

// ── PRR-U2 ── kb v0.39 "The PR Room," unit U2 — Report tab + Room cockpit ──
// See `api/types.ts`'s own "── PRR-U2 ──" block for the wire-shape docs
// these calls return.

/// `GET /api/reviews/{id}/report` — bearer read. `{report: null}` when no
/// report has ever been authored (`get_review_report`'s doc); otherwise the
/// raw stored `report_json` object, echoed verbatim.
export function fetchReviewReport(id: number): Promise<ReviewReportOut> {
  return getJson<ReviewReportOut>(`/api/reviews/${id}/report`, {});
}

export interface FetchReviewFindingsParams {
  ps?: string;
  disposition?: string;
  include_superseded?: boolean;
}

/// `GET /api/reviews/{id}/findings?ps=&disposition=&include_superseded=` —
/// bearer read (`review_findings::list_findings_route`'s doc).
export function fetchReviewFindings(
  id: number,
  params: FetchReviewFindingsParams = {},
): Promise<ReviewFindingsOut> {
  return getJson<ReviewFindingsOut>(`/api/reviews/${id}/findings`, {
    ps: params.ps,
    disposition: params.disposition,
    include_superseded: params.include_superseded ? "true" : undefined,
  });
}

/// `PUT /api/reviews/{id}/findings/{slug}/disposition` — LOOPBACK-ONLY
/// (bare 404 off-loopback, same contract every other review mutation in
/// this file documents). Returns the finding's full updated view.
export function putFindingDisposition(
  id: number,
  slug: string,
  input: SetFindingDispositionInput,
): Promise<ReviewFinding> {
  return sendJson<ReviewFinding>(
    "PUT",
    `/api/reviews/${id}/findings/${encodeURIComponent(slug)}/disposition`,
    input,
  );
}

/// `DELETE /api/reviews/{id}/findings/{slug}/disposition` — LOOPBACK-ONLY.
/// Clears the disposition back to undecided; returns the updated view.
export function deleteFindingDisposition(id: number, slug: string): Promise<ReviewFinding> {
  return sendJson<ReviewFinding>(
    "DELETE",
    `/api/reviews/${id}/findings/${encodeURIComponent(slug)}/disposition`,
  );
}

/// `GET /api/reviews/{id}/artifact` — bearer read (design doc §2 row 7).
/// Live, unpersisted — never cache the result across renders beyond
/// TanStack Query's own short-lived staleness (kb-sibling/1's "never cache
/// an unreached probe," this crate's own invariant #2/#4).
export function fetchReviewArtifact(id: number): Promise<ReviewArtifactOut> {
  return getJson<ReviewArtifactOut>(`/api/reviews/${id}/artifact`, {});
}

/// `GET /api/prs/{number}?repo=` (design doc §2 row 2) — one PR's full
/// detail (works for open/closed/merged, unlike the `PrOut` list). Same
/// 400-vs-degrade split every other PR route documents.
export function fetchPrDetail(repo: string, number: number): Promise<PrDetailResponseOut> {
  return getJson<PrDetailResponseOut>(`/api/prs/${number}`, { repo });
}

/// `GET /api/prs/{number}/checks?repo=` (design doc §2 row 3) — every
/// check-run against the PR's CURRENT head sha, `MAX_CHECKS`-capped.
export function fetchPrChecks(repo: string, number: number): Promise<PrChecksOut> {
  return getJson<PrChecksOut>(`/api/prs/${number}/checks`, { repo });
}

/// `GET /api/prs/{number}/reviews?repo=` (addendum-2 §A) — per-reviewer
/// latest submitted state + requested reviewers + a locally-computed
/// `review_decision` approximation.
export function fetchPrReviews(repo: string, number: number): Promise<PrReviewsOut> {
  return getJson<PrReviewsOut>(`/api/prs/${number}/reviews`, { repo });
}

// ── PRR-U3 ── review findings — kept as its OWN import statement (ESM
// import declarations hoist regardless of position in the module) rather
// than editing the big shared `import type {...}` block above, so this
// unit's diff never touches a line a sibling builder might also be editing.
import type { CreateManualFindingInput } from "./types";

/// `POST /api/reviews/{id}/findings` — LOOPBACK-ONLY (addendum §E). Creates
/// one human-authored (`origin: "manual"`) finding, anchored at the
/// composer's line/side.
export function createManualFinding(
  reviewId: number,
  input: CreateManualFindingInput,
): Promise<ReviewFinding> {
  return sendJson<ReviewFinding>("POST", `/api/reviews/${reviewId}/findings`, input);
}

// ── PRR-U1 ── kb v0.39 "The PR Room," unit U1 (Review Room landing) — kept
// as its OWN import statement, same reasoning as PRR-U3's just above.
import type { CreateReviewPrInput, CreateReviewPrOut, ReviewInboxOut } from "./types";

/// `GET /api/reviews/inbox?repo=&state=&limit=` (design doc §2 S1) — the
/// attention queue for the landing page. Bearer. Always called with `repo`
/// set (this SPA is repo-scoped); `state` defaults server-side to `"open"`.
export function fetchReviewInbox(
  repo: string,
  state?: "open" | "closed" | "all",
  limit?: number,
): Promise<ReviewInboxOut> {
  return getJson<ReviewInboxOut>("/api/reviews/inbox", {
    repo,
    state,
    limit: limit !== undefined ? String(limit) : undefined,
  });
}

/// `POST /api/reviews/pr` — LOOPBACK-ONLY (design doc §2 row 1, PRR-R2).
/// Creates a review bound to a GitHub PR (server-side git-fetch + ps1
/// capture + best-effort metadata enrichment) — the "Start review" action
/// on `routes/Prs.tsx`'s unbound rows and `UnreviewedPrsStrip`. Same bare-
/// 404-off-loopback contract as `createReview`/`postPrFetch`. A 409
/// (already bound) carries `{error, existing_review_id}`; only `error`
/// survives onto `ApiError.message` (`getJson`/`sendJson`'s shared error
/// path) — callers don't need `existing_review_id` itself, since their own
/// PR↔review join already hides "Start review" for any PR a review already
/// binds, so a 409 here is a narrow race, not the common path.
export function createReviewPr(input: CreateReviewPrInput): Promise<CreateReviewPrOut> {
  return sendJson<CreateReviewPrOut>("POST", "/api/reviews/pr", input);
}

// ── PRR-U56 ── kb v0.39 "The PR Room," combined unit U5+U6 — publish
// preview, suggestions batch apply, timeline tab. Own import statement (ESM
// import declarations hoist regardless of position), so this unit's diff
// never touches the shared `import type {...}` block above — see PRR-U3's
// identical precedent just above this block.
import type {
  ApplyBatchConflictOut,
  ApplyBatchResultOut,
  AppliedBatchItem,
  PublishFindingInput,
  PublishVerdictInput,
  PublishVerdictOut,
  ReviewGithubExportOut,
  ReviewTimelineOut,
  ReviewTimelineParams,
} from "./types";

export interface FetchReviewExportGithubParams {
  finding_slugs?: string;
  include_waived?: boolean;
  include_orphaned_as_general?: boolean;
}

/// `GET /api/reviews/{id}/export/github?...` — bearer read (design doc §2
/// row 14 / `review_github_export.rs`'s module doc: "pure computation, zero
/// GitHub calls"). An ABSENT `finding_slugs` returns EVERY eligible
/// (non-published, non-waived-by-default) finding, not "nothing" — callers
/// that mean "preview my marked set" must always pass a non-empty csv (see
/// `PublishPreview.tsx`'s own doc).
export function fetchReviewExportGithub(
  id: number,
  params: FetchReviewExportGithubParams = {},
): Promise<ReviewGithubExportOut> {
  return getJson<ReviewGithubExportOut>(`/api/reviews/${id}/export/github`, {
    finding_slugs: params.finding_slugs,
    include_waived: params.include_waived ? "true" : undefined,
    include_orphaned_as_general: params.include_orphaned_as_general ? "true" : undefined,
  });
}

/// `POST /api/reviews/{id}/findings/{slug}/published` — LOOPBACK-ONLY.
/// Advisory-only, idempotent-by-overwrite (route's own doc) — recorded
/// AFTER the agent's/operator's own `gh` call succeeds, never verified.
export function publishFinding(
  id: number,
  slug: string,
  input: PublishFindingInput = {},
): Promise<ReviewFinding> {
  return sendJson<ReviewFinding>(
    "POST",
    `/api/reviews/${id}/findings/${encodeURIComponent(slug)}/published`,
    input,
  );
}

/// `POST /api/reviews/{id}/verdict/published` — LOOPBACK-ONLY. Same
/// advisory/idempotent-by-overwrite posture as `publishFinding`.
export function publishVerdict(
  id: number,
  input: PublishVerdictInput = {},
): Promise<PublishVerdictOut> {
  return sendJson<PublishVerdictOut>("POST", `/api/reviews/${id}/verdict/published`, input);
}

/// `GET /api/reviews/{id}/timeline` — bearer read; pure server-side
/// composition, no new storage (`review_timeline.rs`'s module doc).
///
/// V73-K2c widens this to `review-timeline/2`'s full param set. Every param
/// is omitted at its default (absent ⇒ "the daemon's own default", never a
/// value invented here) so a plain "open the tab" request stays the short
/// URL it always was. `github`/`hunk` are the two conditional lanes — see
/// `ReviewTimelineParams`'s own doc.
export function fetchReviewTimeline(
  id: number,
  params: ReviewTimelineParams = {},
): Promise<ReviewTimelineOut> {
  return getJson<ReviewTimelineOut>(`/api/reviews/${id}/timeline`, {
    kind: params.kind,
    author: params.author,
    since: params.since !== undefined ? String(params.since) : undefined,
    until: params.until !== undefined ? String(params.until) : undefined,
    limit: params.limit !== undefined ? String(params.limit) : undefined,
    offset: params.offset !== undefined ? String(params.offset) : undefined,
    github: params.github !== undefined ? (params.github ? "true" : "false") : undefined,
    hunk: params.hunk,
    ps: params.ps,
  });
}

/// Structured 409 from `POST /api/annotations/apply-batch` — the
/// verify-phase's per-id verdicts. Nothing was written to any file (the
/// whole batch is rejected together — see `suggestions.rs`'s module doc).
export class ApplyBatchConflictError extends ApiError {
  verdicts: ApplyBatchConflictOut["verdicts"];
  constructor(body: ApplyBatchConflictOut) {
    super(409, "apply-batch verification failed — nothing written");
    this.name = "ApplyBatchConflictError";
    this.verdicts = body.verdicts;
  }
}

/// The rarer mid-batch IO-failure 500 (`suggestions.rs::restore_and_report`)
/// — every already-written file this batch touched is best-effort restored;
/// `restored` names the ones that rolled back cleanly, `failed` names the
/// one whose write actually faulted. Distinct from `ApplyBatchConflictError`
/// (verify-phase rejection, nothing was ever written).
export class ApplyBatchWriteError extends ApiError {
  applied: AppliedBatchItem[];
  restored: string[];
  failed: ApplyBatchResultOut["failed"];
  constructor(body: ApplyBatchResultOut) {
    super(500, body.failed?.error ?? "apply-batch write failed mid-batch");
    this.name = "ApplyBatchWriteError";
    this.applied = body.applied;
    this.restored = body.restored;
    this.failed = body.failed;
  }
}

function isApplyBatchConflictBody(body: unknown): body is ApplyBatchConflictOut {
  if (!body || typeof body !== "object") return false;
  return Array.isArray((body as Record<string, unknown>).verdicts);
}

function isApplyBatchResultBody(body: unknown): body is ApplyBatchResultOut {
  if (!body || typeof body !== "object") return false;
  return "restored" in (body as Record<string, unknown>) && "applied" in (body as Record<string, unknown>);
}

/// `POST /api/annotations/apply-batch` — LOOPBACK-ONLY (addendum-2 §F,
/// `suggestions.rs`'s two-phase verify-then-apply contract). Body
/// `{annotation_ids, resolve_threads?}`. Manual `fetch` (not `sendJson`) —
/// same reasoning as `applyAnnotationSuggestion` above: a 409/500 here
/// carries a structured body a bare `ApiError` would swallow.
export async function applySuggestionsBatch(
  annotationIds: string[],
  resolveThreads = false,
): Promise<ApplyBatchResultOut> {
  const res = await fetch("/api/annotations/apply-batch", {
    method: "POST",
    headers: {
      Accept: "application/json",
      ...KBC_REQUEST_HEADERS,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ annotation_ids: annotationIds, resolve_threads: resolveThreads }),
  });
  let body: unknown = null;
  try {
    body = await res.json();
  } catch {
    // Non-JSON (loopback bare 404) — fall through.
  }
  if (res.status === 409 && isApplyBatchConflictBody(body)) {
    throw new ApplyBatchConflictError(body);
  }
  if (res.status === 500 && isApplyBatchResultBody(body)) {
    throw new ApplyBatchWriteError(body);
  }
  if (!res.ok) {
    const message = (body as { error?: string } | null)?.error ?? res.statusText;
    throw new ApiError(res.status, message);
  }
  return body as ApplyBatchResultOut;
}

// ── PRR-U9 — diagnostics through lip (design-addendum-2.md §D) ────────────

/// `GET /api/diagnostics?repo=&path=` — computed fresh per request, never
/// persisted (`lip.rs::diagnostics_route`'s own doc). `hooks/
/// useDiagnostics.ts` is the only caller; see that hook's doc for the
/// (repo, lang)-coverage gate that keeps this from firing for a file no
/// configured provider covers.
export function fetchDiagnostics(repo: string, path: string): Promise<DiagnosticsOut> {
  return getJson<DiagnosticsOut>("/api/diagnostics", { repo, path });
}

// ── PRR-F — GitHub threads, recurring-finding memory, reviewer X-ray ──────
// (design-ui.md §12 items 1/2/4, design-addendum-2 §A). Own import
// statement, same "never touches a line a sibling builder might also be
// editing" precedent every other PRR-* block in this file already uses.
import type {
  FindingsRecurrenceOut,
  GithubThreadsOut,
  ReviewImpactFileOut,
} from "./types";

/// `GET /api/reviews/{id}/github-threads` (`kbc-github-threads/1`,
/// design-addendum-2 §A) — bearer, live-composed, nothing persisted. A
/// GitHub-side failure degrades to 200 + `unavailable_reason` (the route's
/// own doc); a non-PR-bound or patchset-less review 400s (surfaces as an
/// `ApiError` a caller can branch on, same as every other review route).
export function fetchGithubThreads(id: number): Promise<GithubThreadsOut> {
  return getJson<GithubThreadsOut>(`/api/reviews/${id}/github-threads`, {});
}

/// `GET /api/reviews/{id}/findings/recurrence` (design-ui.md §12.4).
export function fetchFindingsRecurrence(id: number): Promise<FindingsRecurrenceOut> {
  return getJson<FindingsRecurrenceOut>(`/api/reviews/${id}/findings/recurrence`, {});
}

/// `GET /api/reviews/{id}/impact?path=` (design-ui.md §12.2, "Reviewer
/// X-ray") — a SMALL per-file aggregate, distinct from the general
/// `fetchImpactAnalysis` (`GET /api/impact/analysis`) `BlastRadiusStrip`
/// already uses: no transitive BFS, no provenance, just callers-in-diff vs
/// out-of-diff counts for the file's own changed callable symbols. `400`
/// when `path` isn't part of the review's latest patchset change set.
export function fetchReviewImpact(id: number, path: string): Promise<ReviewImpactFileOut> {
  return getJson<ReviewImpactFileOut>(`/api/reviews/${id}/impact`, { path });
}

// ── PRR-U8 (design-addendum-2.md §C) — the Review Room landing's Analytics
// section. Own import statement (never touches a line a sibling builder
// might also be editing), same precedent every other PRR-* block in this
// file already established.
import type { ReviewAnalyticsOut } from "./types";

export interface FetchReviewAnalyticsParams {
  repo?: string;
  from?: number;
  to?: number;
}

/// `GET /api/reviews/analytics?repo=&from=&to=` — bearer, repo-scoped read
/// (`review_analytics.rs`'s own module doc). `repo` 404s on an unknown
/// name; absent scans every finding across every configured repo (this
/// unit always passes it — the landing page is already repo-scoped by its
/// own URL).
export function fetchReviewAnalytics(
  params: FetchReviewAnalyticsParams = {},
): Promise<ReviewAnalyticsOut> {
  return getJson<ReviewAnalyticsOut>("/api/reviews/analytics", {
    repo: params.repo,
    from: params.from != null ? String(params.from) : undefined,
    to: params.to != null ? String(params.to) : undefined,
  });
}

// ── S2-A: unified inbox — kb-code v6.0 "One Inbox" (design-s2.md §S2-A) ───
// Own import statement, same "never touches a line a sibling builder might
// also be editing" precedent every other block in this file already uses.
import type { UnifiedInboxOut } from "./types";

/// `GET /api/inbox` (`unified-inbox/1`) — bearer, ordinary read. Composes
/// three lanes server-side (reviews/annotations/kb); see `UnifiedInboxOut`'s
/// doc. No query params — the route is unscoped by design (federated across
/// every configured repo, plus kb when reachable).
export function fetchUnifiedInbox(): Promise<UnifiedInboxOut> {
  return getJson<UnifiedInboxOut>("/api/inbox", {});
}
// ── /S2-A ──

// ── S2-C (B4) — quick fixes: `POST /api/code-actions` (`code-actions/1`,
// design-s2.md Addendum — EXACT pin, B1 emits/B4 consumes) + the
// `POST /api/annotations/batch` `add_comment(+suggestion)` creation call
// (`routes.rs::batch_annotations`/`AnnotationBatchOp::AddComment` at the
// fork sha — this crate's `client.ts` had no wrapper for that route yet,
// so this adds ONE minimally-scoped op type covering exactly the variant
// this unit uses, not the full 9-variant Rust enum). Own import statement,
// same "never touches a line a sibling builder might also be editing"
// precedent every other block in this file already establishes.
import type { CodeActionsOut } from "./types";

export interface CodeActionsRequest {
  repo: string;
  path: string;
  start_line: number;
  start_col: number;
  end_line?: number;
  end_col?: number;
  kinds?: string[];
}

/// `POST /api/code-actions` — ordinary bearer READ-shaped route (nothing
/// persisted server-side, `lip.rs`'s computed-fresh-never-persisted law —
/// same posture `fetchDiagnostics` above cites for its own sibling route).
export function fetchCodeActions(req: CodeActionsRequest): Promise<CodeActionsOut> {
  return sendJson<CodeActionsOut>("POST", "/api/code-actions", req);
}

/// The ONE `AnnotationBatchOp` variant this unit needs
/// (`routes.rs::AnnotationBatchOp::AddComment`, field-for-field) — see this
/// block's own header doc for why a minimally-scoped mirror rather than the
/// full tagged union.
export interface AnnotationBatchAddCommentOp {
  op: "add_comment";
  path: string;
  line?: number;
  line_end?: number;
  body: string;
  author?: string;
  anchor_kind?: AnchorKind;
  sha?: string;
  intent?: AnnotationIntent;
  review_id?: number;
  ps?: number;
  side?: "old" | "new";
  suggestion?: { replacement: string };
}

export interface AnnotationsBatchRequest {
  repo: string;
  ops: AnnotationBatchAddCommentOp[];
}

/// `routes.rs::batch_annotations`'s 200 body — `{applied, created_ids,
/// changed}`. `created_ids` lands in the SAME order the ops were submitted
/// (only `AddComment`/`AddReply` push an id; every op here is an
/// `AddComment`, so `created_ids[i]` corresponds to `ops[i]`).
export interface AnnotationsBatchResult {
  applied: number;
  created_ids: string[];
  changed: boolean;
}

/// `POST /api/annotations/batch` — BEARER, ONE store transaction, at most
/// ONE SSE (`routes.rs::batch_annotations`'s own doc) — the audited
/// creation path `lib/codeActions.ts`'s quick-fix conversion goes through
/// rather than a bespoke mutation route.
export function postAnnotationsBatch(req: AnnotationsBatchRequest): Promise<AnnotationsBatchResult> {
  return sendJson<AnnotationsBatchResult>("POST", "/api/annotations/batch", req);
}

// --- V71-E2 — the classified usages wire + the action registry ------------

import type { ActionsOut, Usages2Out } from "./types";

/// `GET /api/usages/2?repo=&path=&line=&col=[&ref=][&limit=]` — D4's
/// classified ladder, the wire the SPA's `gr`, the lens chip and the peek
/// `u` all repoint onto. `/api/xrefs` (the grep tier) is still fetched, but
/// only as the explicit "mentions" chip beside it: it is a different
/// question, so it never silently changes a classified count.
export function fetchUsages2(q: {
  repo: string;
  path: string;
  line: number;
  col: number;
  ref?: string;
  limit?: number;
}): Promise<Usages2Out> {
  return getJson<Usages2Out>("/api/usages/2", {
    repo: q.repo,
    path: q.path,
    line: String(q.line),
    col: String(q.col),
    ref: q.ref,
    limit: q.limit !== undefined ? String(q.limit) : undefined,
  });
}

/// `GET /api/actions?repo=&path=&line=&col=…` — `kbc-actions/1`. The menu,
/// the drag-select pill, the mobile sheet and `kb-code act` all render THIS
/// response; nothing composes an action client-side (risk 10).
export function fetchActions(q: {
  repo: string;
  path: string;
  line: number;
  col: number;
  ref?: string;
  endLine?: number;
  endCol?: number;
  text?: string;
  target?: number;
}): Promise<ActionsOut> {
  return getJson<ActionsOut>("/api/actions", {
    repo: q.repo,
    path: q.path,
    line: String(q.line),
    col: String(q.col),
    ref: q.ref,
    end_line: q.endLine !== undefined ? String(q.endLine) : undefined,
    end_col: q.endCol !== undefined ? String(q.endCol) : undefined,
    text: q.text,
    target: q.target !== undefined ? String(q.target) : undefined,
  });
}

// --- V72-G1.2 — `entity/1`, the entity dossier -------------------------------

/// `GET /api/entity/dossier` — everything about exactly ONE entity
/// (`crates/kb-code-server/src/entities/dossier.rs`).
///
/// Three of the four params are the caller's own cuts and are OMITTED at their
/// defaults, so a plain "open the dossier" request is the shortest URL the
/// route accepts and the query key below stays stable across renders:
///
/// - `inherited` — the wire spells it `?inherited=1` (`DossierParams.inherited`
///   is an `Option<String>`, not a bool, precisely so the documented address
///   works); absent means "definitions and members this entity's own body
///   declares".
/// - `budget` — the ROW budget. Absent ⇒ the route's `DEFAULT_BUDGET` (600).
/// - `usagesPerKind` — absent ⇒ `DEFAULT_USAGES_PER_KIND` (20). Raising it is
///   how "show more" re-asks; the page NEVER slices a group client-side, since
///   the rows it did not receive are not rows it can show (invariant: every
///   count comes from the wire).
export interface DossierQuery {
  repo: string;
  ent: string;
  inherited?: boolean;
  budget?: number;
  usagesPerKind?: number;
}

export function fetchDossier(q: DossierQuery): Promise<DossierOut> {
  return getJson<DossierOut>("/api/entity/dossier", {
    repo: q.repo,
    ent: q.ent,
    inherited: q.inherited ? "1" : undefined,
    budget: q.budget !== undefined ? String(q.budget) : undefined,
    usages_per_kind: q.usagesPerKind !== undefined ? String(q.usagesPerKind) : undefined,
  });
}

// ── kbc-canvas/1 — boards (V74-L2) ─────────────────────────────────────────
//
// Four READS on the ordinary bearer surface and two loopback-only MUTATIONS.
// The SPA never composes an action for them beyond the document itself
// (`lib/boardDoc.ts`); the lint, the honesty census and every count come back
// from the daemon.

/// `GET /api/boards?repo=[&status=]`.
export function fetchBoards(repo: string, status?: string): Promise<BoardsListOut> {
  return getJson<BoardsListOut>("/api/boards", { repo, status });
}

/// `GET /api/boards/{slug}?repo=[&ctx=1][&live=1]`. Both flags are OFF by
/// default and each is a deliberate opt-in: `ctx` asks the daemon to read the
/// context range's text, `live` asks it to EXECUTE every query card (a full
/// unified search per card — never on an ordinary page load).
export function fetchBoard(
  repo: string,
  slug: string,
  opts: { ctx?: boolean; live?: boolean } = {},
): Promise<BoardOut> {
  return getJson<BoardOut>(`/api/boards/${encodeURIComponent(slug)}`, {
    repo,
    ctx: opts.ctx ? "1" : undefined,
    live: opts.live ? "1" : undefined,
  });
}

/// `GET /api/boards/sweep?repo=[&slug=]` — the drift report. A READ: it
/// re-resolves and reports, and never repairs what it finds.
export function fetchBoardSweep(repo: string, slug?: string): Promise<BoardSweepOut> {
  return getJson<BoardSweepOut>("/api/boards/sweep", { repo, slug });
}

/// `POST /api/boards/apply` — LOOPBACK-ONLY. The WHOLE document, upserted by
/// slug; there is no partial-patch route, which is why `lib/boardDoc.ts`
/// composes the next document from the one on screen.
export function applyBoard(
  doc: unknown,
  opts: { dryRun?: boolean; allowDisconnected?: boolean } = {},
): Promise<BoardApplyOut> {
  const qs = new URLSearchParams();
  if (opts.dryRun) qs.set("dry_run", "1");
  if (opts.allowDisconnected) qs.set("allow_disconnected", "1");
  const suffix = qs.toString();
  return sendJson<BoardApplyOut>(
    "POST",
    suffix ? `/api/boards/apply?${suffix}` : "/api/boards/apply",
    doc,
  );
}

/// `POST /api/boards/{slug}/accept?repo=` — LOOPBACK-ONLY (D21: an
/// agent-proposed board is PENDING until a human accepts it).
export function acceptBoard(repo: string, slug: string): Promise<BoardStatusOut> {
  return sendJson<BoardStatusOut>(
    "POST",
    `/api/boards/${encodeURIComponent(slug)}/accept?repo=${encodeURIComponent(repo)}`,
  );
}

/// `POST /api/boards/{slug}/archive?repo=` — LOOPBACK-ONLY.
export function archiveBoard(repo: string, slug: string): Promise<BoardStatusOut> {
  return sendJson<BoardStatusOut>(
    "POST",
    `/api/boards/${encodeURIComponent(slug)}/archive?repo=${encodeURIComponent(repo)}`,
  );
}

// ── kbc-tour/1 + kbc-trail/1 (V74-L3b) ─────────────────────────────────────
//
// Tours mirror boards exactly (four bearer reads, two loopback mutations)
// because a tour IS a board — see `api/types.ts`'s own note.
//
// Trails split the OTHER way, and the split is the privacy posture rather
// than a convenience: `state` and `aggregate` are ordinary bearer reads (the
// indicator has to render, and the aggregate IS the agent-facing surface),
// while the two HUMAN reads and every write are loopback-only. A non-loopback
// browser therefore gets an honest 404 from `fetchTrails`, which
// `hooks/useTrails.ts` folds into "not available here" rather than an error
// toast.

/// `GET /api/tours?repo=[&status=]`.
export function fetchTours(repo: string, status?: string): Promise<ToursListOut> {
  return getJson<ToursListOut>("/api/tours", { repo, status });
}

/// `GET /api/tours/{slug}?repo=[&ctx=1]` — every step re-resolved NOW.
export function fetchTour(
  repo: string,
  slug: string,
  opts: { ctx?: boolean } = {},
): Promise<TourOut> {
  return getJson<TourOut>(`/api/tours/${encodeURIComponent(slug)}`, {
    repo,
    ctx: opts.ctx ? "1" : undefined,
  });
}

/// `POST /api/tours/apply` — LOOPBACK-ONLY. The WHOLE document by slug;
/// `lib/tourDoc.ts` composes it.
export function applyTour(doc: unknown, opts: { dryRun?: boolean } = {}): Promise<TourApplyOut> {
  const path = opts.dryRun ? "/api/tours/apply?dry_run=1" : "/api/tours/apply";
  return sendJson<TourApplyOut>("POST", path, doc);
}

/// `DELETE /api/tours/{slug}?repo=` — LOOPBACK-ONLY.
export function deleteTour(repo: string, slug: string): Promise<void> {
  return sendJson<void>(
    "DELETE",
    `/api/tours/${encodeURIComponent(slug)}?repo=${encodeURIComponent(repo)}`,
  );
}

/// `GET /api/trails/state` — bearer. Always answers, even with the ledger
/// off: "off" is a state to render, not an error.
export function fetchTrailState(): Promise<TrailStateOut> {
  return getJson<TrailStateOut>("/api/trails/state", {});
}

/// `POST /api/trails/state` — LOOPBACK-ONLY and audited. The ONLY writer of
/// the opt-in.
export function setTrailState(mode: string): Promise<TrailStateOut> {
  return sendJson<TrailStateOut>("POST", "/api/trails/state", { mode });
}

/// `GET /api/trails?repo=[&limit=]` — LOOPBACK-ONLY (the operator's own
/// movement record never leaves their box).
export function fetchTrails(repo: string, limit?: number): Promise<TrailsListOut> {
  return getJson<TrailsListOut>("/api/trails", {
    repo,
    limit: limit === undefined ? undefined : String(limit),
  });
}

/// `GET /api/trails/{id}?repo=[&from=][&notes=1]` — LOOPBACK-ONLY.
export function fetchTrail(
  repo: string,
  id: string,
  opts: { from?: number; notes?: boolean } = {},
): Promise<TrailOut> {
  return getJson<TrailOut>(`/api/trails/${encodeURIComponent(id)}`, {
    repo,
    from: opts.from === undefined ? undefined : String(opts.from),
    notes: opts.notes ? "1" : undefined,
  });
}

/// `POST /api/trails/purge` — LOOPBACK-ONLY and audited. WHOLESALE unless an
/// `id` narrows it. Deliberately NOT gated on `[trails] enabled`: an operator
/// who just turned the feature off must still be able to delete what it
/// recorded.
export function purgeTrails(repo: string, opts: { id?: string } = {}): Promise<TrailPurgeOut> {
  return sendJson<TrailPurgeOut>("POST", "/api/trails/purge", { repo, ...(opts.id ? { id: opts.id } : {}) });
}

/// `POST /api/trails/{id}/fork` — LOOPBACK-ONLY. Carries a COPY of the steps
/// from the branch point, so a fork reads as "I went another way from here"
/// rather than as an empty trail with a pointer.
export function forkTrail(
  repo: string,
  id: string,
  fromOrdinal: number,
  title?: string,
): Promise<TrailCreatedOut> {
  return sendJson<TrailCreatedOut>("POST", `/api/trails/${encodeURIComponent(id)}/fork`, {
    repo,
    from_ordinal: fromOrdinal,
    ...(title ? { title } : {}),
  });
}

// --- V72-I2 — `rails/1` (`GET /api/rails/*`) --------------------------------
//
// Ten routes, three shapes. `RAILS_NOUN_SEGMENT` is the ONE place the noun →
// URL-segment pluralisation lives on this side; the server's own closed
// vocabulary is `rails::NOUNS` (mirrored for the search grammar in
// `lib/kbcq.ts`'s `RAILS_NOUNS`), and `railsNounSegment` is total over it so
// a ninth noun on the wire degrades to a named 404 rather than a silent
// wrong-lane fetch.

import type { RailsHomeOut, RailsListOut, RailsOrphansOut } from "./types";

/// noun → the `/api/rails/<segment>` path segment. English pluralisation is
/// irregular enough (`mailer` → `mailers`, but the segment set is fixed
/// server-side) that a table beats a rule.
const RAILS_NOUN_SEGMENT: Readonly<Record<string, string>> = {
  model: "models",
  controller: "controllers",
  action: "actions",
  route: "routes",
  job: "jobs",
  mailer: "mailers",
  view: "views",
  concern: "concerns",
};

export function railsNounSegment(noun: string): string | null {
  return RAILS_NOUN_SEGMENT[noun] ?? null;
}

/// `GET /api/rails/home?repo=` — the passport.
export function fetchRailsHome(repo: string): Promise<RailsHomeOut> {
  return getJson<RailsHomeOut>("/api/rails/home", { repo });
}

/// `GET /api/rails/<plural>?repo=[&q=][&limit=][&offset=]` — one noun's page.
/// Paging is the SERVER's (`limit`/`offset` with a TRUE `total`); this client
/// never slices a page it already holds.
export function fetchRailsNoun(q: {
  repo: string;
  noun: string;
  q?: string;
  limit?: number;
  offset?: number;
}): Promise<RailsListOut> {
  const seg = railsNounSegment(q.noun);
  if (!seg) {
    return Promise.reject(new Error(`unknown rails/1 noun ${JSON.stringify(q.noun)}`));
  }
  return getJson<RailsListOut>(`/api/rails/${seg}`, {
    repo: q.repo,
    q: q.q,
    limit: q.limit !== undefined ? String(q.limit) : undefined,
    offset: q.offset !== undefined ? String(q.offset) : undefined,
  });
}

/// `GET /api/rails/orphans?repo=` — the six-lane triage queue.
export function fetchRailsOrphans(repo: string): Promise<RailsOrphansOut> {
  return getJson<RailsOrphansOut>("/api/rails/orphans", { repo });
}

// ── V73-K2c — kbc-claim/1, kbc-pseudo/1, kbc-hunk-turns/1 (SPA half of
// the review-timeline/2 stream: the claim register, pseudo-files, and the
// on-demand hunk↔turn join). Own import statement, same append-only
// precedent every prior PRR-*/V7* block in this file already established.
import type {
  ClaimsListOut,
  FetchClaimsParams,
  HunkTurnsOut,
  PseudoFileOut,
  PseudoSetOut,
} from "./types";

/// `GET /api/claims?repo=&subject=&subject_kind=&path=&review=&kind=&limit=&offset=`
/// — bearer, `repo` REQUIRED (`claims.rs`'s `CLAIMS_ROUTE` contract). Every
/// other param narrows; omitted means "every claim in the repo", so a caller
/// scoping to one review must pass `review` explicitly.
export function fetchClaims(params: FetchClaimsParams): Promise<ClaimsListOut> {
  return getJson<ClaimsListOut>("/api/claims", {
    repo: params.repo,
    subject: params.subject,
    subject_kind: params.subject_kind,
    path: params.path,
    review: params.review !== undefined ? String(params.review) : undefined,
    kind: params.kind,
    limit: params.limit !== undefined ? String(params.limit) : undefined,
    offset: params.offset !== undefined ? String(params.offset) : undefined,
  });
}

/// `GET /api/reviews/{id}/pseudo?ps=` (`kbc-pseudo/1`) — the four pseudo-file
/// names, content-free (the list read never carries `content`).
export function fetchReviewPseudoList(id: number, ps?: string): Promise<PseudoSetOut> {
  return getJson<PseudoSetOut>(`/api/reviews/${id}/pseudo`, { ps });
}

/// `GET /api/reviews/{id}/pseudo/{name}?ps=` — the single-file read, with
/// `content` populated. `name` is one of the four reserved leaf names
/// (`pr-body.md`/`review.md`/`findings.json`/`commits.md`), never the
/// `~review/`-prefixed path.
export function fetchReviewPseudoFile(
  id: number,
  name: string,
  ps?: string,
): Promise<PseudoFileOut> {
  return getJson<PseudoFileOut>(`/api/reviews/${id}/pseudo/${encodeURIComponent(name)}`, { ps });
}

/// `GET /api/reviews/{id}/hunks/{hunk}/turns?ps=` (`kbc-hunk-turns/1`) —
/// LOOPBACK-ONLY: the join reads raw transcript content (D19's
/// `raw-transcript` sensitivity class). Off loopback this rejects the same
/// way any other loopback-gated route does (`ApiError`); callers render
/// `e.message` as the honest refusal rather than assuming a specific status
/// code, since the route is mounted behind the loopback sub-router rather
/// than an in-handler check.
export function fetchHunkTurns(id: number, hunk: string, ps?: string): Promise<HunkTurnsOut> {
  return getJson<HunkTurnsOut>(`/api/reviews/${id}/hunks/${encodeURIComponent(hunk)}/turns`, { ps });
}

/// `GET /api/lanes[?repo=]` — `aug-lane/1` registry + enablement + counts.
export function fetchLanes(repo?: string): Promise<LanesOut> {
  return getJson<LanesOut>("/api/lanes", { repo });
}

/// `GET /api/lanes/facts?repo=&path=[&lane=][&at_blob=]` — per-request classing.
export function fetchLaneFacts(
  repo: string,
  path: string,
  opts?: { lane?: string; atBlob?: string },
): Promise<FactsOut> {
  return getJson<FactsOut>("/api/lanes/facts", {
    repo,
    path,
    lane: opts?.lane,
    at_blob: opts?.atBlob,
  });
}

/// `GET /api/lanes/summary?repo=` — stored-claim counts, no class.
export function fetchLanesSummary(repo: string): Promise<LanesSummaryOut> {
  return getJson<LanesSummaryOut>("/api/lanes/summary", { repo });
}
