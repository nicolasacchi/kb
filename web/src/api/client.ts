// Fetch wrapper for the kb HTTP API. Always sends Origin (the browser
// does this automatically) and surfaces RFC 7807 problem+json errors as
// readable Error messages. The `base` URL is empty by default so
// requests go to the same origin the SPA loaded from; D7 will switch
// to a per-daemon URL pulled from localStorage.

// Wire types adopted from the ts-rs generated bindings (see
// ./generated/ + `just types`) — definitionally in sync with the Rust
// structs, field docs included. Imported (not just re-exported) so the
// fetchers below can reference them; exported under their historical
// client.ts names. Types that stay hand-written (DocSummary, Anchor, …)
// are deliberately wider than one endpoint's wire shape;
// web/src/api/drift.ts holds those to the generated truth.
import type { IdentityResponse as Identity } from "./generated/IdentityResponse";
import type { KbSummary } from "./generated/KbSummary";
import type { Hit as SearchHit } from "./generated/Hit";
import type { SearchResponse } from "./generated/SearchResponse";
import type { Choice } from "./generated/Choice";
import type { Attachment } from "./generated/Attachment";
import type { Reply } from "./generated/Reply";
import type { Comment } from "./generated/Comment";
import type { ArtifactRef } from "./generated/ArtifactRef";
import type { ReviewFile } from "./generated/ReviewFile";
import type { SourceSummary } from "./generated/SourceSummary";
import type { TagSummary } from "./generated/TagSummary";
import type { FolderNode } from "./generated/FolderNode";
import type { FoldersResponse } from "./generated/FoldersResponse";
import type { FacetsResponse } from "./generated/FacetsResponse";
import type { FacetBucket } from "./generated/FacetBucket";
import type { KbStats } from "./generated/KbStats";
import type { CrossStats } from "./generated/CrossStats";
import type { ErrorEntry } from "./generated/ErrorEntry";
import type { RecallHit } from "./generated/RecallHit";
import type { RecallResponse } from "./generated/RecallResponse";
import type { MemoryPinResponse } from "./generated/MemoryPinResponse";
import type { MemorySalienceResponse } from "./generated/MemorySalienceResponse";
import type { MemoryPromoteResponse } from "./generated/MemoryPromoteResponse";
import type { MemoryLinks } from "./generated/MemoryLinks";
import type { MemoryLineageNode } from "./generated/MemoryLineageNode";
import type { MemoryLineageResponse } from "./generated/MemoryLineageResponse";
import type { MemoryTriageItem } from "./generated/MemoryTriageItem";
import type { MemoryTriageResponse } from "./generated/MemoryTriageResponse";
import type { MemoryFromRow } from "./generated/MemoryFromRow";
import type { MemoriesFromResponse } from "./generated/MemoriesFromResponse";
import type { SavedQuery as SavedQueryWire } from "./generated/SavedQuery";
import type { SavedQueriesList } from "./generated/SavedQueriesList";
import type { CorkboardEntry } from "./generated/CorkboardEntry";
import type { CorkboardResponse } from "./generated/CorkboardResponse";
import type { AnchorPinResponse } from "./generated/AnchorPinResponse";
import type { StaleAnchorRow } from "./generated/StaleAnchorRow";
import type { StaleAnchorsResponse } from "./generated/StaleAnchorsResponse";
import type { ListSummary } from "./generated/ListSummary";
import type { ListEntry } from "./generated/ListEntry";
import type { ReadState } from "./generated/ReadState";
import type { ReadOverride } from "./generated/ReadOverride";
import type { ListIndexResponse } from "./generated/ListIndexResponse";
import type { ListDetailResponse } from "./generated/ListDetailResponse";
import type { ListCreateBody } from "./generated/ListCreateBody";
import type { ListEntryCreateBody } from "./generated/ListEntryCreateBody";
import type { ResurfaceItem } from "./generated/ResurfaceItem";
import type { ResurfaceResponse } from "./generated/ResurfaceResponse";
import type { ExclusionEntry } from "./generated/ExclusionEntry";
import type { ExcludeBody } from "./generated/ExcludeBody";
import type { ExcludeResponse } from "./generated/ExcludeResponse";
import type { IncludeResponse } from "./generated/IncludeResponse";
import type { ZeroHitGroup } from "./generated/ZeroHitGroup";
import type { KbZeroHit } from "./generated/KbZeroHit";
import type { PromptResponse } from "./generated/PromptResponse";
import type { Verdict } from "./generated/Verdict";
import type { VerdictState } from "./generated/VerdictState";
import type { SessionReplayResponse } from "./generated/SessionReplayResponse";
import type { ReplayBeatOut } from "./generated/ReplayBeatOut";
import type { ReplayBeat } from "./generated/ReplayBeat";
import type { ReplayKind } from "./generated/ReplayKind";
import type { UsersResponse } from "./generated/UsersResponse";
import type { UserEntry } from "./generated/UserEntry";
// DCB W1.D.R #10 — was a mid-file `import type` right above `fetchCodeRefs`
// (its only use site); moved up with every other generated-type import.
import type { CodeRefsResponse } from "./generated/CodeRefsResponse";
import type { CodeRefsFeedResponse } from "./generated/CodeRefsFeedResponse";
import { CODEREF_FEED_PAGE_LIMIT } from "../lib/codeRefCounts";
export type {
  ExclusionEntry,
  ExcludeResponse,
  IncludeResponse,
  ZeroHitGroup,
  KbZeroHit,
};
export type {
  SourceSummary,
  TagSummary,
  FolderNode,
  FoldersResponse,
  FacetsResponse,
  FacetBucket,
  KbStats,
  CrossStats,
  ErrorEntry,
  RecallHit,
  RecallResponse,
  MemoryFromRow,
  MemoriesFromResponse,
  MemoryLinks,
  SavedQueryWire,
  SavedQueriesList,
  CorkboardEntry,
  CorkboardResponse,
  StaleAnchorRow,
  StaleAnchorsResponse,
  ListSummary,
  ListEntry,
  ReadState,
  ReadOverride,
  ListIndexResponse,
  ListDetailResponse,
  ListCreateBody,
  ListEntryCreateBody,
};
export type {
  Identity,
  KbSummary,
  SearchHit,
  SearchResponse,
  Choice,
  Attachment,
  Reply,
  Comment,
  ArtifactRef,
  ReviewFile,
  Verdict,
  VerdictState,
  UsersResponse,
  UserEntry,
};

export type DocSummary = {
  id: string;
  title: string;
  path: string;
  /// v0.8 G1 — parent directory relative to the kb source root. Always
  /// present on list/single responses; empty string for docs at the root.
  /// The gallery's folder filter + grouped view key on this without
  /// re-parsing `path`.
  folder: string;
  /// Track U — full source-root-relative path (forward-slash separated,
  /// e.g. `ideas/foo/bar.html`). Backs the path-based permalink
  /// `/a/<kb>/<source_relative>`. Always present on list/single responses.
  source_relative: string;
  /// Absent (not null) when the artifact declared no category — the
  /// daemon omits it (skip_serializing_if), like every optional below.
  kb_category?: string | null;
  /// v0.7 S1 — optional `<meta name="kb-status">` content (free-form).
  kb_status?: string | null;
  /// v0.7 S1 — optional `<meta name="kb-severity">` content (free-form).
  kb_severity?: string | null;
  // v0.3 — populated when the request used `?include=atlas`. The
  // daemon omits these when null (skip_serializing_if), so the SPA
  // treats `undefined` and `null` interchangeably.
  atlas_x?: number | null;
  atlas_y?: number | null;
  atlas_cluster?: number | null;
  // v0.6 — populated by later phases. UI gracefully degrades when
  // missing.
  mtime_unix?: number | null;
  indexed_at_unix?: number | null;
  // v0.15 — filesystem birth time (btime) of the source file. Backs the
  // gallery's "created" sort. Null on btime-less filesystems and on
  // rows indexed before v0.15.
  created_unix?: number | null;
  // v0.33 Y2 — first time this row was indexed. Stable across edits;
  // optional forever (absent on old rows until the daemon seeds). Used
  // by the reader Folder sidebar "created" sort as the mid-chain fallback
  // (`created ?? first_indexed ?? mtime`).
  first_indexed_unix?: number | null;
  summary?: string | null;
  tags?: string[] | null;
  word_count?: number | null;
  backlinks?: number | null;
  outlinks?: number | null;
  // capability indicators (B1 — bool + counts where the parser
  // already records them).
  svg_count?: number | null;
  table_count?: number | null;
  code_block_count?: number | null;
  has_canvas?: boolean | null;
  has_form?: boolean | null;
  has_animation?: boolean | null;
  has_details?: boolean | null;
  has_math?: boolean | null;
  has_drag?: boolean | null;
  longread?: boolean | null;
  // W1.A — server-derived read-state, joined in from the per-kb reading
  // rollup on the docs-list route's `default` projection only (`slim`/
  // `atlas` omit these, same as backlinks/outlinks). Same wire strings as
  // the search `Hit` (`unread|in_progress|read`).
  read_state?: string | null;
  read_pct?: number | null;
  last_opened_unix?: number | null;
  // CT-F4 — session residue: how many OTHER memories were born in this
  // artifact's own kb-session, summed across the daemon's memory-scoped
  // corpora. Decorated page-scoped on the docs-list route's `default`
  // projection only, and ABSENT when zero (never a "0 kept" badge) — so a
  // card renders it iff it's truthy. Display only: the daemon computes it
  // post-filter and it never reaches ranking.
  session_residue?: number | null;
  // multi-file artifact (M1).
  pages?: Page[] | null;
};

export type Page = {
  id: string;
  label: string;
  order: number;
};

/// v0.8 G1 — folder tree node. Full path relative to the kb source
/// root, descendant-inclusive count, lexicographically-sorted children.

type Problem = {
  type: string;
  title: string;
  status: number;
  detail?: string;
};

// D7-prep — read the daemon base URL on every fetch from a shared
// module. Defaults to "" (same-origin) so today's behaviour is
// bit-identical, but the D7 implementation round can wire
// `setDaemonBase(...)` at boot to switch the whole API surface to a
// remote daemon. See `api/base.ts` for rationale.
import { currentDaemonBase } from "./base";

// Every fetcher takes an optional `signal: AbortSignal`. M-SPA: pre-fix
// the SPA had no AbortController anywhere; an in-flight request from
// route A would resolve after the user navigated to route B and
// overwrite B's state. The cleanup of useEffect should call
// `controller.abort()` on the controller it created; downstream
// callers should ignore the AbortError. Helper `isAbortError` keeps
// the swallow uniform.
export function isAbortError(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}

/// Typed error for any non-2xx daemon response. Carries the HTTP status
/// (and the parsed problem+json body when the daemon sent one) so callers
/// can tell a 400 (bad request / UI bug) from a 404 from a 5xx (daemon
/// fault) without string-matching `message` — which keeps the exact
/// pre-ApiError shapes, so existing `String(e)` rendering is unchanged.
/// A network-down failure stays a `TypeError` from fetch itself, so
/// `e instanceof ApiError` also separates "daemon answered with an error"
/// from "daemon unreachable".
export class ApiError extends Error {
  readonly status: number;
  readonly problem?: Problem;
  constructor(message: string, status: number, problem?: Problem) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.problem = problem;
  }
}

