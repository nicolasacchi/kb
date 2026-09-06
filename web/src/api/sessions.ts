// v0.14 S5 — client for /api/sessions/*. The /sessions SPA route and
// the atlas-overlay (S7) both consume these. All calls are
// best-effort: a daemon hiccup logs + returns an empty payload so a
// transient miss doesn't surface as a hard UI failure.

import { currentDaemonBase } from "./base";
// Wire types are the generated ts-rs bindings (routes/sessions.rs is
// the source of truth; `just types` regenerates).
import type { SessionRow } from "./generated/SessionRow";
import type { SessionsListResponse } from "./generated/SessionsListResponse";
import type { SessionDetail } from "./generated/SessionDetail";
import type { SessionMemoryHit } from "./generated/SessionMemoryHit";
import type { SessionMemoriesResponse } from "./generated/SessionMemoriesResponse";
import type { SessionRecallHit } from "./generated/SessionRecallHit";
import type { SessionRecallsResponse } from "./generated/SessionRecallsResponse";
import type { TouchesConfidence } from "./generated/TouchesConfidence";
import type { TouchesResponse } from "./generated/TouchesResponse";
import type { FolderOut } from "./generated/FolderOut";
import type { SessionFoldersResponse } from "./generated/SessionFoldersResponse";
import type { SessionFileOut } from "./generated/SessionFileOut";
import type { SessionFilesResponse } from "./generated/SessionFilesResponse";
import type { ArtifactSessionOut } from "./generated/ArtifactSessionOut";
import type { ArtifactSessionsResponse } from "./generated/ArtifactSessionsResponse";
import type { DecisionOut } from "./generated/DecisionOut";
import type { SessionDecisionsResponse } from "./generated/SessionDecisionsResponse";
import type { CommitOut } from "./generated/CommitOut";
import type { SessionCommitsResponse } from "./generated/SessionCommitsResponse";
// MI-W4.6 — the provenance thread's 3rd hop: per-commit touched-file
// staleness.
import type { TouchedFileOut } from "./generated/TouchedFileOut";
import type { CommitFilesResponse } from "./generated/CommitFilesResponse";
import type { ThreadOut } from "./generated/ThreadOut";
import type { ThreadsResponse } from "./generated/ThreadsResponse";
import type { SaveThreadResponse } from "./generated/SaveThreadResponse";
// R2/R4/R5 — episodic-memory surfaces.
import type { WhyResponse } from "./generated/WhyResponse";
import type { WhySessionOut } from "./generated/WhySessionOut";
import type { SessionResearchResponse } from "./generated/SessionResearchResponse";
import type { ResearchOut } from "./generated/ResearchOut";
import type { SessionCommentsResponse } from "./generated/SessionCommentsResponse";
import type { SessionCommentArtifactOut } from "./generated/SessionCommentArtifactOut";
import type { SessionCommentOut } from "./generated/SessionCommentOut";
import type { RaisedCommentOut } from "./generated/RaisedCommentOut";
// A-w2 (wave-1 wire-don't-build) — R9's funnel + research-rollup endpoints
// shipped server-side with no SPA caller; these are the fetchers.
import type { FunnelResponse } from "./generated/FunnelResponse";
import type { FunnelStageOut } from "./generated/FunnelStageOut";
import type { ResearchRollupResponse } from "./generated/ResearchRollupResponse";
import type { ResearchRollupFolderOut } from "./generated/ResearchRollupFolderOut";
import type { ResearchRollupOut } from "./generated/ResearchRollupOut";
// W3.A — the projects facet + the by-artifact join + two-lane search.
import type { ProjectOut } from "./generated/ProjectOut";
import type { SessionsProjectsResponse } from "./generated/SessionsProjectsResponse";
import type { ByArtifactResponse } from "./generated/ByArtifactResponse";
import type { RecollectSessionOut } from "./generated/RecollectSessionOut";
import type { RecollectResponse } from "./generated/RecollectResponse";
// W3.E/S5 — `?turn=N` resolution: the outline projection of `/view`.
import type { OutlineRow } from "./generated/OutlineRow";
import type { SessionViewResponse } from "./generated/SessionViewResponse";
// W6 (moonshots M4) — the project ledger.
import type { LedgerResponse } from "./generated/LedgerResponse";
import type { LedgerDayOut } from "./generated/LedgerDayOut";
import type { LedgerSessionOut } from "./generated/LedgerSessionOut";
import type { LedgerCommitOut } from "./generated/LedgerCommitOut";
import type { LedgerTotalsOut } from "./generated/LedgerTotalsOut";

