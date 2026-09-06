// v0.6+ H4 — client for the /api/kb/{kb}/history endpoints.
//
// All calls are best-effort: history is observational, not load-bearing
// for any feature except the gallery timeline view (H5). Errors are
// swallowed and logged so a transient daemon hiccup doesn't surface as
// a user-visible failure inside detail.tsx / Cmdk.

type Problem = {
  type: string;
  title: string;
  status: number;
  detail?: string;
};

// D7-prep — see `api/base.ts`. Read on every call so a runtime
// daemon switch takes effect immediately.
import { currentDaemonBase } from "./base";

async function jsonOrThrow<T>(r: Response): Promise<T> {
  if (!r.ok) {
    const ct = r.headers.get("content-type") ?? "";
    if (ct.includes("application/problem+json")) {
      const p = (await r.json()) as Problem;
      throw new Error(`${p.title}: ${p.detail ?? ""}`);
    }
    throw new Error(`${r.status} ${r.statusText}`);
  }
  return (await r.json()) as T;
}

// Wire types are the generated ts-rs bindings (routes/history.rs is
// the source of truth; `just types` regenerates).
import type { ReadingResumeSection } from "./generated/ReadingResumeSection";
import type { ReadingResumeState } from "./generated/ReadingResumeState";
import type { HistoryOpenResponse } from "./generated/HistoryOpenResponse";
import type { ReadingSectionBeacon } from "./generated/ReadingSectionBeacon";
import type { HistoryEntry } from "./generated/HistoryEntry";
import type { HistoryListResponse } from "./generated/HistoryListResponse";

export type { ReadingResumeSection, ReadingSectionBeacon, HistoryEntry };
/// RP-track seed-on-open: the visit's prior cumulative reading state, echoed
/// by /history/open so the iframe runtime resumes its counters within the
/// 30-min visit window (the server max-merges, so the client value must
/// never regress).
export type ReadingResume = ReadingResumeState;
export type OpenResponse = HistoryOpenResponse;
export type HistoryKind = HistoryEntry["kind"];

/// POST /api/kb/{kb}/history/open — register a visit. Returns the
/// visit_id (used by recordScroll) and the prior scroll_y to resume at.
export async function recordOpen(
  kb: string,
  artifactId: string,
): Promise<OpenResponse> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/history/open`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ artifact_id: artifactId }),
    },
  );
  return jsonOrThrow<OpenResponse>(r);
}

/// POST /api/kb/{kb}/history/scroll — UPDATE scroll position on an
/// open visit. Returns true on success, false if the visit_id is no
/// longer recognised (caller should re-open).
export async function recordScroll(
  kb: string,
  visitId: number,
  scrollY: number,
  scrollMax: number,
): Promise<boolean> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/history/scroll`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        visit_id: visitId,
        scroll_y: scrollY,
        scroll_max: scrollMax,
      }),
    },
  );
  if (r.status === 404) return false;
  if (!r.ok) {
    // Other errors (400/500) — log and swallow.
    console.warn(`[history] scroll POST: ${r.status} ${r.statusText}`);
    return false;
  }
  return true;
}

/// RP-track per-section reading beacon — one CUMULATIVE snapshot of the
/// visit's dwell/enters + active time + current section. The server
/// max-merges so resends are idempotent. Best-effort like scroll.
export async function recordReading(
  kb: string,
  visitId: number,
  artifactId: string,
  sections: ReadingSectionBeacon[],
  activeMs: number,
  lastSection: string | null,
): Promise<boolean> {
  try {
    const r = await fetch(
      `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/history/reading`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          visit_id: visitId,
          artifact_id: artifactId,
          sections,
          active_ms: activeMs,
          last_section: lastSection,
        }),
      },
    );
    return r.ok;
  } catch (e) {
    console.warn("[history] reading POST failed", e);
    return false;
  }
}

/// POST /api/kb/{kb}/history/search — record a search query.
export async function recordSearch(kb: string, query: string): Promise<void> {
  if (!query.trim()) return;
  try {
    await fetch(
      `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/history/search`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ query }),
      },
    );
  } catch (e) {
    console.warn("[history] search POST failed", e);
  }
}

/// GET /api/kb/{kb}/history — newest-first list, filtered by kind and
/// optionally before a started_at cursor (for pagination).
export async function fetchHistory(
  kb: string,
  opts: {
    limit?: number;
    before?: number;
    kind?: HistoryKind | "all";
    signal?: AbortSignal;
  } = {},
): Promise<HistoryEntry[]> {
  const params = new URLSearchParams();
  if (opts.limit !== undefined) params.set("limit", String(opts.limit));
  if (opts.before !== undefined) params.set("before", String(opts.before));
  if (opts.kind && opts.kind !== "all") params.set("kind", opts.kind);
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/history?${params.toString()}`,
    { headers: { Accept: "application/json" }, signal: opts.signal },
  );
  const body = await jsonOrThrow<HistoryListResponse>(r);
  return body.entries;
}