/// Uniform non-2xx thrower shared by every fetcher: prefers the
/// problem+json title/detail, falls back to `status statusText`;
/// `prefix` preserves a call site's historical operation tag (e.g.
/// "unpin failed") in the message.
async function throwApiError(r: Response, prefix?: string): Promise<never> {
  const ct = r.headers.get("content-type") ?? "";
  let detail: string | undefined;
  let problem: Problem | undefined;
  if (ct.includes("application/problem+json")) {
    try {
      problem = (await r.json()) as Problem;
      detail = `${problem.title}: ${problem.detail ?? ""}`;
    } catch {
      // Torn or empty problem body — fall through to the status line.
    }
  }
  const base = detail ?? `${r.status} ${r.statusText}`;
  throw new ApiError(prefix ? `${prefix}: ${base}` : base, r.status, problem);
}

async function get<T>(path: string, signal?: AbortSignal): Promise<T> {
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    headers: { Accept: "application/json" },
    signal,
  });
  if (!r.ok) await throwApiError(r);
  return (await r.json()) as T;
}

export const fetchIdentity = (signal?: AbortSignal) =>
  get<Identity>("/api/identity", signal);
export const fetchKbs = (signal?: AbortSignal) =>
  get<KbSummary[]>("/api/kbs", signal);

/// v0.34 W — GET /api/users: configured ∪ observed attribution usernames.
/// Config-static + slowly-growing observed set; no users.* SSE event.
export const fetchUsers = (signal?: AbortSignal) =>
  get<UsersResponse>("/api/users", signal);

// TM-track — GET /api/metrics snapshot. The coarse fields are always present
// (mirrored by the metrics.tick SSE); `detailed` is populated only when the
// daemon runs with `[server] metrics = true`. Mirrors
// crates/kb-server/src/routes/metrics.rs.
export type MetricsLatency = {
  label: string;
  count: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
};
export type MetricsStorageStat = {
  kind: string;
  count: number;
  handler_p50_ms: number;
  handler_p95_ms: number;
  handler_p99_ms: number;
  queue_wait_p50_ms: number;
  queue_wait_p95_ms: number;
  queue_wait_p99_ms: number;
};
export type MetricsPipeline = {
  enabled: boolean;
  indexer: { files_indexed: number; p50_ms: number; p95_ms: number; p99_ms: number };
  embed_index: {
    calls: number;
    docs: number;
    p50_ms: number;
    p95_ms: number;
    p99_ms: number;
  };
  storage: MetricsStorageStat[];
};
export type MetricsSnapshot = {
  requests_total: number;
  storage_channel_depth: number;
  storage_channel_capacity: number;
  detailed_enabled: boolean;
  detailed: null | {
    search_stages: MetricsLatency[];
    per_kb: MetricsLatency[];
    pipeline: MetricsPipeline;
  };
};
export const fetchMetricsSnapshot = (signal?: AbortSignal) =>
  get<MetricsSnapshot>("/api/metrics", signal);

// === Settings dashboard observability surface (S2-S3) ===
//
// Mirrors `crates/kb-server/src/routes/{stats,sources,search,errors}.rs`
// response shapes. Kept in client.ts (rather than per-tab modules) so
// the types stay in lockstep with the other API consumers.

export const fetchCrossStats = (signal?: AbortSignal) =>
  get<CrossStats>("/api/stats", signal);

export const fetchKbStats = (kb: string, signal?: AbortSignal) =>
  get<KbStats>(`/api/kb/${encodeURIComponent(kb)}/stats`, signal);

export type RunStatus = "running" | "complete";

export type RunEntry = {
  run: string;
  src: string | null;
  started_at: string;
  finished_at: string | null;
  ok_count: number;
  err_count: number;
  duration_ms: number | null;
  status: RunStatus;
};

export const fetchRuns = (kb: string, limit = 50, signal?: AbortSignal) =>
  get<RunEntry[]>(
    `/api/kb/${encodeURIComponent(kb)}/runs?limit=${limit}`,
    signal,
  );

export type QueryEntry = {
  at: string;
  q: string;
  mode: string;
  hits: number;
  ms: number;
};

export const fetchQueries = (kb: string, limit = 50, signal?: AbortSignal) =>
  get<QueryEntry[]>(
    `/api/kb/${encodeURIComponent(kb)}/queries?limit=${limit}`,
    signal,
  );

/// W1.search — GC-B3 zero-hit query groups (normalized query + count +
/// last-seen), backing the search page's "recent misses" retry row. The
/// ring resets per daemon boot (see ZeroHitGroup's doc comment).
export const fetchZeroHitQueries = (
  kb: string,
  limit = 8,
  signal?: AbortSignal,
) =>
  get<ZeroHitGroup[]>(
    `/api/kb/${encodeURIComponent(kb)}/queries?zero_hit=true&limit=${limit}`,
    signal,
  );

/// W1.pulse — the cross-kb twin of `fetchZeroHitQueries`: every kb's
/// zero-hit query groups in one fan-out response (invariant #28), backing
/// the census panel's fleet-wide list. Same volatile in-process ring, same
/// per-boot reset.
export const fetchZeroHitQueriesAll = (
  limit = 50,
  minCount = 1,
  signal?: AbortSignal,
) =>
  get<KbZeroHit[]>(
    `/api/queries/zero-hit?limit=${limit}&min_count=${minCount}`,
    signal,
  );

// === CT-F5 — corpus-health SLOs ===
//
// Hand-written wire types (not ts-rs generated) mirroring
// `kb_core::slo::{SloReport, SloIndicator}` and
// `kb_core::storage::sqlite::SloSnapshotRow`.
//
// SURFACED, NEVER ENFORCED — the SPA renders these and does nothing else
// with them: no badge on any other surface gates on a warn, no query is
// invalidated by one, nothing retries.
export type SloStatus = "ok" | "warn" | "unknown";

export type SloIndicator = {
  key: string;
  label: string;
  /// "percent" | "count" | "hours".
  unit: string;
  /// "higher_is_better" (target is a minimum) | "lower_is_better" (maximum).
  direction: string;
  /// `null` when the inputs genuinely aren't there. NEVER render this as 0 —
  /// an unmeasured indicator and a measured zero are different facts.
  value: number | null;
  /// `null` when `[kb.*.slo]` configured no target for this indicator.
  target: number | null;
  status: SloStatus;
  /// One honest sentence: the numerator/denominator, or why it's unknown.
  detail: string;
};

export type SloReport = {
  grammar: string;
  kb: string;
  computed_at_unix: number;
  indicators: SloIndicator[];
  warn_count: number;
};

export type SloSnapshotRow = {
  id: number;
  taken_at_unix: number;
  indicator: string;
  value: number | null;
  target: number | null;
  status: string;
};

/// GET /api/kb/{kb}/slo — the four indicators, computed now.
export const fetchSlo = (kb: string, signal?: AbortSignal) =>
  get<SloReport>(`/api/kb/${encodeURIComponent(kb)}/slo`, signal);

/// GET /api/kb/{kb}/slo/snapshots — newest-first page over the append-only log.
export const fetchSloSnapshots = (
  kb: string,
  limit = 100,
  signal?: AbortSignal,
) =>
  get<{ kb: string; rows: SloSnapshotRow[] }>(
    `/api/kb/${encodeURIComponent(kb)}/slo/snapshots?limit=${limit}`,
    signal,
  );

/// POST /api/kb/{kb}/slo/snapshot — append one reading to the log. Every run
/// lands (no skip-if-unchanged): a flat line is itself the signal.
export const snapshotSlo = (kb: string) =>
  mutate<{ taken_at_unix: number; appended: number; report: SloReport }>(
    `/api/kb/${encodeURIComponent(kb)}/slo/snapshot`,
    "POST",
  );

export const fetchErrors = (kb: string, signal?: AbortSignal) =>
  get<ErrorEntry[]>(`/api/kb/${encodeURIComponent(kb)}/errors`, signal);

// === Safe daemon actions (S4) ===
//
// Pause/resume sources, reindex a kb (whole or per-source), kick the
// atlas, dismiss/apply-fix an error. All 200/202 routes; the SPA
// reacts via the existing SSE event stream rather than polling for
// completion.

export const pauseSource = (kb: string, src: string) =>
  mutate<{ src: string; paused: boolean }>(
    `/api/kb/${encodeURIComponent(kb)}/sources/${encodeURIComponent(src)}/pause`,
    "POST",
  );

export const resumeSource = (kb: string, src: string) =>
  mutate<{ src: string; paused: boolean }>(
    `/api/kb/${encodeURIComponent(kb)}/sources/${encodeURIComponent(src)}/resume`,
    "POST",
  );

export type ReindexResponse = { run: string; events: string };

export const reindexKb = (kb: string) =>
  mutate<ReindexResponse>(`/api/kb/${encodeURIComponent(kb)}/reindex`, "POST");

export const reindexSource = (kb: string, src: string) =>
  mutate<ReindexResponse>(
    `/api/kb/${encodeURIComponent(kb)}/sources/${encodeURIComponent(src)}/reindex`,
    "POST",
  );

// `recomputeAtlas` and `reclusterAtlas` are already defined below
// (around line 485) — those existing wrappers handle the 202 +
// `{run}` shape correctly. Don't duplicate them here.

export const dismissError = (kb: string, id: string) =>
  mutate<{ id: string; dismissed: boolean }>(
    `/api/kb/${encodeURIComponent(kb)}/errors/${encodeURIComponent(id)}/dismiss`,
    "POST",
  );

export const applyFixError = (kb: string, id: string) =>
  mutate<{ run: string; events: string; note?: string }>(
    `/api/kb/${encodeURIComponent(kb)}/errors/${encodeURIComponent(id)}/apply-fix`,
    "POST",
  );

// === Quarantine ===
//
// Artifacts the indexer gave up on after QUARANTINE_THRESHOLD (3)
// consecutive failures. The daemon copies them aside to
// <state>/quarantine/<kb>/ and keeps the error rows pinned with
// retry_count >= 2. The Settings → Quarantine tab calls these to
// inspect + restore.

export type QuarantineEntry = {
  error_id: string;
  kind: string;
  source_slug: string;
  path: string;
  message: string;
  retry_count: number;
  created_at: number;
  sidecar_present: boolean;
};

export const fetchQuarantine = (kb: string, signal?: AbortSignal) =>
  get<QuarantineEntry[]>(`/api/kb/${encodeURIComponent(kb)}/quarantine`, signal);

export type QuarantineRestoreResult = {
  path: string;
  errors_cleared: number;
  sidecar_removed: boolean;
};

export const restoreQuarantine = (kb: string, path: string) =>
  mutate<QuarantineRestoreResult>(
    `/api/kb/${encodeURIComponent(kb)}/quarantine/restore`,
    "POST",
    { path },
  );

export const restoreAllQuarantine = (kb: string) =>
  mutate<{ kb: string; restored: QuarantineRestoreResult[] }>(
    `/api/kb/${encodeURIComponent(kb)}/quarantine/restore-all`,
    "POST",
  );

// === Per-file exclusion (v0.24 X4) ===
//
// Mirrors `crates/kb-server/src/routes/exclusions.rs`. Excluding pulls
// the artifact from search/gallery via a KeepUserData cascade (the
// `.review` sidecar + reading history survive; the file stays on
// disk); re-including nudges a reindex. The daemon confirms over SSE
// (`artifact.excluded`/`artifact.included` + the cascade's
// `artifact.removed`/`artifact.indexed`) — the bridge in queryClient.ts
// owns the invalidation, so callers just fire-and-toast.

export const fetchExclusions = (kb: string, signal?: AbortSignal) =>
  get<ExclusionEntry[]>(`/api/kb/${encodeURIComponent(kb)}/exclusions`, signal);

export const excludeArtifact = (kb: string, path: string, note?: string) =>
  mutate<ExcludeResponse>(`/api/kb/${encodeURIComponent(kb)}/exclusions`, "POST", {
    path,
    note: note ?? null,
  } satisfies ExcludeBody);