export type {
  SessionRow,
  SessionsListResponse,
  SessionDetail,
  SessionMemoryHit,
  SessionMemoriesResponse,
  SessionRecallHit,
  SessionRecallsResponse,
  TouchesConfidence,
  TouchesResponse,
  FolderOut,
  SessionFoldersResponse,
  SessionFileOut,
  SessionFilesResponse,
  ArtifactSessionOut,
  ArtifactSessionsResponse,
  DecisionOut,
  SessionDecisionsResponse,
  CommitOut,
  SessionCommitsResponse,
  TouchedFileOut,
  CommitFilesResponse,
  ThreadOut,
  ThreadsResponse,
  SaveThreadResponse,
  WhyResponse,
  WhySessionOut,
  SessionResearchResponse,
  ResearchOut,
  SessionCommentsResponse,
  SessionCommentArtifactOut,
  SessionCommentOut,
  RaisedCommentOut,
  FunnelResponse,
  FunnelStageOut,
  ResearchRollupResponse,
  ResearchRollupFolderOut,
  ResearchRollupOut,
  ProjectOut,
  SessionsProjectsResponse,
  ByArtifactResponse,
  RecollectSessionOut,
  RecollectResponse,
  OutlineRow,
  SessionViewResponse,
  LedgerResponse,
  LedgerDayOut,
  LedgerSessionOut,
  LedgerCommitOut,
  LedgerTotalsOut,
};

async function jsonOrEmpty<T>(r: Response, fallback: T): Promise<T> {
  if (!r.ok) {
    if (r.status === 404) return fallback;
    console.warn(`[sessions] ${r.url}: ${r.status} ${r.statusText}`);
    return fallback;
  }
  try {
    return (await r.json()) as T;
  } catch (e) {
    console.warn(`[sessions] ${r.url}: bad JSON`, e);
    return fallback;
  }
}

export async function fetchSessions(
  signal?: AbortSignal,
): Promise<SessionRow[]> {
  const r = await fetch(`${currentDaemonBase()}/api/sessions`, {
    headers: { Accept: "application/json" },
    signal,
  });
  const body = await jsonOrEmpty<SessionsListResponse>(r, { sessions: [] });
  return body.sessions;
}

