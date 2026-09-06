// DCB W1.D — kb-code's `codelens/1` + `codelens-scorecard/1` cross-origin
// fetch surface, plus the `codelens-pin/1` write.
//
// kb-code is a SEPARATE crate/process with no ts-rs export pipeline of its
// own (12-w1c has no `just types` equivalent), so these types are
// HAND-WRITTEN — against the LANDED Rust wire
// (crates/kb-code-server/src/doclens/{wire.rs,resolve.rs}), not against
// 13-w1d-kb-spa.md §1.2/§1.3's prose draft, which predates several fields
// going non/nullable during W1.C's build. Notable deviations from that
// draft, recorded here rather than silently:
//   - `candidate_count` and `symbol_state` are ALWAYS present on a ref
//     (never null — they default to `0`/`"no_symbol"` rather than being
//     omitted), so callers tier on `path_state`, not on a null check.
//   - `context` is nullable here (kb-code re-serves whatever coderef/1
//     sent, which itself stores an `Option`).
//   - `reader.line` is nullable: a `present` path with no line hint at all
//     still carries a `reader`, just with `line: null`.
//   - `doc_title`/`doc_path` are nullable on both `DocLensOut` and
//     `DocLensScorecardOut` (the plan's draft had them as plain `string`).
//   - `DocLensScorecardOut` carries an additional `never_scanned` field the
//     draft didn't have (unused here — kb's own `coderef/1` fetch already
//     answers that question before this module's queries are even enabled,
//     see `useCodeRefs`/`hasCodeRefs` in PreviewInspector.tsx).
//
// This is kb's SPA's first-ever cross-origin credentialed fetch — every
// prior fetcher in web/src/api/*.ts is same-origin. Registered as the
// FOURTH documented #23 exception (see queryClient.ts's "Deliberately NOT
// bridged" block): finite staleTime, a manual refresh affordance, no SSE
// tie (kb-code is a different daemon with no wiring into kb's `sse`
// facade).

export type RepoState = "ready" | "indexing" | "error";
export type PathState = "present" | "ambiguous" | "absent" | "external";
export type LineState = "confirmed" | "drifted" | "unverifiable" | "absent";
export type LineEvidence = "rev_remap" | "context_token" | "none";
export type SymbolState =
  | "hit_unique"
  | "hit_container_matched"
  | "hit_ambiguous"
  | "no_symbol";

export type LensRepo = {
  name: string;
  root: string;
  state: RepoState;
  head_sha: string | null;
  head_branch: string | null;
  dirty: boolean | null;
  /// `"param"` (an explicit `?repo=`) or `"pin"` — never "guessed"
  /// (Decision 1: a checkout is never auto-selected).
  source: string;
};

export type SymbolHit = {
  path: string;
  line_start: number;
  line_end: number;
  kind: string;
  container: string | null;
};

/// One resolved span of a `path_list` ref (or the single span of a
/// `path_line`/`path_range` ref).
export type SpanOutcome = {
  line_hint: number;
  line_hint_end: number | null;
  line_state: LineState;
  line_evidence: LineEvidence;
  confirm_token: string | null;
  token_line: number | null;
  resolved_line: number | null;
  resolved_line_end: number | null;
  line_hint_delta: number | null;
  line_reason: string | null;
};

export type IssueRef = {
  owner: string;
  repo: string;
  number: number;
  href: string;
};

export type SearchLink = { q: string; repo: string };

export type ReaderTarget = { repo: string; path: string; line: number | null };

export type ResolvedCodeRef = {
  ordinal: number;
  group: string | null;
  kind: string;
  raw: string;
  declared: boolean;

  // --- what the document said (hints) ---------------------------------
  path_hint: string | null;
  line_hint: number | null;
  line_hint_end: number | null;
  symbol_container: string | null;
  symbol_member: string | null;
  context: string | null;

  // --- what this daemon verified ---------------------------------------
  path_state: PathState | null;
  resolved_path: string | null;
  candidate_count: number;
  candidates: string[];
  issue: IssueRef | null;

  line_state: LineState;
  line_evidence: LineEvidence;
  confirm_token: string | null;
  token_line: number | null;
  resolved_line: number | null;
  line_hint_delta: number | null;
  file_lines: number;
  line_reason: string | null;
  spans: SpanOutcome[];

  symbol_state: SymbolState;
  symbol_hit_count: number;
  symbol_hits: SymbolHit[];

  reader: ReaderTarget | null;
  search: SearchLink | null;
  note: string | null;
};

export type LensCounts = {
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
};