/// DELETE …/exclusions/{path} — `path` is ONE percent-encoded segment
/// (`/` travels as `%2F`), which encodeURIComponent produces exactly.
export const includeArtifact = (kb: string, path: string) =>
  mutate<IncludeResponse>(
    `/api/kb/${encodeURIComponent(kb)}/exclusions/${encodeURIComponent(path)}`,
    "DELETE",
  );

// F4 — filesystem move / folder rename. Daemon mutates source paths; SSE
// emits artifact.removed(old)+artifact.indexed(new) so the queryClient
// bridge refreshes docs/gallery/folders (no extra SPA invalidation).

/** One artifact after a path change (move doc or a row inside rename-folder). */
export type MovePathResult = {
  old_id: string;
  new_id: string;
  old_source_rel: string;
  new_source_rel: string;
};

export type MoveDocResponse = MovePathResult;

export type RenameFolderResponse = {
  moved: MovePathResult[];
};

/// POST …/docs/{id}/move — body `{ to: source-rel path }`. Id changes when
/// the path does (artifact id is path-derived).
export const moveDoc = (
  kb: string,
  id: string,
  to: string,
): Promise<MoveDocResponse> =>
  mutate<MoveDocResponse>(
    `/api/kb/${encodeURIComponent(kb)}/docs/${encodeURIComponent(id)}/move`,
    "POST",
    { to },
  );

/// POST …/folders/rename — body `{ from, to }` folder paths (source-rel,
/// no trailing slash). Moves every artifact under `from` whose new path
/// is under `to`.
export const renameFolder = (
  kb: string,
  from: string,
  to: string,
): Promise<RenameFolderResponse> =>
  mutate<RenameFolderResponse>(
    `/api/kb/${encodeURIComponent(kb)}/folders/rename`,
    "POST",
    { from, to },
  );

// v0.9 M7 — agent-memory recall. One ranked, cross-corpus list; the
// /api/memory/recall endpoint owns scope resolution + ranking, so the
// SPA makes a single call instead of aggregating per-corpus client-side.
export type MemoryScope = "all" | "global" | "project";

// === v0.10 M2/M3 — memory pin + daemon-wide decay policy ===========
export type DecayPolicy = "strict" | "balanced" | "loose";

export const pinMemory = (
  kb: string,
  artifactId: string,
): Promise<MemoryPinResponse> =>
  mutate<MemoryPinResponse>(
    `/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(artifactId)}/pin`,
    "POST",
  );

export async function unpinMemory(kb: string, artifactId: string): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(artifactId)}/pin`,
    { method: "DELETE", headers: { "X-Requested-By": "kb-spa" } },
  );
  if (!r.ok && r.status !== 404) await throwApiError(r, "unpin failed");
}

/// MI-W4.1 — `drop_threshold` is the ACTIVE policy's salience floor
/// (`null` for `loose`, which never drops anything), added so the
/// health-timeline sparkline's reference line never has to duplicate the
/// strict/balanced/loose → threshold mapping client-side.
export type MemoryPolicyOut = { policy: DecayPolicy; drop_threshold?: number | null };

export const fetchMemoryPolicy = (signal?: AbortSignal) =>
  get<MemoryPolicyOut>("/api/memory/policy", signal);

/// MI-W3.2b — PATCH …/memories/{id}/salience { salience }: the ONE
/// mutable memory meta via the API (everything else is immutable — see
/// `updateArtifactMeta`'s tags/category scope, which deliberately never
/// touches memory metas). `salience` is clamped server-side to `[0,1]`.
export const patchMemorySalience = (
  kb: string,
  artifactId: string,
  salience: number,
): Promise<MemorySalienceResponse> =>
  mutate<MemorySalienceResponse>(
    `/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(artifactId)}/salience`,
    "PATCH",
    { salience },
  );

/// v0.13 D7 — POST /api/kb/{src}/memories/{id}/promote with
/// `{dest_kb}`: copy a memory artifact into a non-memory kb,
/// stripping the kb-salience / kb-decay / kb-pinned / kb-supersedes
/// metas. Returns the new artifact's id + dest path + dest kb.
export type PromoteResponse = MemoryPromoteResponse;

export const promoteMemory = (
  srcKb: string,
  artifactId: string,
  destKb: string,
): Promise<PromoteResponse> =>
  mutate<PromoteResponse>(
    `/api/kb/${encodeURIComponent(srcKb)}/memories/${encodeURIComponent(artifactId)}/promote`,
    "POST",
    { dest_kb: destKb },
  );

// v0.13 Q4 — daemon-side saved-query store. The SPA hook still uses
// localStorage as the offline-first cache; when the daemon is
// reachable it overlays the canonical list and syncs writes.

export const fetchSavedQueries = (signal?: AbortSignal) =>
  get<SavedQueriesList>("/api/saved-queries", signal);

export const upsertSavedQuery = (
  name: string,
  path: string,
  search: string,
): Promise<SavedQueriesList> =>
  mutate<SavedQueriesList>("/api/saved-queries", "POST", {
    name,
    path,
    search,
  });

export async function deleteSavedQuery(name: string): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/saved-queries/${encodeURIComponent(name)}`,
    { method: "DELETE", headers: { "X-Requested-By": "kb-spa" } },
  );
  if (!r.ok && r.status !== 404) await throwApiError(r, "delete saved query");
}

/// PUT /api/memory/policy — flip the daemon-wide decay policy.
/// Routes around the shared `mutate` helper (POST/PATCH/DELETE only).
export async function setMemoryPolicy(policy: DecayPolicy): Promise<MemoryPolicyOut> {
  const r = await fetch(`${currentDaemonBase()}/api/memory/policy`, {
    method: "PUT",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({ policy }),
  });
  if (!r.ok) await throwApiError(r, "policy update failed");
  return (await r.json()) as MemoryPolicyOut;
}

export function fetchRecall(
  opts: {
    q?: string;
    scope?: MemoryScope;
    project?: string;
    limit?: number;
    /// L9 — when set, only return memories whose V0010 link set
    /// contains `*` (global) or this kb. Drives the per-kb /memory
    /// view filter and the PreviewInspector rail.
    forKb?: string;
    /// MI-W4.2a — opt-in per-week injection histogram
    /// (`RecallHit.recall_weekly`). The `/memory` row sparkline's only
    /// consumer; every other caller leaves this off so the hot per-turn
    /// `kb-recall` hook path never pays for the extra ledger fan-out.
    withWeekly?: boolean;
  },
  signal?: AbortSignal,
): Promise<RecallResponse> {
  const p = new URLSearchParams();
  if (opts.q) p.set("q", opts.q);
  p.set("scope", opts.scope ?? "all");
  if (opts.project) p.set("project", opts.project);
  p.set("limit", String(opts.limit ?? 50));
  if (opts.forKb) p.set("for_kb", opts.forKb);
  if (opts.withWeekly) p.set("with_weekly", "true");
  return get<RecallResponse>(`/api/memory/recall?${p.toString()}`, signal);
}

// === CT-A1 (U3 parse-back) — reverse provenance =========================

/// `GET /api/kb/{kb}/docs/{id}/memories-from` — every memory highlighted
/// FROM artifact `{kb}/{id}` (the reverse of `MemoryProvenance`).
export function fetchMemoriesFrom(
  kb: string,
  id: string,
  signal?: AbortSignal,
): Promise<MemoriesFromResponse> {
  return get<MemoriesFromResponse>(
    `/api/kb/${encodeURIComponent(kb)}/docs/${encodeURIComponent(id)}/memories-from`,
    signal,
  );
}

// === MI-W4.3 — lineage viewer ===========================================

export type { MemoryLineageNode, MemoryLineageResponse };

/// MI-W4.3 — `GET /api/kb/{kb}/memories/{id}/lineage`: one supersede
/// chain's nodes, both directions, `kb memory log`'s own data source.
export const fetchMemoryLineage = (
  kb: string,
  id: string,
  signal?: AbortSignal,
): Promise<MemoryLineageResponse> =>
  get<MemoryLineageResponse>(
    `/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(id)}/lineage`,
    signal,
  );

// === CT-B2 — recalled-by ================================================
//
// Re-exported from `./generated/*` (the hand-typed interim shapes were
// swapped out once the ts-export bindings were regenerated, per the note
// that used to live here — CT-C5).

import type { MemoryRecalledByRow } from "./generated/MemoryRecalledByRow";
import type { MemoryRecalledByResponse } from "./generated/MemoryRecalledByResponse";

export type { MemoryRecalledByRow, MemoryRecalledByResponse };

/// `GET /api/kb/{kb}/memories/{id}/recalled-by` — every session that
/// recalled this memory, fanned out across the daemon (invariant #28; the
/// ledger lives with the RECALLING session's kb, not necessarily `kb`).
export const fetchMemoryRecalledBy = (
  kb: string,
  id: string,
  signal?: AbortSignal,
): Promise<MemoryRecalledByResponse> =>
  get<MemoryRecalledByResponse>(
    `/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(id)}/recalled-by`,
    signal,
  );

// === CT-F1 — committed-in (the memory↔commit exact-id join) =============
//
// Re-exported from `./generated/*` — the hand-typed interim shapes were
// swapped out once the ts-rs bindings were regenerated, exactly as the
// comment that used to live here instructed (and as CT-B2's `recalled-by`
// types did before them). The Rust doc comments ride along in the
// generated files, so the field semantics (EXACT-ID trust, `recorded_at`
// is the capture clock not the commit date, `repo_root` because a bare
// sha is meaningless across repos) are documented at the source.

import type { MemoryCommittedInRow } from "./generated/MemoryCommittedInRow";
import type { MemoryCommittedInResponse } from "./generated/MemoryCommittedInResponse";

export type { MemoryCommittedInRow, MemoryCommittedInResponse };

/// `GET /api/kb/{kb}/memories/{id}/commits` — CT-F1. An EMPTY list is a
/// non-signal: the `Kb-Memory:` trailer is opt-in per repo and off by
/// default, so "no rows" almost always means "that repo never opted in".
/// Every caller must label it that way.
export const fetchMemoryCommittedIn = (
  kb: string,
  id: string,
  signal?: AbortSignal,
): Promise<MemoryCommittedInResponse> =>
  get<MemoryCommittedInResponse>(
    `/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(id)}/commits`,
    signal,
  );

// === MI-W4.4 — hygiene triage queue =====================================

export type { MemoryTriageItem, MemoryTriageResponse };

/// MI-W4.4 — `GET /api/memory/triage[?kb=][&limit=]`: the bounded, DERIVED
/// hygiene queue. Never mutates anything.
export function fetchMemoryTriage(
  opts: { kb?: string; limit?: number } = {},
  signal?: AbortSignal,
): Promise<MemoryTriageResponse> {
  const p = new URLSearchParams();
  if (opts.kb) p.set("kb", opts.kb);
  if (opts.limit) p.set("limit", String(opts.limit));
  const qs = p.toString();
  return get<MemoryTriageResponse>(`/api/memory/triage${qs ? `?${qs}` : ""}`, signal);
}

// === L6/L9 — memory ↔ kb link mutations ============================

