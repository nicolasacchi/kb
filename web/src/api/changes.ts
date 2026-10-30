// OL IDEA5 — client for GET /api/changes, the project axis of "since you
// left" (sessions + commits + slate posts, grouped by project). Best-effort,
// A daemon hiccup throws; the strip's useQuery holds it as an ERROR (never
// cached as data, so it refetches on remount) and renders nothing, so a
// transient miss can never hard-fail the Front Page band that hosts the strip.

import { currentDaemonBase } from "./base";
// Wire types are the generated ts-rs bindings (routes/changes.rs is the
// source of truth; `just types` regenerates).
import type { ChangesCommit } from "./generated/ChangesCommit";
import type { ChangesProject } from "./generated/ChangesProject";
import type { ChangesResponse } from "./generated/ChangesResponse";
import type { ChangesSession } from "./generated/ChangesSession";
import type { ChangesSlate } from "./generated/ChangesSlate";
import type { ChangesSlatePost } from "./generated/ChangesSlatePost";

export type {
  ChangesCommit,
  ChangesProject,
  ChangesResponse,
  ChangesSession,
  ChangesSlate,
  ChangesSlatePost,
};

export async function fetchChanges(
  opts: { since: number },
  signal?: AbortSignal,
): Promise<ChangesResponse> {
  const params = new URLSearchParams({ since: String(opts.since) });
  let r: Response;
  try {
    r = await fetch(`${currentDaemonBase()}/api/changes?${params.toString()}`, {
      headers: { Accept: "application/json" },
      signal,
    });
  } catch (e) {
    if (signal?.aborted) throw e;
    console.warn("[changes] fetch failed", e);
    throw e;
  }
  if (!r.ok) {
    console.warn(`[changes] ${r.url}: ${r.status} ${r.statusText}`);
    throw new Error(`changes: ${r.status} ${r.statusText}`);
  }
  return (await r.json()) as ChangesResponse;
}