/// T5 — paginated variant. Used by the /sessions view when the user
/// scrolls to the bottom of the list; the daemon returns at most
/// `limit` rows + a `next_cursor` (`started_at` of the last row)
/// for the follow-up call.
export async function fetchSessionsPage(
  opts: {
    cursor?: number;
    cursor_id?: string;
    limit?: number;
    folder?: string;
    q?: string;
    /// W3.A — the registry id or raw `project_key` (`?project=`).
    project?: string;
    /// W3.A/S1 — csv over `trivial|routine|substantive`.
    substance?: string;
    /// W5/I — csv over the closed harness set
    /// (claude|codex|opencode|grok|kimi|omp). OK1: an unknown token 400s.
    harness?: string;
  } = {},
  signal?: AbortSignal,
): Promise<SessionsListResponse> {
  const params = new URLSearchParams();
  if (opts.cursor !== undefined) params.set("cursor", String(opts.cursor));
  if (opts.cursor_id !== undefined) params.set("cursor_id", opts.cursor_id);
  if (opts.limit !== undefined) params.set("limit", String(opts.limit));
  if (opts.folder) params.set("folder", opts.folder);
  if (opts.q) params.set("q", opts.q);
  if (opts.project) params.set("project", opts.project);
  if (opts.substance) params.set("substance", opts.substance);
  if (opts.harness) params.set("harness", opts.harness);
  const qs = params.toString();
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions${qs ? `?${qs}` : ""}`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<SessionsListResponse>(r, { sessions: [] });
}

// W3.A/P4 — the projects facet: one card per registry entry / auto-project.
export async function fetchSessionProjects(
  signal?: AbortSignal,
): Promise<ProjectOut[]> {
  const r = await fetch(`${currentDaemonBase()}/api/sessions/projects`, {
    headers: { Accept: "application/json" },
    signal,
  });
  const body = await jsonOrEmpty<SessionsProjectsResponse>(r, { projects: [] });
  return body.projects;
}

// W3.E/S3 — the by-artifact join: which session (if any) is BEHIND this
// capture artifact, and whether it's the newest capture. Replaces the old
// filename-regex `SessionSelfLink` sid recovery.
export async function fetchSessionByArtifact(
  kb: string,
  artifactId: string,
  signal?: AbortSignal,
): Promise<ByArtifactResponse> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/by-artifact/${encodeURIComponent(kb)}/${encodeURIComponent(artifactId)}`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<ByArtifactResponse>(r, { session: undefined, newest: false });
}

// W3.E/S5 — the outline projection of `/view` (`?fields=outline`): `?turn=N`
// deep links resolve the ordinal to the stable `t-<uuid12>` render id via
// this list (`OutlineRow.n` ↔ `.id`), fetched lazily only when a numeric
// `?turn=` is actually present (see `ArtifactPane`'s `useSessionOutline`).
// PF-R1: `turns=all` explicit — `outline` itself isn't windowed by the
// server's new tail-window default (every row covers every turn either
// way), but a `?turn=N` deep link can target ANY ordinal in the session,
// so this fetch states outright that it wants the complete picture rather
// than leaning on outline's current windowing-immunity as an implicit
// guarantee.
export async function fetchSessionOutline(
  sessionId: string,
  signal?: AbortSignal,
): Promise<OutlineRow[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/view?fields=outline&turns=all`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<Partial<SessionViewResponse>>(r, {});
  return body.outline ?? [];
}

// W3.C/P5 — lane B of the two-lane sessions search: semantic "has this been
// done?" over the R1 digests, scoped to the current project when given.
export async function fetchSessionRecollect(
  q: string,
  opts: { project?: string; limit?: number } = {},
  signal?: AbortSignal,
): Promise<RecollectResponse> {
  const params = new URLSearchParams({ q });
  if (opts.project) params.set("project", opts.project);
  if (opts.limit !== undefined) params.set("limit", String(opts.limit));
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/recollect?${params.toString()}`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<RecollectResponse>(r, { sessions: [], ms: 0 });
}

// P8 — materialise a thread into an editable kb-list/1 list.
// CT-E5 — `narrative` asks the daemon to expand each session into its story
// (capture → files touched → memories produced → memories recalled) instead
// of one entry per transcript, and to write the ordering contract + session
// ids/dates (+ the kb-code session-diff link, when the kb has a `code_url`)
// into the list description. Omitted/false = the unchanged flat shape.
export async function saveThreadAsList(
  kb: string,
  title: string,
  artifactIds: string[],
  narrative = false,
): Promise<SaveThreadResponse | null> {
  const r = await fetch(`${currentDaemonBase()}/api/sessions/threads/save`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({ kb, title, artifact_ids: artifactIds, narrative }),
  });
  if (!r.ok) {
    console.warn(`[sessions] save thread: ${r.status}`);
    return null;
  }
  return (await r.json()) as SaveThreadResponse;
}