/// PUT /api/kb/{kb}/memories/{id}/links — atomic replace of the entire
/// link set. Pass `{ global: true, linked_kbs: [] }` to revert to "visible
/// everywhere"; `{ global: false, linked_kbs: ["foo", "bar"] }` to scope.
export async function setMemoryLinks(
  kb: string,
  artifactId: string,
  body: MemoryLinks,
): Promise<MemoryLinks> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(artifactId)}/links`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(body),
    },
  );
  if (!r.ok) await throwApiError(r, "set links failed");
  return (await r.json()) as MemoryLinks;
}

/// POST /api/kb/{kb}/memories/{id}/links/{target_kb} — add one edge.
/// Idempotent. Used by the inline "Pin to this kb" quick action.
export async function addMemoryLink(
  kb: string,
  artifactId: string,
  targetKb: string,
): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(artifactId)}/links/${encodeURIComponent(targetKb)}`,
    { method: "POST", headers: { "X-Requested-By": "kb-spa" } },
  );
  if (!r.ok) await throwApiError(r, "add link failed");
}

/// DELETE /api/kb/{kb}/memories/{id}/links/{target_kb} — remove one
/// edge. 204; idempotent (also OK when the link didn't exist).
export async function removeMemoryLink(
  kb: string,
  artifactId: string,
  targetKb: string,
): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/memories/${encodeURIComponent(artifactId)}/links/${encodeURIComponent(targetKb)}`,
    { method: "DELETE", headers: { "X-Requested-By": "kb-spa" } },
  );
  if (!r.ok && r.status !== 404) await throwApiError(r, "remove link failed");
}

/// M7 — forget a memory (DELETE the artifact file). 204 on success; a
/// 404 (already gone) is treated as success so double-clicks are benign.
export async function forgetMemory(kb: string, id: string): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/artifacts/${encodeURIComponent(id)}`,
    { method: "DELETE", headers: { "X-Requested-By": "kb-spa" } },
  );
  if (!r.ok && r.status !== 404) await throwApiError(r, "forget failed");
}

// === U3 — highlight → save as memory ================================
//
// The SPA's ONE memory-write path, and it is not a new one: it POSTs the
// same `/api/kb/{kb}/artifacts` body `kb remember` has always posted (see
// `crates/kb-server/src/routes/artifacts.rs`), plus four additive,
// `#[serde(default)]` provenance fields. No new store, no new table, no
// second write path — a memory kept from a highlight is indistinguishable
// from an agent-remembered one except that it says where it came from.
//
// Three things this deliberately does NOT do:
//   · no salience/decay override — ruling 5: "human memories are more
//     important" is a score term wearing a hat. The server default stands.
//   · no summarisation — the memory text is the SELECTION, verbatim
//     (edited by the operator if they choose). kb runs no model, client
//     side or daemon side.
//   · no identity — `author` is the `you | claude` ROLE split, the same
//     two values a comment carries.

/// Where a memory came from. Every field optional + omitted when unset,
/// so this type also describes "an ordinary remember" (all absent).
export type MemoryProvenance = {
  /// The human↔agent ROLE, not an identity.
  author?: "you" | "claude";
  /// kb + artifact id of the source artifact.
  source_kb?: string;
  source_artifact?: string;
  /// The selection the text was lifted from — a `review::Anchor`, the
  /// SAME shape highlights/comments/list entries use (invariant #25).
  source_anchor?: Anchor;
};

export type RememberBody = MemoryProvenance & {
  title: string;
  /// Plain text; the daemon escapes + wraps it into paragraphs. Never
  /// pre-rendered HTML from the SPA.
  body: string;
  category?: string;
  tags?: string[];
  summary?: string;
};

/// The ingest route's response: the path-derived id the indexer will
/// assign, and the source-root-relative path written.
export type RememberResult = { id: string; path: string };

/// POST /api/kb/{kb}/artifacts — write a memory. 201. The daemon emits
/// `memory.ingested`, which the SSE bridge already invalidates the recall
/// lanes on (invariant #23) — callers need no manual cache poke.
export const rememberMemory = (
  kb: string,
  body: RememberBody,
): Promise<RememberResult> =>
  mutate<RememberResult>(`/api/kb/${encodeURIComponent(kb)}/artifacts`, "POST", body);

/// Which corpus a hand-written memory lands in. Mirrors the CLI's
/// `resolve_memory_kb` (crates/kb-cli/src/commands/memory.rs) so the two
/// entry points never disagree: the single `memory_scope === "project"`
/// corpus, else the single `"global"` one. Ambiguity (two project corpora)
/// resolves to `null` rather than guessing — the CLI errors out in the
/// same case, and a memory written to the wrong corpus is worse than one
/// not written. Pure; unit-tested.
export function pickMemoryKb(
  kbs: readonly KbSummary[] | undefined,
): string | null {
  for (const want of ["project", "global"] as const) {
    const matches = (kbs ?? []).filter((k) => k.memory_scope === want);
    if (matches.length === 1) return matches[0].name;
  }
  return null;
}

/// A memory needs a title; a highlight only has text. Same rule as the
/// CLI's `derive_title` (crates/kb-cli/src/commands/memory.rs), kept in
/// lock-step with it: first non-empty line, trimmed, hard-capped at 80
/// code points, re-trimmed, `"memory"` when nothing survives. It is
/// deterministic string handling, NOT a summary — no model generates a
/// title here, client-side or daemon-side.
export function deriveMemoryTitle(text: string): string {
  const first = text
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  const capped = Array.from(first ?? "")
    .slice(0, 80)
    .join("")
    .trim();
  return capped || "memory";
}

// v0.10 K2 — anchor corkboard. The SPA calls these "anchors" but the
// internal kb-core module + sqlite table use `corkboard` to avoid
// colliding with the stale-comment-anchor sidecar (`/api/anchors/stale`
// below). External shape mirrors `kb_core::corkboard::Entry`.

export const fetchAnchors = (signal?: AbortSignal) =>
  get<CorkboardResponse>("/api/anchors", signal);

/// POST /api/kb/{kb}/anchors/{artifact_id} — pin. Idempotent; the
/// `added` flag tells you if it was a fresh row.
export const pinAnchor = (
  kb: string,
  artifactId: string,
): Promise<AnchorPinResponse> =>
  mutate<AnchorPinResponse>(
    `/api/kb/${encodeURIComponent(kb)}/anchors/${encodeURIComponent(artifactId)}`,
    "POST",
  );

/// DELETE /api/kb/{kb}/anchors/{artifact_id} — unpin. 204; idempotent.
export async function unpinAnchor(kb: string, artifactId: string): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/anchors/${encodeURIComponent(artifactId)}`,
    { method: "DELETE", headers: { "X-Requested-By": "kb-spa" } },
  );
  if (!r.ok && r.status !== 404) await throwApiError(r, "unpin failed");
}

// === RL-track (v0.18) — reading lists ======================================

/// PATCH bodies stay hand-written: their tri-state fields (absent = keep,
/// null = clear, value = set) can't ride ts-rs (no nested-Option TS shape).
/// The daemon's double_option deserializer is the wire counterpart.
export type ListPatchBody = {
  title?: string;
  description?: string | null;
  pinned?: boolean;
  archived?: boolean;
};
export type ListEntryPatchBody = {
  note?: string | null;
  anchor?: Anchor | null;
  /// "clear" drops the override (back to the derived state).
  read_override?: "read" | "unread" | "clear";
  before?: string;
  after?: string;
  position?: number;
};

/// Cross-kb index. Always asks for archived too — the SPA partitions
/// client-side (collapsed Archived section) so one cache entry serves
/// every consumer.
export const fetchLists = (signal?: AbortSignal) =>
  get<ListIndexResponse>("/api/lists?include_archived=true", signal);

export const fetchList = (kb: string, id: string, signal?: AbortSignal) =>
  get<ListDetailResponse>(
    `/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(id)}`,
    signal,
  );

export const createList = (
  kb: string,
  body: ListCreateBody,
): Promise<ListSummary> =>
  mutate<ListSummary>(`/api/kb/${encodeURIComponent(kb)}/lists`, "POST", body);

export const patchList = (
  kb: string,
  id: string,
  body: ListPatchBody,
): Promise<ListSummary> =>
  mutate<ListSummary>(
    `/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(id)}`,
    "PATCH",
    body,
  );

/// DELETE …/lists/{id} — 204; idempotent (404-tolerant like removeBookmark
/// before it, so double-clicks don't surface errors).
export async function deleteList(kb: string, id: string): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  );
  if (!r.ok && r.status !== 404) await throwApiError(r, "deleteList failed");
}

export const addListEntry = (
  kb: string,
  id: string,
  body: ListEntryCreateBody,
): Promise<ListEntry> =>
  mutate<ListEntry>(
    `/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(id)}/entries`,
    "POST",
    body,
  );

export const patchListEntry = (
  kb: string,
  id: string,
  eid: string,
  body: ListEntryPatchBody,
): Promise<ListEntry> =>
  mutate<ListEntry>(
    `/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(id)}/entries/${encodeURIComponent(eid)}`,
    "PATCH",
    body,
  );

export async function removeListEntry(
  kb: string,
  id: string,
  eid: string,
): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(id)}/entries/${encodeURIComponent(eid)}`,
    { method: "DELETE" },
  );
  if (!r.ok && r.status !== 404)
    await throwApiError(r, "removeListEntry failed");
}

/// v0.33 Y3 — drop every tombstoned entry from the list. Returns the
/// count removed; a single `list.updated` SSE follows (TanStack bridge
/// refreshes — no manual refetch at call sites).
export type PruneListResponse = { removed: number };
export const pruneList = (
  kb: string,
  id: string,
): Promise<PruneListResponse> =>
  mutate<PruneListResponse>(
    `/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(id)}/prune`,
    "POST",
  );

/// Track U lookup — resolve a user-supplied path / unique filename /
/// 12-hex id to an artifact. Always 200; callers branch on `kind`.
export type LookupHit = { id: string; source_relative: string; title?: string | null };
export type LookupResponse =
  | { kind: "exact"; id: string }
  | { kind: "unique_suffix"; id: string }
  | { kind: "ambiguous"; candidates: LookupHit[]; truncated?: boolean }
  | { kind: "not_found" };

export const lookupArtifact = (
  kb: string,
  q: string,
  signal?: AbortSignal,
): Promise<LookupResponse> =>
  get<LookupResponse>(
    `/api/kb/${encodeURIComponent(kb)}/lookup?q=${encodeURIComponent(q)}`,
    signal,
  );

/// Download href for the export links (plain <a download> targets).
export const listExportUrl = (
  kb: string,
  id: string,
  format: "md" | "json",
): string =>
  `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(id)}/export?format=${format}`;

import type { CanvasDoc } from "../lib/canvas";

// === W2.4 — Boards v1 ======================================================
//
// `GET`/`PUT /api/kb/{kb}/boards/{list_id}/canvas` — a JSON Canvas
// geometry sidecar, one per reading list. No `#[derive(TS)]` binding on
// the daemon side (`routes::boards`'s doc explains why: the response
// body IS the open-ended JSON Canvas document, "the format IS the
// interop"), so `CanvasDoc` — defined in `lib/canvas.ts`, not here — is
// a hand-written type, not a generated one. `get`/`mutate` (this file's
// existing fetch helpers) work unmodified: the daemon stores whatever
// parses, and a `CanvasDoc` JS object round-trips through
// `JSON.stringify` exactly like any other `mutate` body.

export const fetchBoardCanvas = (
  kb: string,
  listId: string,
  signal?: AbortSignal,
): Promise<CanvasDoc> =>
  get<CanvasDoc>(
    `/api/kb/${encodeURIComponent(kb)}/boards/${encodeURIComponent(listId)}/canvas`,
    signal,
  );

export const putBoardCanvas = (
  kb: string,
  listId: string,
  canvas: CanvasDoc,
): Promise<CanvasDoc> =>
  mutate<CanvasDoc>(
    `/api/kb/${encodeURIComponent(kb)}/boards/${encodeURIComponent(listId)}/canvas`,
    "PUT",
    canvas,
  );