export type LensGroup = {
  key: string;
  label: string;
  anchor: string;
  ordinal: number;
  ref_count: number;
};

export type CodeRevOut = { repo_label: string; sha: string; dirty: boolean };

export type DocLensOut = {
  schema: string;
  kb: string;
  doc_id: string;
  moved_from: string | null;
  doc_path: string | null;
  doc_href: string | null;
  doc_hash: string | null;
  doc_title: string | null;
  doc_extracted_at: number | null;
  doc_code_rev: CodeRevOut | null;
  never_scanned: boolean;
  repo: LensRepo;
  resolved_unix: number;
  truncated: boolean;
  partial: boolean;
  partial_reason: string | null;
  counts: LensCounts;
  ungrouped_count: number;
  groups: LensGroup[];
  refs: ResolvedCodeRef[];
  note: string;
};

export type ScorecardRepoRow = {
  name: string;
  root: string;
  state: RepoState;
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
  /// Set (never null) when `state !== "ready"`.
  reason: string | null;
};

export type DocLensScorecardOut = {
  schema: string;
  kb: string;
  doc_id: string;
  doc_hash: string | null;
  doc_title: string | null;
  never_scanned: boolean;
  resolved_unix: number;
  /// Drives §7.1's pre-selection; `null` when the doc has no pin.
  pinned_repo: string | null;
  counted_refs: number;
  truncated: boolean;
  repos: ScorecardRepoRow[];
  note: string;
};

export type DocLensPinOut = {
  schema: string;
  kb: string;
  doc_id: string;
  repo: string;
  repo_root: string;
  doc_hash: string | null;
  pinned_at: number;
};

const DOCLENS_TIMEOUT_MS = 8_000;

function trimBase(url: string): string {
  return url.replace(/\/+$/, "");
}

/// A browser `fetch()` cannot distinguish "CORS origin not allowlisted"
/// from "kb-code process down" from "network unreachable" — all three
/// throw the same opaque `TypeError`/`AbortError`, with no readable status
/// or body (CORS-blocked responses are invisible to JS by design). This is
/// why the degrade matrix (13-w1d-kb-spa.md §6) collapses those three into
/// ONE honest compound message, discriminated by the PRESENCE of a
/// `.reason` on the thrown error — never by trying to distinguish the
/// underlying cause, which the platform makes structurally impossible.
///
/// kb-code's error body is plain JSON `{error, reason?}`
/// (crates/kb-code-server/src/routes.rs's `ApiError::into_response` —
/// verified, not `application/problem+json`). That shape is parsed as
/// PRIMARY; problem+json is a fallback only, kept so a future non-doclens
/// kb-code error path degrades gracefully instead of throwing on `.json()`.
/// `signal` (W1.D.R #9) is TanStack's own queryFn abort signal — cancelling
/// a superseded fetch (rapid repo-switch, doc navigation) promptly instead
/// of leaving it in flight until the 8s timeout. `AbortSignal.any` combines
/// it with the existing timeout so EITHER firing aborts the request; the
/// timeout alone still applies when no caller signal is given (the plain
/// `fetch()` call below `getCrossOrigin` directly, if any is ever added).
function requestSignal(signal?: AbortSignal): AbortSignal {
  const timeout = AbortSignal.timeout(DOCLENS_TIMEOUT_MS);
  return signal ? AbortSignal.any([timeout, signal]) : timeout;
}

async function getCrossOrigin<T>(url: string, signal?: AbortSignal): Promise<T> {
  const r = await fetch(url, {
    headers: { Accept: "application/json" },
    credentials: "include", // D-A: same-site Authelia session cookie rides
    signal: requestSignal(signal),
  });
  if (!r.ok) {
    let message: string | undefined;
    let reason: string | undefined;
    const ct = r.headers.get("content-type") ?? "";
    try {
      if (ct.includes("application/problem+json")) {
        const problem = (await r.json()) as { title: string; detail?: string };
        message = `${problem.title}: ${problem.detail ?? ""}`;
      } else {
        const body = (await r.json()) as { error?: string; reason?: string };
        message = body.error;
        reason = body.reason;
      }
    } catch {
      /* torn/non-JSON body — fall through to the status line */
    }
    const err = new Error(message ?? `${r.status} ${r.statusText}`);
    (err as Error & { reason?: string }).reason = reason;
    throw err;
  }
  return (await r.json()) as T;
}

export function fetchDocLens(
  codeUrl: string,
  kb: string,
  docId: string,
  repo: string,
  signal?: AbortSignal,
): Promise<DocLensOut> {
  const base = trimBase(codeUrl);
  const q = new URLSearchParams({ kb, doc: docId, repo });
  return getCrossOrigin<DocLensOut>(`${base}/api/doc-lens?${q}`, signal);
}

