// OL IDEA5 — client for GET /api/changes, the project axis of "since you
// left" (sessions + commits + slate posts, grouped by project). Best-effort,
// the inbox.ts shape: a daemon hiccup logs and returns null so a transient
// miss can never hard-fail the Front Page band that hosts the strip.

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
): Promise<ChangesResponse | null> {
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
    return null;
  }
  if (!r.ok) {
    console.warn(`[changes] ${r.url}: ${r.status} ${r.statusText}`);
    return null;
  }
  try {
    return (await r.json()) as ChangesResponse;
  } catch (e) {
    console.warn(`[changes] ${r.url}: bad JSON`, e);
    return null;
  }
}