// P7 — narrative threads: sessions clustered into continued efforts.
export async function fetchSessionThreads(
  signal?: AbortSignal,
): Promise<ThreadOut[]> {
  const r = await fetch(`${currentDaemonBase()}/api/sessions/threads`, {
    headers: { Accept: "application/json" },
    signal,
  });
  const body = await jsonOrEmpty<ThreadsResponse>(r, { threads: [] });
  return body.threads;
}

// A1 — the folder facet: distinct working directories with counts.
export async function fetchSessionFolders(
  signal?: AbortSignal,
): Promise<FolderOut[]> {
  const r = await fetch(`${currentDaemonBase()}/api/sessions/folders`, {
    headers: { Accept: "application/json" },
    signal,
  });
  const body = await jsonOrEmpty<SessionFoldersResponse>(r, { folders: [] });
  return body.folders;
}

const EMPTY_FUNNEL: FunnelResponse = { stages: [] };

// A-w2/R9 — the activity funnel (searched → opened → edited → committed →
// commented), overall or scoped to one project folder. Deterministic +
// LLM-free (#10); federated cross-corpus sum (#28).
export async function fetchSessionFunnel(
  folder?: string,
  signal?: AbortSignal,
  project?: string,
): Promise<FunnelResponse> {
  const params = new URLSearchParams();
  if (folder) params.set("folder", folder);
  if (project) params.set("project", project);
  const qs = params.toString();
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/funnel${qs ? `?${qs}` : ""}`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<FunnelResponse>(r, EMPTY_FUNNEL);
}

// A-w2/R9 — the top research queries per project folder ("what has this
// project been researching"), merged cross-corpus. `folder` accepts the
// full cwd or its basename, matching the sessions-list folder facet; `limit`
// mirrors the server's per-folder top-N (default 5, capped at 50) — a caller
// only after a rollup-wide count (not the top-N display itself) should pass
// the max so it's a closer approximation of "distinct topics", not just the
// top 5 per folder.
export async function fetchResearchRollup(
  folder?: string,
  limit?: number,
  signal?: AbortSignal,
  project?: string,
): Promise<ResearchRollupFolderOut[]> {
  const params = new URLSearchParams();
  if (folder) params.set("folder", folder);
  if (limit !== undefined) params.set("limit", String(limit));
  if (project) params.set("project", project);
  const qs = params.toString();
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/research-rollup${qs ? `?${qs}` : ""}`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<ResearchRollupResponse>(r, { folders: [] });
  return body.folders;
}

const EMPTY_LEDGER: LedgerResponse = {
  days: 0,
  days_out: [],
  totals: { sessions: 0, commits: 0, decisions: 0, active_secs: 0 },
};

/// W6 (moonshots M4) — a project's sessions/commits/decisions/research
/// grouped by UTC day over a trailing window. `project` absent = every
/// project in the window (same "compose, don't require" posture as
/// `fetchSessionFunnel`/`fetchResearchRollup`).
export async function fetchSessionLedger(
  project?: string,
  days?: number,
  signal?: AbortSignal,
): Promise<LedgerResponse> {
  const params = new URLSearchParams();
  if (project) params.set("project", project);
  if (days !== undefined) params.set("days", String(days));
  const qs = params.toString();
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/ledger${qs ? `?${qs}` : ""}`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<LedgerResponse>(r, EMPTY_LEDGER);
}

// A4/A6 — the per-session file-activity manifest (read/edit/write, in-corpus
// links + out-of-corpus plain paths).
export async function fetchSessionFiles(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionFileOut[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/files`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<SessionFilesResponse>(r, { files: [] });
  return body.files;
}

// S9 — the per-session decisions log (steering moments).
export async function fetchSessionDecisions(
  sessionId: string,
  signal?: AbortSignal,
): Promise<DecisionOut[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/decisions`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<SessionDecisionsResponse>(r, { decisions: [] });
  return body.decisions;
}

// P5 — the git actions a session produced.
export async function fetchSessionCommits(
  sessionId: string,
  signal?: AbortSignal,
): Promise<CommitOut[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/commits`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<SessionCommitsResponse>(r, { commits: [] });
  return body.commits;
}