// Q1 — cold-load shape for the stale-anchors dashboard. Persisted
// `(kb, artifact_id, comment_id)` triples from every kb's
// `.anchors-stale.json` sidecar.
//
// #4 — `anchor_kind` + `fuzzy_score` come from the v3 sidecar metadata
// captured at the most recent stale transition. `first_seen` is still
// session-only (the daemon doesn't persist a wall-clock timestamp);
// the hook stamps `Date.now()` on cold-load entries.
export const fetchStaleAnchors = (signal?: AbortSignal) =>
  get<StaleAnchorsResponse>("/api/anchors/stale", signal);
export const fetchSources = (kb: string, signal?: AbortSignal) =>
  get<SourceSummary[]>(`/api/kb/${encodeURIComponent(kb)}/sources`, signal);
export const fetchDocs = (
  kb: string,
  opts: { limit?: number; includeAtlas?: boolean; signal?: AbortSignal } = {},
) => {
  const params = new URLSearchParams({
    limit: String(opts.limit ?? 10000),
  });
  if (opts.includeAtlas) params.set("include", "atlas");
  return get<DocSummary[]>(
    `/api/kb/${encodeURIComponent(kb)}/docs?${params.toString()}`,
    opts.signal,
  );
};

// S-milestone S2 — paginated envelope from /api/kb/<kb>/docs. The
// server returns this shape whenever the client passes `offset=`
// (any value, including 0). See routes/docs.rs.
export type DocsPage = {
  docs: DocSummary[];
  total: number;
  offset: number;
  limit: number;
  has_more: boolean;
  /// v0.10 Q2 — route timing in milliseconds. The QueryRibbon's
  /// "… in N ms" counter reads this.
  ms?: number;
  /// v0.10 Q2 — diagnostic warnings from the query DSL parser (unknown
  /// keys, OR collapses, NOT-on-fields-we-can't-exclude). Absent when
  /// no `q=` was sent or the parse was clean.
  query_warnings?: string[];
};

export type DocsQueryParams = {
  folder?: string;
  /// v0.33 Y1 — exact folder only (`folder_exact=1`). Only meaningful with
  /// `folder`; absent = descendant-inclusive (today's default).
  folderExact?: boolean;
  /// Any-of: tag slugs (csv on the wire).
  tags?: string[];
  /// All-of: capability flags (csv on the wire).
  caps?: string[];
  /// "7d" | "30d" | "all". Anything else (or absent) = no time filter.
  since?: string;
  /// v0.22 — positive `kb-category` include (exact match).
  category?: string;
  /// v0.22 — absolute `mtime_unix` window bounds (unix seconds, inclusive,
  /// either open-ended). Powers the reader's "modified around this time" pivot.
  from?: number;
  to?: number;
  /// W1.A — csv read-state facet (any-of): `never-opened|unread|
  /// in_progress|read`. A route-side set-membership filter over the
  /// per-kb reading rollup (not a `?q=` DSL atom).
  read?: string[];
  /// W2.3a — csv id-set membership filter (the atlas lasso / working-set
  /// gallery pivot). Route-injected server-side into `DocsQuery.ids`,
  /// gated across every OR branch (invariant #35).
  ids?: string[];
  indexOnly?: boolean;
  sort?: "recent" | "indexed" | "created" | "title" | "words";
  dir?: "asc" | "desc";
  /// S7 — `folder` prepends folder ASC to the primary sort.
  group?: "none" | "folder";
  offset: number; // required — its presence is the envelope-mode opt-in
  limit?: number;
  projection?: "slim" | "default" | "atlas";
  /// v0.10 Q2 — structured query DSL. Parsed server-side; the response
  /// surfaces `query_warnings` for any constructs the linearised
  /// pipeline can't honor (OR alternatives, NOT on most fields, …).
  q?: string;
  signal?: AbortSignal;
};

export const fetchDocsPage = (
  kb: string,
  q: DocsQueryParams,
): Promise<DocsPage> => {
  const params = new URLSearchParams({ offset: String(q.offset) });
  if (q.limit != null) params.set("limit", String(q.limit));
  if (q.folder) params.set("folder", q.folder);
  if (q.folderExact && q.folder) params.set("folder_exact", "1");
  if (q.tags && q.tags.length > 0) params.set("tags", q.tags.join(","));
  if (q.caps && q.caps.length > 0) params.set("caps", q.caps.join(","));
  if (q.since) params.set("since", q.since);
  if (q.category) params.set("category", q.category);
  if (q.from != null) params.set("from", String(Math.floor(q.from)));
  if (q.to != null) params.set("to", String(Math.floor(q.to)));
  if (q.read && q.read.length > 0) params.set("read", q.read.join(","));
  if (q.ids && q.ids.length > 0) params.set("ids", q.ids.join(","));
  if (q.indexOnly) params.set("index", "1");
  if (q.sort) params.set("sort", q.sort);
  if (q.dir) params.set("dir", q.dir);
  if (q.group && q.group !== "none") params.set("group", q.group);
  if (q.projection) params.set("projection", q.projection);
  if (q.q && q.q.trim().length > 0) params.set("q", q.q);
  return get<DocsPage>(
    `/api/kb/${encodeURIComponent(kb)}/docs?${params.toString()}`,
    q.signal,
  );
};
export const fetchDoc = (kb: string, id: string, signal?: AbortSignal) =>
  get<DocSummary>(
    `/api/kb/${encodeURIComponent(kb)}/docs/${encodeURIComponent(id)}`,
    signal,
  );
// Track U — resolve a doc by its source-relative path (the splat of the
// `/a/<kb>/<path>` permalink). Encodes per segment so the slashes stay
// literal path separators on the wire (matching the `{*path}` route).
export const fetchDocByPath = (
  kb: string,
  relPath: string,
  signal?: AbortSignal,
) => {
  const encRel = relPath
    .split("/")
    .map(encodeURIComponent)
    .join("/");
  return get<DocSummary>(
    `/api/kb/${encodeURIComponent(kb)}/docs/by-path/${encRel}`,
    signal,
  );
};
export type { PromptResponse };
/// W2.11 — the artifact's stored generation prompt (the `<template
/// id="kb-prompt">` bundle, 8 KiB-capped at index time). LOCAL-RENDER
/// ONLY: `stripped: true` means this (non-loopback) daemon withheld it —
/// never surfaced as an error, `prompt` is simply `null`. Query key
/// `["prompt", kb, id]` (see queryClient.ts) — no bridge invalidation,
/// prompts only change on reindex and `artifact.indexed` already
/// invalidates broadly.
export const fetchPrompt = (kb: string, id: string, signal?: AbortSignal) =>
  get<PromptResponse>(
    `/api/kb/${encodeURIComponent(kb)}/artifacts/${encodeURIComponent(id)}/prompt`,
    signal,
  );
