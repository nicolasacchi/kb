// Z4 — client for GET /api/inbox (fleet-wide open-comments inbox). The
// /inbox route consumes `items`; the Header badge reads `total_open` off
// the SAME ["inbox"] query. Best-effort: a daemon hiccup logs + returns an
// empty payload so a transient miss never hard-fails the badge/view.

import { currentDaemonBase } from "./base";
// Wire types are the generated ts-rs bindings (routes/inbox.rs is the
// source of truth; `just types` regenerates).
import type { InboxItem } from "./generated/InboxItem";
import type { InboxResponse } from "./generated/InboxResponse";

export type { InboxItem, InboxResponse };

const EMPTY: InboxResponse = { items: [], total_open: 0 };

export async function fetchInbox(
  opts: { kb?: string; limit?: number } = {},
  signal?: AbortSignal,
): Promise<InboxResponse> {
  const params = new URLSearchParams();
  if (opts.kb) params.set("kb", opts.kb);
  if (opts.limit !== undefined) params.set("limit", String(opts.limit));
  const qs = params.toString();
  let r: Response;
  try {
    r = await fetch(`${currentDaemonBase()}/api/inbox${qs ? `?${qs}` : ""}`, {
      headers: { Accept: "application/json" },
      signal,
    });
  } catch (e) {
    if (signal?.aborted) throw e;
    console.warn("[inbox] fetch failed", e);
    return EMPTY;
  }
  if (!r.ok) {
    console.warn(`[inbox] ${r.url}: ${r.status} ${r.statusText}`);
    return EMPTY;
  }
  try {
    return (await r.json()) as InboxResponse;
  } catch (e) {
    console.warn(`[inbox] ${r.url}: bad JSON`, e);
    return EMPTY;
  }
}