// MI-W4.6 — the provenance thread's 3rd hop: which files `sha` (one of
// `sessionId`'s commits) touched, and whether each has changed again since.
// On-demand (only fetched once a commit row is expanded) — never part of
// `useSessionDetail`'s composite fetch.
const COMMIT_FILES_UNAVAILABLE: CommitFilesResponse = {
  available: false,
  files: [],
  truncated: false,
};

export async function fetchCommitFiles(
  sessionId: string,
  sha: string,
  signal?: AbortSignal,
): Promise<CommitFilesResponse> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/commits/${encodeURIComponent(sha)}/files`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<CommitFilesResponse>(r, COMMIT_FILES_UNAVAILABLE);
}

// A7 — the reverse link: which sessions touched this artifact.
export async function fetchArtifactSessions(
  kb: string,
  artifactId: string,
  signal?: AbortSignal,
): Promise<ArtifactSessionOut[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/artifacts/${encodeURIComponent(kb)}/${encodeURIComponent(artifactId)}/sessions`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<ArtifactSessionsResponse>(r, { sessions: [] });
  return body.sessions;
}

export async function fetchSession(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionDetail | null> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}`,
    { headers: { Accept: "application/json" }, signal },
  );
  if (r.status === 404) return null;
  if (!r.ok) {
    console.warn(`[sessions] detail ${sessionId}: ${r.status}`);
    return null;
  }
  return (await r.json()) as SessionDetail;
}

const EMPTY_WHY: WhyResponse = { path: "", basename: "", sessions: [] };

// R2 — why is this file the way it is: the touching sessions + their reasoning.
export async function fetchWhy(
  path: string,
  signal?: AbortSignal,
): Promise<WhyResponse> {
  const r = await fetch(
    `${currentDaemonBase()}/api/why?path=${encodeURIComponent(path)}`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<WhyResponse>(r, EMPTY_WHY);
}

// R4 — the research / tool-usage signals a session produced.
export async function fetchSessionResearch(
  sessionId: string,
  signal?: AbortSignal,
): Promise<ResearchOut[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/research`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<SessionResearchResponse>(r, { research: [] });
  return body.research;
}

const EMPTY_COMMENTS: SessionCommentsResponse = {
  artifacts: [],
  total: 0,
  raised: [],
};

// R5/R8 — open comments on touched artifacts + comments raised in the window.
export async function fetchSessionComments(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionCommentsResponse> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/comments`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<SessionCommentsResponse>(r, EMPTY_COMMENTS);
}

export async function fetchSessionMemories(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionMemoryHit[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/memories`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<SessionMemoriesResponse>(r, {
    memories: [],
  });
  return body.memories;
}

const EMPTY_RECALLS: SessionRecallsResponse = { recalls: [] };

/// MI-W4.2c — the PULL side of `fetchSessionMemories`' WRITE side: every
/// memory this session's `kb-recall` hook actually injected.
export async function fetchSessionRecalls(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionRecallHit[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/recalls`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<SessionRecallsResponse>(r, EMPTY_RECALLS);
  return body.recalls;
}

export async function fetchSessionTouches(
  sessionId: string,
  signal?: AbortSignal,
): Promise<TouchesResponse> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/touches`,
    { headers: { Accept: "application/json" }, signal },
  );
  return jsonOrEmpty<TouchesResponse>(r, {
    artifact_ids: [],
    confidence: "exact",
    artifacts: [],
  });
}

import type { SessionReadingRow } from "./generated/SessionReadingRow";
import type { SessionReadingsResponse } from "./generated/SessionReadingsResponse";
export type { SessionReadingRow, SessionReadingsResponse };