export type { SessionReplayResponse, ReplayBeatOut, ReplayBeat, ReplayKind };
/// W3.R-c — one captured session's `session-replay/1` timeline
/// (`GET /api/sessions/{session_id}/replay`). FLEET-WIDE by session id, not
/// per-kb: the daemon fans out to locate the NEWEST capture of that session
/// (invariant #11) and reports the owning kb back in the response.
///
/// `artifact` narrows the window to beats that resolved to one artifact id;
/// `limit` caps the returned beats AFTER that filter. Both are windows onto
/// the same server-side timeline — `matched` (pre-limit) and `total_beats`
/// (pre-filter) come back so the UI can say the window IS a window.
///
/// The response also carries `scrubbed` / `redactions` (mirroring the
/// `x-kb-session-scrubbed` / `x-kb-redactions` headers): a non-loopback
/// client always gets at least the `secrets` redaction layer (#4), and the
/// reader MUST say so rather than presenting redacted text as verbatim.
///
/// Query key `["sessions", "replay", sessionId, artifactId]` — see
/// queryClient.ts.
export const fetchSessionReplay = (
  sessionId: string,
  opts: { artifact?: string; limit?: number } = {},
  signal?: AbortSignal,
) => {
  const p = new URLSearchParams();
  if (opts.artifact) p.set("artifact", opts.artifact);
  if (opts.limit != null) p.set("limit", String(opts.limit));
  const qs = p.toString();
  return get<SessionReplayResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/replay${qs ? `?${qs}` : ""}`,
    signal,
  );
};
export const fetchTags = (kb: string, signal?: AbortSignal) =>
  get<TagSummary[]>(`/api/kb/${encodeURIComponent(kb)}/tags`, signal);
export const fetchFolders = (kb: string, signal?: AbortSignal) =>
  get<FoldersResponse>(`/api/kb/${encodeURIComponent(kb)}/folders`, signal);
/// v0.22 — distinct kb-category / kb-status / kb-severity values + counts.
/// Backs the gallery left-rail category control + the reader's "Explore
/// from here" facet chips. One full-corpus scan server-side (uncached).
export const fetchFacets = (kb: string, signal?: AbortSignal) =>
  get<FacetsResponse>(`/api/kb/${encodeURIComponent(kb)}/facets`, signal);
export type { ResurfaceItem, ResurfaceResponse };
/// Resurface — deterministic pull-only queue (open comments + unfinished
/// reads); the gallery strip shows the top 2 of these items.
export const fetchResurface = (kb: string, signal?: AbortSignal) =>
  get<ResurfaceResponse>(`/api/kb/${encodeURIComponent(kb)}/resurface`, signal);

import type { EchoKind } from "./generated/EchoKind";
import type { EchoOut } from "./generated/EchoOut";
import type { EchoesResponse } from "./generated/EchoesResponse";
export type { EchoKind, EchoOut, EchoesResponse };
/// CT-E6 — the beliefs lane. Typed locally rather than in
/// `api/generated/` (ts-rs output the orchestrator regenerates from
/// `routes::echoes::{BeliefStatus, BeliefOut}` — not hand-edited here);
/// intersected onto the generated `EchoesResponse` below so a stale
/// regen still typechecks (both new fields are additive).
export type BeliefStatus = "active" | "superseded" | "forgotten";
export type BeliefOut = {
  id: string;
  kb: string;
  title: string;
  created: number;
  months_ago: number;
  status: BeliefStatus;
  superseded_by?: string;
  superseded_at?: number;
};
export type EchoesResponseWithBeliefs = EchoesResponse & {
  beliefs: BeliefOut[];
  beliefs_tombstone_caveat?: string;
};
/// W2.2 — on-this-day + session echoes: a deterministic date-join (created /
/// read / worked-on vs 1-3yr + 6mo anniversaries), sibling to resurface — a
/// separate endpoint/wire so its own date-proximity ordering never rides
/// through resurface's golden-pinned comment/read term contract. The
/// gallery's EchoStrip shows the top 2 of these items (plus, CT-E6, the
/// beliefs lane below them).
export const fetchEchoes = (kb: string, signal?: AbortSignal) =>
  get<EchoesResponseWithBeliefs>(`/api/kb/${encodeURIComponent(kb)}/echoes`, signal);

// W2.15b — the tribal-knowledge proposal inbox. A post-session memory
// CANDIDATE (agent-authored, `kb propose`) sits in a per-kb `.proposals/`
// queue until a human approves (writes the memory via the EXACT `kb
// remember` path) or rejects (discards) it — the human gate the daemon's
// no-in-daemon-LLM invariant requires. `fetchProposals` is fleet-wide
// (`?kb=` narrows); approve/reject are per-kb mutations.
import type { ProposalSource } from "./generated/ProposalSource";
import type { Proposal } from "./generated/Proposal";
import type { ProposalItem } from "./generated/ProposalItem";
import type { ProposalsResponse } from "./generated/ProposalsResponse";
import type { ApproveResponse } from "./generated/ApproveResponse";
export type { ProposalSource, Proposal, ProposalItem, ProposalsResponse, ApproveResponse };

export const fetchProposals = (
  opts: { kb?: string } = {},
  signal?: AbortSignal,
) => {
  const params = new URLSearchParams();
  if (opts.kb) params.set("kb", opts.kb);
  const qs = params.toString();
  return get<ProposalsResponse>(`/api/proposals${qs ? `?${qs}` : ""}`, signal);
};

export const approveProposal = (kb: string, id: string) =>
  mutate<ApproveResponse>(
    `/api/kb/${encodeURIComponent(kb)}/proposals/${encodeURIComponent(id)}/approve`,
    "POST",
  );

export async function rejectProposal(kb: string, id: string): Promise<void> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/proposals/${encodeURIComponent(id)}/reject`,
    { method: "POST", headers: { "X-Requested-By": "kb-spa" } },
  );
  if (!r.ok) await throwApiError(r, "reject failed");
}

import type { HistoryCalendarResponse } from "./generated/HistoryCalendarResponse";
import type { CalendarDay } from "./generated/CalendarDay";
export type { HistoryCalendarResponse, CalendarDay };
/// W2.10 — per-UTC-day event density for the gallery's activity calendar
/// (`{ days: [{ day, opens, searches, comments }] }`). `from`/`to` are unix
/// seconds, both optional — the server defaults to a trailing 365-day
/// window ending now, and caps the requested span at ~400 days.
export const fetchCalendar = (
  kb: string,
  opts: { from?: number; to?: number } = {},
  signal?: AbortSignal,
): Promise<HistoryCalendarResponse> => {
  const params = new URLSearchParams();
  if (opts.from != null) params.set("from", String(Math.floor(opts.from)));
  if (opts.to != null) params.set("to", String(Math.floor(opts.to)));
  const qs = params.toString();
  return get<HistoryCalendarResponse>(
    `/api/kb/${encodeURIComponent(kb)}/history/calendar${qs ? `?${qs}` : ""}`,
    signal,
  );
};

import type { TimelineResponse } from "./generated/TimelineResponse";
import type { TimelineLane } from "./generated/TimelineLane";
import type { TimelineTrack } from "./generated/TimelineTrack";
export type { TimelineResponse, TimelineLane, TimelineTrack };
/// W3.C-b — the reflection canvas's four UTC-day-bucketed lanes (created ·
/// read · session · comment) over ONE shared axis, each carrying the
/// RESOLVED artifact-id set for the requested window plus an honest
/// `truncated` flag (`routes/timeline.rs`). `from`/`to` are unix seconds,
/// both optional — the server defaults to a trailing 365-day window ending
/// now and caps the requested span at ~400 days, exactly like
/// `fetchCalendar` above (the two endpoints share the UTC-day contract).
///
/// The ids are resolved per REQUEST WINDOW, not per day, so a brushed
/// sub-window re-asks this route with the brush's bounds — see
/// `ReflectionCanvas.tsx` (the brush itself never refetches).
export const fetchTimeline = (
  kb: string,
  opts: { from?: number; to?: number } = {},
  signal?: AbortSignal,
): Promise<TimelineResponse> => {
  const params = new URLSearchParams();
  if (opts.from != null) params.set("from", String(Math.floor(opts.from)));
  if (opts.to != null) params.set("to", String(Math.floor(opts.to)));
  const qs = params.toString();
  return get<TimelineResponse>(
    `/api/kb/${encodeURIComponent(kb)}/timeline${qs ? `?${qs}` : ""}`,
    signal,
  );
};

import type { DaycardResponse } from "./generated/DaycardResponse";
import type { DaycardActivity } from "./generated/DaycardActivity";
import type { DaycardDoc } from "./generated/DaycardDoc";
export type { DaycardResponse, DaycardActivity, DaycardDoc };
/// Unit 2 — the desk-radiator daycard's JSON twin (`routes/daycard.rs`). The
/// SAME endpoint also serves a self-contained HTML+inline-SVG document for
/// an e-ink panel (content-negotiated server-side on the `Accept` header /
/// `?format=`); the SPA always wants JSON, so `get<T>`'s `Accept:
/// application/json` (see above) already selects that branch — no
/// `?format=json` needed here, but it's harmless to add if that ever
/// changes. `day` is `YYYY-MM-DD` UTC; omitted → today (UTC, server-side).
export const fetchDaycard = (
  kb: string,
  opts: { day?: string } = {},
  signal?: AbortSignal,
): Promise<DaycardResponse> => {
  const params = new URLSearchParams();
  if (opts.day) params.set("day", opts.day);
  const qs = params.toString();
  return get<DaycardResponse>(
    `/api/kb/${encodeURIComponent(kb)}/daycard${qs ? `?${qs}` : ""}`,
    signal,
  );
};

export type AtlasEdge = { src: string; dst: string };

/// GET /api/kb/{kb}/edges — every `kind = 'link'` cross-artifact edge
/// in the kb. Backs the atlas view's curved-edge layer.
export const fetchAtlasEdges = async (
  kb: string,
  signal?: AbortSignal,
): Promise<AtlasEdge[]> => {
  const body = await get<{ edges: AtlasEdge[] }>(
    `/api/kb/${encodeURIComponent(kb)}/edges`,
    signal,
  );
  return body.edges;
};

// DCB W1.D — GET /api/kb/{kb}/docs/{id}/code-refs: one doc's `coderef/1`
// payload (kb-core's extracted, UNRESOLVED code references — resolution
// against a working tree is kb-code's cross-origin `codelens/1`, see
// api/doclens.ts). Same-origin, so a normal fetcher — mirrors
// fetchAtlasEdges above. (W1.D.R #10 — `CodeRefsResponse` is imported at
// the top of the file with every other generated type, not here.)
export const fetchCodeRefs = (
  kb: string,
  docId: string,
  signal?: AbortSignal,
): Promise<CodeRefsResponse> =>
  get<CodeRefsResponse>(
    `/api/kb/${encodeURIComponent(kb)}/docs/${encodeURIComponent(docId)}/code-refs`,
    signal,
  );

// CT-E3 — one page of the corpus coderef/1 feed, headers-only (`refs=0`:
// ref_count/extracted_at/never_scanned intact, `refs: []`/`groups: []`).
// The gallery's ref-chip join walks this via lib/codeRefCounts.ts's
// hard-capped `collectCodeRefCounts`; `cursor` is passed back verbatim
// from the previous page's `next_cursor` (opaque — the route 400s a
// hand-built or empty one rather than silently restarting the walk).
export const fetchCodeRefsFeedPage = (
  kb: string,
  cursor: string | undefined,
  signal?: AbortSignal,
): Promise<CodeRefsFeedResponse> => {
  const params = new URLSearchParams({
    refs: "0",
    limit: String(CODEREF_FEED_PAGE_LIMIT),
  });
  if (cursor !== undefined) params.set("cursor", cursor);
  return get<CodeRefsFeedResponse>(
    `/api/kb/${encodeURIComponent(kb)}/code-refs?${params.toString()}`,
    signal,
  );
};

// W1.B — c-TF-IDF cluster labels computed at atlas recompute/recluster
// time; wire truth is the generated ts-rs bindings. `score = tf *
// ln(1 + avg_tokens/ft)` per cluster/term — decomposable for the legend's
// term-breakdown popover. `AtlasLabelTerm` aliases the generated name so
// AtlasView's imports read naturally.
import type { AtlasLabelsResponse } from "./generated/AtlasLabelsResponse";
import type { AtlasClusterLabels } from "./generated/AtlasClusterLabels";
import type { AtlasTermScore } from "./generated/AtlasTermScore";
export type { AtlasLabelsResponse, AtlasClusterLabels };
export type AtlasLabelTerm = AtlasTermScore;

export const fetchAtlasLabels = (kb: string, signal?: AbortSignal) =>
  get<AtlasLabelsResponse>(
    `/api/kb/${encodeURIComponent(kb)}/atlas/labels`,
    signal,
  );

// W3.M-c — the FULL-corpus atlas point set (`GET /api/kb/{kb}/atlas/points`,
// M-a). Deliberately lean (id/coords/title/source_relative/kb_category —
// NOT the full gallery-card `DocSummary`), memoised server-side on the
// storage-actor generation. Fixes the measured defect where the atlas view
// only ever drew the gallery's first `useDocs` page (pageSize 200, no
// loadMore) while its own status line claimed the full count.
import type { AtlasPointsResponse } from "./generated/AtlasPointsResponse";
import type { AtlasPoint } from "./generated/AtlasPoint";
import type { AtlasClusterCount } from "./generated/AtlasClusterCount";
export type { AtlasPointsResponse, AtlasPoint, AtlasClusterCount };

export const fetchAtlasPoints = (kb: string, signal?: AbortSignal) =>
  get<AtlasPointsResponse>(
    `/api/kb/${encodeURIComponent(kb)}/atlas/points`,
    signal,
  );

// W3.T-c — the atlas time-lapse wire. `GET /api/kb/{kb}/atlas/history` is
// the frame list, NEWEST FIRST; an EMPTY `frames` array is the honest
// answer for a corpus with no recorded frames (never a 404 — kb retains no
// past layout or embedding, so history cannot be backfilled and starts
// empty by construction). `GET .../history/{id}?align_to=` returns ONE
// frame's points ALREADY Procrustes-aligned SERVER-SIDE against `align_to`
// (default: the newest frame), plus the fitted alignment params, the
// residual, and the overlap size. The alignment is deliberately done on the
// daemon so the CLI (`kb atlas show`) and the SPA agree byte for byte —
// callers must NOT re-align client-side.
import type { AtlasHistoryResponse } from "./generated/AtlasHistoryResponse";
import type { AtlasFrameOut } from "./generated/AtlasFrameOut";
import type { AtlasFrameShowResponse } from "./generated/AtlasFrameShowResponse";
import type { AtlasFramePointOut } from "./generated/AtlasFramePointOut";
import type { AtlasAlignmentOut } from "./generated/AtlasAlignmentOut";
export type {
  AtlasHistoryResponse,
  AtlasFrameOut,
  AtlasFrameShowResponse,
  AtlasFramePointOut,
  AtlasAlignmentOut,
};

export const fetchAtlasHistory = (kb: string, signal?: AbortSignal) =>
  get<AtlasHistoryResponse>(
    `/api/kb/${encodeURIComponent(kb)}/atlas/history`,
    signal,
  );

export const fetchAtlasFrame = (
  kb: string,
  id: number,
  alignTo?: number,
  signal?: AbortSignal,
) => {
  const params = new URLSearchParams();
  if (alignTo != null) params.set("align_to", String(alignTo));
  const qs = params.toString();
  return get<AtlasFrameShowResponse>(
    `/api/kb/${encodeURIComponent(kb)}/atlas/history/${encodeURIComponent(String(id))}${qs ? `?${qs}` : ""}`,
    signal,
  );
};

// W2.3a — true (embedding-space) nearest neighbors for one doc. Distinct
// from `fetchAtlasEdges` (the `kind='link'` wikilink/hub graph) and from
// `useRelatedMemories` (text-query recall, rank-position × salience ×
// decay) — this is vector-space cosine similarity, computed server-side
// from the raw vectors (never lance's undecoded `_distance`).
import type { SimilarResponse } from "./generated/SimilarResponse";
import type { SimilarOut } from "./generated/SimilarOut";
export type { SimilarResponse, SimilarOut };

export const fetchSimilar = (
  kb: string,
  id: string,
  limit?: number,
  signal?: AbortSignal,
) => {
  const params = new URLSearchParams();
  if (limit != null) params.set("limit", String(limit));
  const qs = params.toString();
  return get<SimilarResponse>(
    `/api/kb/${encodeURIComponent(kb)}/atlas/similar/${encodeURIComponent(id)}${qs ? `?${qs}` : ""}`,
    signal,
  );
};

/// POST /api/kb/{kb}/atlas/recompute — 202-async kick-off. The actual
/// completion arrives via SSE `atlas.recompute.complete`; callers should
/// subscribe and match on the returned `run` id.
export async function recomputeAtlas(kb: string): Promise<{ run: string }> {
  return atlasKickoff(kb, "recompute");
}

/// POST /api/kb/{kb}/atlas/recluster[?k=N] — Q5 fast path that only
/// re-runs k-means on existing atlas coords (no UMAP). Coords stay,
/// `atlas_cluster` rewrites. Completion arrives via SSE
/// `atlas.recluster.complete`.
export async function reclusterAtlas(
  kb: string,
  k?: number,
): Promise<{ run: string }> {
  const qs = k != null ? `?k=${encodeURIComponent(String(k))}` : "";
  return atlasKickoff(kb, `recluster${qs}`);
}

async function atlasKickoff(
  kb: string,
  pathSuffix: string,
): Promise<{ run: string }> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/atlas/${pathSuffix}`,
    { method: "POST", headers: { Accept: "application/json" } },
  );
  if (!r.ok) await throwApiError(r);
  const body = (await r.json()) as { run: string };
  return { run: body.run };
}

export type SearchMode = "hybrid" | "semantic" | "keyword";

export type SearchScope = "one" | "all";

// Q-track — the search-page sort axis. `relevance` (default) is the score
// order the daemon already returns; the rest re-order the matched pool
// server-side. `opened`/`progress` lean on the read-state rollup.
export type SearchSort =
  | "relevance"
  | "opened"
  | "modified"
  | "created"
  | "indexed"
  | "title"
  | "words"
  | "progress";

// Track F / Q-track — options for the full search page. Drives the same
// /api/search endpoint as the popup, with the deeper controls: page size,
// federated `scope=all`, the faceted filters (tags/meta/date/caps/read-
// state/session/list), the sort menu, and the `rich` flag (→ detail=full)
// that widens each hit with the gallery-card metadata in one round-trip.
// Every faceted field is OPTIONAL and serialises default-out, so the
// popup's positional call stays byte-identical on the wire.
export type SearchOptions = {
  mode?: SearchMode;
  kb?: string;
  limit?: number;
  scope?: SearchScope;
  category?: string;
  folder?: string;
  // Q-track facets — arrays go csv on the wire; absent/empty = no filter.
  tags?: string[];
  excludeTags?: string[];
  status?: string[];
  severity?: string[];
  caps?: string[];
  since?: string; // "day" | "week" | "month" | "year"
  sinceField?: "created" | "modified"; // default "modified" → dropped
  read?: string[]; // ("unread" | "in_progress" | "read")[]
  session?: string;
  list?: string;
  // W1.search — "read during" window (unix seconds; filters to artifacts
  // opened in the window, via the history-opens rollup). Distinct from
  // `since`/`sinceField` (mtime/created recency). This worktree doesn't
  // yet have the regenerated server-side parsing (see the phase brief's
  // wire caveat) — these are plain query params, no generated dependency.
  readFrom?: number;
  readTo?: number;
  sort?: SearchSort; // default "relevance" → dropped
  dir?: "asc" | "desc";
  rich?: boolean;
  signal?: AbortSignal;
};

// Overloaded so the popup keeps its legacy positional call:
// `search(q, mode, kb)` takes the string branch and builds a
// byte-identical URL (no scope/limit/detail/facets), while the full search
// page calls `search(q, { … })` for the deeper controls.
export function search(
  q: string,
  modeOrOpts: SearchMode | SearchOptions = "hybrid",
  kb?: string,
): Promise<SearchResponse> {
  const o: SearchOptions =
    typeof modeOrOpts === "string" ? { mode: modeOrOpts, kb } : modeOrOpts;
  const params = new URLSearchParams({ q, mode: o.mode ?? "hybrid" });
  if (o.kb) params.set("kb", o.kb);
  if (o.limit != null) params.set("limit", String(o.limit));
  if (o.scope && o.scope !== "one") params.set("scope", o.scope);
  if (o.category) params.set("category", o.category);
  if (o.folder) params.set("folder", o.folder);
  // Q-track facets — each default-out so the popup wire is unchanged.
  if (o.tags?.length) params.set("tags", o.tags.join(","));
  if (o.excludeTags?.length) params.set("exclude_tags", o.excludeTags.join(","));
  if (o.status?.length) params.set("status", o.status.join(","));
  if (o.severity?.length) params.set("severity", o.severity.join(","));
  if (o.caps?.length) params.set("caps", o.caps.join(","));
  if (o.since) params.set("since", o.since);
  if (o.sinceField && o.sinceField !== "modified")
    params.set("since_field", o.sinceField);
  if (o.read?.length) params.set("read", o.read.join(","));
  if (o.session) params.set("session", o.session);
  if (o.list) params.set("list", o.list);
  if (o.readFrom != null) params.set("read_from", String(Math.floor(o.readFrom)));
  if (o.readTo != null) params.set("read_to", String(Math.floor(o.readTo)));
  if (o.sort && o.sort !== "relevance") params.set("sort", o.sort);
  if (o.dir) params.set("dir", o.dir);
  if (o.rich) params.set("detail", "full");
  return get<SearchResponse>(`/api/search?${params.toString()}`, o.signal);
}

// --- v0.2 comments API ---------------------------------------------------

export type Anchor =
  | { kind: "file" }
  | { kind: "chapter"; path: string }
  | { kind: "section"; id: string; tag?: string | null; snippet?: string | null }
  | { kind: "selection"; css_path: string; offset: number; snippet: string };

// R3/Y-track wire types live in the generated bindings now (imported +
// re-exported at the top of this file). The SPA only ever READS them
// (comments/replies/attachments come back from the daemon; QuickButtons
// renders choices), so the generated always-present arrays are the
// truth — the old hand-written `?:` tolerance dated from pre-Y daemons.

// ArtifactRef + ReviewFile come from the generated bindings (top of
// file). Note: `schema` is `string` in the generated type — the daemon
// validates the "kb-comments/1" literal on save; emptyReview writes it.

/// GET /api/kb/{kb}/review/{id}. Returns `null` when the file doesn't
/// exist yet (404 → no comments — the SPA serves an empty skeleton).
/// All other errors throw.
export async function fetchReview(
  kb: string,
  id: string,
  signal?: AbortSignal,
): Promise<{ file: ReviewFile; etag: string } | null> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/review/${encodeURIComponent(id)}`,
    { headers: { Accept: "application/json" }, signal },
  );
  if (r.status === 404) return null;
  if (!r.ok) await throwApiError(r);
  const etag = r.headers.get("etag") ?? "";
  const file = (await r.json()) as ReviewFile;
  return { file, etag };
}