export function fetchDocLensScorecard(
  codeUrl: string,
  kb: string,
  docId: string,
  signal?: AbortSignal,
): Promise<DocLensScorecardOut> {
  const base = trimBase(codeUrl);
  const q = new URLSearchParams({ kb, doc: docId });
  return getCrossOrigin<DocLensScorecardOut>(
    `${base}/api/doc-lens/repos?${q}`,
    signal,
  );
}

/// `PUT /api/doc-lens/pin` — R5's pin write, fired on every repo pick
/// (`PreviewInspector.tsx`'s `pickRepo`). A JSON body is not a
/// CORS-safelisted `Content-Type`, so this is NOT a "simple request" — the
/// browser preflights with an `OPTIONS` the daemon's `pin_cors` layer
/// answers.
export function putDocLensPin(
  codeUrl: string,
  kb: string,
  docId: string,
  repo: string,
  docHash: string | null,
): Promise<DocLensPinOut> {
  const base = trimBase(codeUrl);
  return fetch(`${base}/api/doc-lens/pin`, {
    method: "PUT",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    credentials: "include",
    signal: AbortSignal.timeout(DOCLENS_TIMEOUT_MS),
    body: JSON.stringify({ kb, doc: docId, repo, doc_hash: docHash }),
  }).then(async (r) => {
    if (!r.ok) throw new Error(`pin failed: ${r.status} ${r.statusText}`);
    return (await r.json()) as DocLensPinOut;
  });
}

// ── D29 (v0.42) — the path-addressed lens the slate board captions with ───
//
// The slate board cites REPO paths (`path:crates/foo/src/bar.rs:141`) on
// posts that are not kb documents at all, so none of the doc-scoped reads
// above can answer for them: `/api/doc-lens` needs a kb ARTIFACT ID, and
// `/api/doc-lens/resolve-path` resolves a kb SOURCE path to that id — a
// different question entirely.
//
// SHIPPED (SL7e landed `GET /api/doc-lens/path`, joining the same CORS'd
// `doclens_read` set as `/doc-lens`/`/doc-lens/repos`/`/doc-lens/pins`; SL7f
// added `?repo=`/`?context=` below). A production kb-code serving 2+ repos
// still answers `400 repo_required` with no `?repo=` — this SPA always
// sends one (the board's own slug), and a caller that gets `repo_required`
// anyway (a slug naming no configured repo) must caption `unknown` and
// never retry the same ref without `repo` (`useSlateGrounding`'s
// `retry: false` already guarantees the "never retry" half). A
// CORS-blocked/offline/down kb-code is still indistinguishable from every
// other opaque `fetch()` failure, still collapsed to the one honest
// `unknown` degrade (D29) rather than a guessed `grounded`.
export type PathLensOut = {
  schema?: string;
  repo?: string | null;
  path?: string | null;
  path_state?: PathState | null;
  /// Echoes the `?line=` this call asked about (`null`/absent = none). Lets
  /// `groundednessOf` tell "no line was asked" (the path alone is the
  /// claim) apart from "a line WAS asked and kb-code sent back no verdict
  /// at all" — the two must not collapse to the same caption.
  line_hint?: number | null;
  line_state?: LineState | null;
  resolved_path?: string | null;
  resolved_line?: number | null;
  file_lines?: number | null;
};

/// `GET /api/doc-lens/path?kb=&path=[&line=][&repo=][&context=]`. `kb`
/// rides along for the same `[doclens] kbs` allowlist gate every other
/// doclens read applies. `repo` is optional (kb-code picks its lone
/// configured checkout when a caller has no opinion — REQUIRED once it
/// serves 2+, else `400 repo_required`). `context` (SL7f) is the caller's
/// own line text — the ONLY way a `?line=` can resolve to
/// `confirmed`/`drifted` rather than the honest `unverifiable` default;
/// capped server-side (truncated, never rejected).
export function fetchPathLens(
  codeUrl: string,
  kb: string,
  path: string,
  line: number | null,
  repo?: string | null,
  context?: string | null,
  signal?: AbortSignal,
): Promise<PathLensOut> {
  const base = trimBase(codeUrl);
  const q = new URLSearchParams({ kb, path });
  if (line !== null) q.set("line", String(line));
  if (repo) q.set("repo", repo);
  if (context) q.set("context", context);
  return getCrossOrigin<PathLensOut>(`${base}/api/doc-lens/path?${q}`, signal);
}