/// RP-track — what the human READ in the SPA during the session window (the
/// read-counterpart to /touches, which is what the agent referenced in the
/// transcript). Best-effort: 404 / bad JSON → [].
export async function fetchSessionReadings(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionReadingRow[]> {
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/readings`,
    { headers: { Accept: "application/json" }, signal },
  );
  const body = await jsonOrEmpty<SessionReadingsResponse>(r, { readings: [] });
  return body.readings;
}

// W7 (sessions-rethink R15/LF-1/LF-3b) — live-follow. Both routes are
// loopback-only HARD server-side (LF-5) — a non-loopback daemon simply
// answers `{enabled:false}`/403, which the callers below treat as "Tier-1
// unavailable here" rather than an error (never surfaced as a hard UI
// failure, same posture as every other best-effort fetcher in this file).

import type { PresenceResponse } from "./generated/PresenceResponse";
import type { LiveDeltaResponse } from "./generated/LiveDeltaResponse";
export type { PresenceResponse, LiveDeltaResponse };
export type { PresenceEntry } from "./generated/PresenceEntry";

const EMPTY_PRESENCE: PresenceResponse = { enabled: false, live: [] };

/// LF-1 — the stat-only Tier-1 presence probe. Polled 30s, visibility-gated,
/// from `/sessions` and an open session page ONLY (`useSessionPresence`,
/// the #23 exception ledger's `ambient` precedent). A non-loopback/
/// unconfigured daemon degrades to `{enabled:false, live:[]}` — never
/// thrown, never logged as a warning (this is the EXPECTED shape for prod).
export async function fetchSessionPresence(
  signal?: AbortSignal,
): Promise<PresenceResponse> {
  const r = await fetch(`${currentDaemonBase()}/api/sessions/presence`, {
    headers: { Accept: "application/json" },
    signal,
  });
  return jsonOrEmpty<PresenceResponse>(r, EMPTY_PRESENCE);
}

// LSC-4 — the live-sessions cockpit read. Distinct from the LF-1/Tier-1
// `presence`/`live` pair above: `live-status` is `auth_bearer` (NOT
// loopback-only — design §6's documented graduation for METADATA, never raw
// transcript bytes), fans out over `state.kbs` server-side (invariant #28),
// and merges LSC-2's in-memory beat registry with a Tier-0 capture-derived
// fallback so a freshly-restarted daemon is honest rather than empty. A
// daemon miss/error degrades to an empty list — same best-effort posture as
// every other list fetcher in this file, never a hard UI failure.

import type { LiveStatusResponse } from "./generated/LiveStatusResponse";
import type { LiveStatusRow } from "./generated/LiveStatusRow";
export type { LiveStatusResponse, LiveStatusRow };

const EMPTY_LIVE_STATUS: LiveStatusResponse = { sessions: [] };

export async function fetchLiveStatus(
  signal?: AbortSignal,
): Promise<LiveStatusRow[]> {
  const r = await fetch(`${currentDaemonBase()}/api/sessions/live-status`, {
    headers: { Accept: "application/json" },
    signal,
  });
  const body = await jsonOrEmpty<LiveStatusResponse>(r, EMPTY_LIVE_STATUS);
  return body.sessions;
}

/// LF-3b — one delta poll against the live-tail route. `raw=true` requests
/// decoded JSONL lines instead of interpreted `session-view/1` events.
/// Callers loop (`from = next_from`) until `next_from === size`. Throws on
/// a non-2xx/network failure — unlike the other best-effort fetchers in
/// this file, `useLiveTail` (the sole caller) needs to distinguish
/// "nothing new" from "the daemon can't serve this at all" so it can stop
/// polling + surface an honest state rather than retry forever.
export async function fetchLiveDelta(
  sessionId: string,
  from: number,
  raw: boolean,
  signal?: AbortSignal,
): Promise<LiveDeltaResponse> {
  const params = new URLSearchParams({ from: String(from) });
  if (raw) params.set("raw", "1");
  const r = await fetch(
    `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/live?${params.toString()}`,
    { headers: { Accept: "application/json" }, signal },
  );
  if (!r.ok) {
    throw new Error(`live delta fetch failed: ${r.status}`);
  }
  return (await r.json()) as LiveDeltaResponse;
}