// --- R7 fine-grained comment mutations -----------------------------------
//
// The SPA no longer POSTs the whole ReviewFile + retries on 412. Each
// action sends a small delta to a dedicated endpoint and the daemon owns
// the load → mutate → save under its review_lock (no client-side ETag
// juggling, no minted ids — the server assigns `c_<hex>`/`r_<hex>`). The
// `comments.updated` SSE then reconciles every open tab; useReview also
// splices add/reply results in optimistically. Same-origin fetch (the
// daemon serves the SPA), so the request carries the SPA's Origin and
// clears the daemon's origin allowlist without a custom header.

/// Shared writer for the review-mutation endpoints. Same problem+json
/// surfacing as `get`; returns the parsed JSON body (the daemon always
/// answers JSON on 200/201). `body` is JSON-encoded when present.
async function mutate<T>(
  path: string,
  method: "POST" | "PUT" | "PATCH" | "DELETE",
  body?: unknown,
): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) await throwApiError(r);
  return (await r.json()) as T;
}

export type MetaPatch = { tags?: string[]; category?: string };
export type MetaPatchResult = {
  id: string;
  tags: string[];
  kb_category?: string | null;
};

/// PATCH …/artifacts/{id}/meta — edit kb-tags / kb-category in the source
/// file (the watcher re-indexes it). Returns the EFFECTIVE values after the
/// edit: tags slugged server-side, or the path-derived fallback when cleared.
export const updateArtifactMeta = (
  kb: string,
  id: string,
  patch: MetaPatch,
): Promise<MetaPatchResult> =>
  mutate<MetaPatchResult>(
    `/api/kb/${encodeURIComponent(kb)}/artifacts/${encodeURIComponent(id)}/meta`,
    "PATCH",
    patch,
  );

const reviewBase = (kb: string, id: string) =>
  `/api/kb/${encodeURIComponent(kb)}/review/${encodeURIComponent(id)}`;

export type AddCommentInput = {
  body: string;
  anchor: Anchor;
  author: "you" | "claude";
  file?: string;
  fileLabel?: string;
  choices?: Choice[];
  /// Y-track — staged attachment ids to adopt onto the new comment.
  attachment_ids?: string[];
};

/// POST …/comments → 201 + the created Comment (server-assigned id).
export const addComment = (
  kb: string,
  artifactId: string,
  input: AddCommentInput,
): Promise<Comment> =>
  mutate<Comment>(`${reviewBase(kb, artifactId)}/comments`, "POST", input);

/// POST …/comments/{cid}/replies → 201 + the created Reply.
export const addReply = (
  kb: string,
  artifactId: string,
  commentId: string,
  input: {
    author: "you" | "claude";
    body: string;
    choices?: Choice[];
    /// Y-track — staged attachment ids to adopt onto the new reply.
    attachment_ids?: string[];
  },
): Promise<Reply> =>
  mutate<Reply>(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/replies`,
    "POST",
    input,
  );

/// POST …/comments/{cid}/resolve → { ok, open_count }.
export const resolveComment = (
  kb: string,
  artifactId: string,
  commentId: string,
): Promise<{ ok: boolean; open_count: number }> =>
  mutate(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/resolve`,
    "POST",
  );

/// GET …/artifact/{id} — raw artifact HTML bytes. Used by the portable
/// "export with comments" flow to fetch the document before embedding.
export async function fetchArtifactHtml(
  kb: string,
  artifactId: string,
): Promise<string> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/artifact/${encodeURIComponent(artifactId)}`,
    { headers: { Accept: "text/html" } },
  );
  if (!r.ok) await throwApiError(r);
  return r.text();
}

/// POST …/review/{id}/import[?force=] — write a whole kb-comments/1 document
/// back to the sidecar (the read side of the portable round-trip). `force`
/// overwrites existing non-empty comments. The daemon emits `comments.updated`,
/// so the SSE bridge refreshes the panel — no manual invalidation needed.
export const importReview = (
  kb: string,
  artifactId: string,
  file: ReviewFile,
  force = false,
): Promise<{ ok: boolean; imported: number; open_count: number }> =>
  mutate(
    `${reviewBase(kb, artifactId)}/import${force ? "?force=true" : ""}`,
    "POST",
    file,
  );

/// POST …/comments/{cid}/unresolve → { ok, open_count }.
export const unresolveComment = (
  kb: string,
  artifactId: string,
  commentId: string,
): Promise<{ ok: boolean; open_count: number }> =>
  mutate(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/unresolve`,
    "POST",
  );

/// POST …/resolve-all → { flipped, open_count }.
export const resolveAll = (
  kb: string,
  artifactId: string,
): Promise<{ flipped: number; open_count: number }> =>
  mutate(`${reviewBase(kb, artifactId)}/resolve-all`, "POST");

/// POST …/unresolve-all → { flipped, open_count }.
export const unresolveAll = (
  kb: string,
  artifactId: string,
): Promise<{ flipped: number; open_count: number }> =>
  mutate(`${reviewBase(kb, artifactId)}/unresolve-all`, "POST");

/// PATCH …/comments/{cid} { body } → { ok }.
export const editComment = (
  kb: string,
  artifactId: string,
  commentId: string,
  body: string,
): Promise<{ ok: boolean }> =>
  mutate(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}`,
    "PATCH",
    { body },
  );

/// PATCH …/comments/{cid}/replies/{rid} { body } → { ok }.
export const editReply = (
  kb: string,
  artifactId: string,
  commentId: string,
  replyId: string,
  body: string,
): Promise<{ ok: boolean }> =>
  mutate(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/replies/${encodeURIComponent(replyId)}`,
    "PATCH",
    { body },
  );

/// DELETE …/comments/{cid} → { ok, open_count }.
export const deleteComment = (
  kb: string,
  artifactId: string,
  commentId: string,
): Promise<{ ok: boolean; open_count: number }> =>
  mutate(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}`,
    "DELETE",
  );

/// DELETE …/comments/{cid}/replies/{rid} → { ok }.
export const deleteReply = (
  kb: string,
  artifactId: string,
  commentId: string,
  replyId: string,
): Promise<{ ok: boolean }> =>
  mutate(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/replies/${encodeURIComponent(replyId)}`,
    "DELETE",
  );

/// W2.15a — set (`verdict` object) or clear (`null`) the artifact's
/// three-state review-pass verdict (`ReviewFile.verdict`, distinct from any
/// individual comment's open/resolved status). POST …/verdict when setting,
/// DELETE …/verdict when clearing — one fn, the grammar switches on
/// `verdict === null`.
export const setVerdict = (
  kb: string,
  artifactId: string,
  verdict: { state: VerdictState; note?: string } | null,
): Promise<{ ok: boolean; verdict?: Verdict }> =>
  verdict === null
    ? mutate(`${reviewBase(kb, artifactId)}/verdict`, "DELETE")
    : mutate(`${reviewBase(kb, artifactId)}/verdict`, "POST", verdict);

// --- Y-track: attachments -------------------------------------------------
//
// Multipart upload via FormData — the browser sets the multipart boundary,
// so DON'T set Content-Type. Same problem+json surfacing as `mutate`.

async function uploadFiles<T>(path: string, files: File[]): Promise<T> {
  const form = new FormData();
  for (const f of files) form.append("file", f, f.name);
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    method: "POST",
    body: form,
  });
  if (!r.ok) await throwApiError(r);
  return (await r.json()) as T;
}

/// POST …/attachments (multipart) → the staged Attachment(s) (compose-time;
/// not yet adopted by any comment).
export const uploadAttachment = (
  kb: string,
  artifactId: string,
  files: File[],
): Promise<Attachment[]> =>
  uploadFiles<Attachment[]>(`${reviewBase(kb, artifactId)}/attachments`, files);

/// POST …/comments/{cid}/attachments (multipart) → upload + adopt onto an
/// existing comment.
export const attachToComment = (
  kb: string,
  artifactId: string,
  commentId: string,
  files: File[],
): Promise<Attachment[]> =>
  uploadFiles<Attachment[]>(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/attachments`,
    files,
  );

/// POST …/comments/{cid}/replies/{rid}/attachments (multipart) → adopt onto a reply.
export const attachToReply = (
  kb: string,
  artifactId: string,
  commentId: string,
  replyId: string,
  files: File[],
): Promise<Attachment[]> =>
  uploadFiles<Attachment[]>(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/replies/${encodeURIComponent(replyId)}/attachments`,
    files,
  );

/// DELETE …/comments/{cid}/attachments/{aid} — detach (the orphaned blob is
/// GC-reaped server-side).
export const detachAttachment = (
  kb: string,
  artifactId: string,
  commentId: string,
  aid: string,
): Promise<unknown> =>
  mutate(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/attachments/${encodeURIComponent(aid)}`,
    "DELETE",
  );

/// DELETE …/comments/{cid}/replies/{rid}/attachments/{aid} — detach from a reply.
export const detachReplyAttachment = (
  kb: string,
  artifactId: string,
  commentId: string,
  replyId: string,
  aid: string,
): Promise<unknown> =>
  mutate(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/replies/${encodeURIComponent(replyId)}/attachments/${encodeURIComponent(aid)}`,
    "DELETE",
  );

// --- kb share -------------------------------------------------------------

export type CreateShareInput = {
  /// Source-relative file or folder to publish.
  target: string;
  /// `cloudflare-pages` (default) or `github-pages`.
  host?: string;
  /// Gate rules (Cloudflare only): `email:DOMAIN` | `email:a@x,b@y` |
  /// `google` | `github`. Empty + `public` = ungated.
  gate?: string[];
  public?: boolean;
  /// `warn` (default) or `absolute`.
  links?: string;
  update?: boolean;
  no_scrub?: boolean;
  /// Y-track — publish comment threads + attachments into the static site
  /// (PUBLISHES otherwise-private review state).
  include_comments?: boolean;
};

export type ShareResult = {
  name: string;
  url: string;
  host: string;
  gate: string | null;
  /// Artifact ids linked from the share but not included in it.
  danglers: string[];
  files: number;
  updated: boolean;
};

export type ShareListItem = {
  name: string;
  target: string;
  host: string;
  url: string;
  gate: string | null;
  created_at: number;
  updated_at: number;
};

/// POST /api/kb/{kb}/share — publish (deploy + gate) → the live URL.
export const createShare = (
  kb: string,
  input: CreateShareInput,
): Promise<ShareResult> =>
  mutate<ShareResult>(
    `/api/kb/${encodeURIComponent(kb)}/share`,
    "POST",
    input,
  );

/// GET /api/kb/{kb}/shares — recorded shares, newest-first.
export const listShares = (kb: string, signal?: AbortSignal) =>
  get<ShareListItem[]>(`/api/kb/${encodeURIComponent(kb)}/shares`, signal);

/// DELETE /api/kb/{kb}/shares/{name} — revoke (teardown + drop the row).
export const revokeShare = (
  kb: string,
  name: string,
): Promise<{ revoked: boolean; name: string }> =>
  mutate(
    `/api/kb/${encodeURIComponent(kb)}/shares/${encodeURIComponent(name)}`,
    "DELETE",
  );

export type ExportBundleInput = {
  /// Source-relative file/folder to bundle.
  target: string;
  /// `warn` (default) or `absolute` — for OUT-of-bundle links.
  links?: string;
  no_scrub?: boolean;
  include_comments?: boolean;
};

export type ExportBundleResult = {
  /// The `.zip` bytes (the caller triggers the save).
  blob: Blob;
  /// Server-suggested download filename (`<kb>-<target>.zip`).
  filename: string;
  /// Deploy-relative entry page (`index.html` or the first HTML file).
  entry: string;
  /// Total files in the bundle (HTML + assets).
  files: number;
  /// Artifact ids linked from the bundle but NOT in it (dead links offline).
  danglers: string[];
};

/// POST /api/kb/{kb}/share/export — stage a self-contained OFFLINE bundle and
/// return the `.zip` blob + its metadata. Read-only on the server (no host,
/// no registry row, no tokens); the in-share cross-artifact links are rewritten
/// to relative paths so the bundle works with no daemon. The caller saves it.
export async function exportShareBundle(
  kb: string,
  input: ExportBundleInput,
): Promise<ExportBundleResult> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/share/export`,
    {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/zip",
      },
      body: JSON.stringify(input),
    },
  );
  if (!r.ok) await throwApiError(r, "share export");
  return parseShareZipResponse(r, kb);
}

export type ExportListShareResult = ExportBundleResult & {
  /// List entry ids that were tombstoned/unresolvable and left out of the zip.
  skipped: string[];
};

/// POST /api/kb/{kb}/lists/{id}/share/export — stage a reading list as a
/// self-contained OFFLINE `.zip` (ordered entries + generated TOC index.html).
/// Tombstoned entries are skipped and reported in `skipped` / the
/// `x-kb-share-skipped` header.
export async function exportListShareBundle(
  kb: string,
  listId: string,
  input: { links?: string; no_scrub?: boolean; include_comments?: boolean } = {},
): Promise<ExportListShareResult> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(listId)}/share/export`,
    {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/zip",
      },
      body: JSON.stringify(input),
    },
  );
  if (!r.ok) await throwApiError(r, "list share export");
  const base = await parseShareZipResponse(r, kb);
  const skipped = (r.headers.get("x-kb-share-skipped") ?? "")
    .split(",")
    .filter((s) => s.length > 0);
  return { ...base, skipped };
}

async function parseShareZipResponse(
  r: Response,
  kb: string,
): Promise<ExportBundleResult> {
  const blob = await r.blob();
  const dispo = r.headers.get("content-disposition") ?? "";
  const filename =
    /filename="?([^"]+)"?/i.exec(dispo)?.[1] ?? `${kb}-bundle.zip`;
  const danglers = (r.headers.get("x-kb-share-danglers") ?? "")
    .split(",")
    .filter((s) => s.length > 0);
  return {
    blob,
    filename,
    entry: r.headers.get("x-kb-share-entry") ?? "",
    files: Number(r.headers.get("x-kb-share-files") ?? "0") || 0,
    danglers,
  };
}

export type ExportPageInput = {
  /// Source-relative file (a single artifact) to export.
  target: string;
  no_scrub?: boolean;
};

export type ExportPageResult = {
  /// The single-page bytes (HTML or Markdown — `contentType` says which).
  blob: Blob;
  /// Server-suggested filename (the source basename, native extension).
  filename: string;
  /// `text/html; charset=utf-8` or `text/markdown; charset=utf-8`.
  contentType: string;
  /// Cross-artifact links not in the lone page (dead in the standalone file).
  danglers: string[];
};

/// POST /api/kb/{kb}/share/export/page — stage ONE artifact UNCOMPRESSED in its
/// native format: a scrubbed, self-contained `.html`, or the raw `.md` SOURCE
/// for a Markdown artifact (never rendered). The same export scrub as the
/// bundle (kb-prompt stripped + outbound redactions). Read-only on the server
/// (no host, no registry row); the caller saves the returned blob.
export async function exportSharePage(
  kb: string,
  input: ExportPageInput,
): Promise<ExportPageResult> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/share/export/page`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
    },
  );
  if (!r.ok) await throwApiError(r, "share export page");
  const blob = await r.blob();
  const dispo = r.headers.get("content-disposition") ?? "";
  const filename = /filename="?([^"]+)"?/i.exec(dispo)?.[1] ?? `${kb}-page`;
  const danglers = (r.headers.get("x-kb-share-danglers") ?? "")
    .split(",")
    .filter((s) => s.length > 0);
  return {
    blob,
    filename,
    contentType: r.headers.get("content-type") ?? "application/octet-stream",
    danglers,
  };
}

/// Build an empty kb-comments/1 envelope, used when fetchReview returns
/// null and the SPA needs something to render against.
export function emptyReview(kb: string, artifactId: string, title = ""): ReviewFile {
  return {
    schema: "kb-comments/1",
    artifact: { id: artifactId, title, kb, tags: [], pages: [] },
    generatedAt: new Date().toISOString(),
    comments: [],
  };
}
