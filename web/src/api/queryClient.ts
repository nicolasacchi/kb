// TanStack Query foundation (server-state migration, post-v0.17).
//
// Cache philosophy: this SPA is SSE-driven — the daemon announces every
// server-state change (artifact.indexed, comments.updated, note.*, …)
// and the Last-Event-ID resume + gap/resync path covers lost windows.
// Data is therefore fresh until an event says otherwise:
//
//   staleTime: Infinity     no time-based refetching, ever
//   refetchOnWindowFocus /  off — SSE owns invalidation ("reconnect"
//     refetchOnReconnect    here is the BROWSER network event; the SSE
//                           drop path is sse.ts's gap → resync)
//   retry once, 5xx only    a 4xx ApiError stays bad; one quick retry
//                           absorbs daemon-restart blips
//
// The bridge below maps SSE events onto query invalidations in ONE
// place — it replaces the per-hook subscribe-and-refetch wiring as
// hooks migrate onto useQuery. Invalidating a key nobody holds is a
// no-op, so the bridge is safe to run from day one.

import { notifyManager, QueryClient } from "@tanstack/react-query";
import { ApiError } from "./client";
import { sse } from "./sse";

// Flush cache notifications SYNCHRONOUSLY instead of the default
// microtask deferral. An optimistic setQueryData inside a click handler
// must re-render within that event's React batch — exactly like the
// setState it replaced — or controlled inputs flicker through a
// stale-DOM window (react-markdown's checkbox re-create made this
// observable: the clicked node was replaced a microtask later and
// probes caught the old, un-flipped one). React 18 auto-batches sync
// setState everywhere, so this costs no extra renders.
notifyManager.setScheduler((cb) => cb());

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: Infinity,
      refetchOnWindowFocus: false,
      refetchOnReconnect: false,
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status < 500) return false;
        return failureCount < 1;
      },
    },
  },
});

// --- query-key conventions --------------------------------------------
//
// The contract every migrated hook follows (invalidation matches by
// key PREFIX, so ["docs", kb] hits every paged/filtered variant):
//
//   ["docs", kb, ...params]        gallery / folder row sets
//   ["doc", kb, relPath]           single artifact by path
//   ["doc", kb, "by-id", id]       link-flow — an artifact resolved by ID
//                                  (`api/artifactLookup.ts`: an artifact-
//                                  subdomain link names an id but no kb, so
//                                  the fetcher walks pane-kb-first and
//                                  returns `{kb, doc}`). Deliberately under
//                                  the SAME ["doc", kb] prefix, so the
//                                  existing artifact.* invalidation covers it
//                                  with no new plumbing.
//   ["review", kb, artifactId]     kb-comments document
//   ["edges", kb]                  corpus-wide link-edge list (reader rail)
//   ["proposals", ...params]       W2.15b — the tribal-knowledge proposal
//                                  inbox (`GET /api/proposals?kb=`), fleet-
//                                  wide unless scoped. Bridged directly
//                                  (not burst-gated — proposal.* is a
//                                  one-write-per-human-action rate, same
//                                  shape as board.updated below)
//   ["inbox"]                      fleet-wide open-comments inbox (Z4)
//   ["inbox", kb]                  W2.5 — kb-scoped comment-mass aggregation
//                                  for the broadsheet view (`fetchInbox({kb})`,
//                                  Broadsheet.tsx); a DISTINCT cache entry from
//                                  the fleet-wide ["inbox"] above (same
//                                  fetcher, kb-scoped call), but TanStack
//                                  invalidates by key PREFIX, so the existing
//                                  `comments.updated` burst-gated
//                                  invalidate(["inbox"]) below already
//                                  refreshes this variant too — no bridge
//                                  change needed
//   ["notes"]                      notes store (daemon-wide)
//   ["lists"]                      reading-list index (cross-kb)
//   ["list", kb, listId]           one list's detail (ordered entries)
//   ["board", kb, listId]          W2.4 — one list's JSON Canvas geometry
//                                  sidecar (`GET /api/kb/{kb}/boards/
//                                  {listId}/canvas`). Bridged directly
//                                  (not burst-gated — one `board.updated`
//                                  per debounced drag-end PUT, already
//                                  throttled client-side)
//   ["anchors"]                    corkboard (daemon-wide)
//   ["memories", ...params]        recall results
//   ["memories", "from", kb, id]   CT-A1 (U3 parse-back) — every memory
//                                  highlighted FROM artifact {kb}/{id}
//                                  (`GET …/docs/{id}/memories-from`,
//                                  `hooks/useMemoriesFrom.ts`). Nested under
//                                  the ["memories"] prefix so a NEW
//                                  highlight (memory.ingested, already
//                                  bridged below) refreshes this for free —
//                                  no bridge change needed.
//   ["sessions", ...params]        session list
//   ["sessions", "funnel", folder?] activity-funnel strip (A-w2/R9) — plain
//                                   useQuery, no bridge wiring (nothing SSE
//                                   marks this stale; a page revisit refetches)
//   ["sessions", "research-rollup", folder?, limit?] research-rollup strip
//                                   (A-w2/R9) — same plain-useQuery shape as
//                                   the funnel above
//   ["sessions", "replay", sid, artifactId] W3.R-c — one captured session's
//                                   `session-replay/1` timeline
//                                   (`GET /api/sessions/{sid}/replay`,
//                                   `hooks/useSessionReplay.ts`). Plain
//                                   useQuery, staleTime Infinity, NO SSE
//                                   subscription of its own (#24): the
//                                   replay reader is pull-only (the operator
//                                   scrubs; there is no autoplay/timer), and
//                                   a finished session's transcript is
//                                   frozen. Deliberately nested under the
//                                   ["sessions"] prefix so `useSessions`'s
//                                   own session.captured/session.deleted
//                                   invalidation (which owns that prefix —
//                                   see "Deliberately NOT bridged" below)
//                                   refreshes a RE-captured session's replay
//                                   for free. The artifactId slot is the
//                                   server-side `?artifact=` window, a
//                                   genuinely different response, hence its
//                                   own entry
//   ["sessions", "live"]            LSC-4 — the live-sessions cockpit read
//                                   (`GET /api/sessions/live-status`,
//                                   `hooks/useLiveSessions.ts`, the FIFTH
//                                   documented #23 exception). Finite
//                                   staleTime + a ~10s refetchInterval (an
//                                   elapsed-timer display is a CLIENT
//                                   concern, `useNowTick` — never a reason to
//                                   poll faster) PLUS a direct invalidation
//                                   on `session.state` below: that kind
//                                   fires ONLY on an actual derived-state
//                                   TRANSITION (never per beat — LSC-2's
//                                   server doc), so it's exactly as low-rate
//                                   as board.updated/proposal.* above and
//                                   needs no burst gate. Nested under the
//                                   ["sessions"] prefix on purpose: it also
//                                   rides useSessions' own
//                                   session.captured/session.deleted
//                                   invalidation (a session ending IS a
//                                   state transition), so this bridge entry
//                                   only has to add session.state.
//   ["readingProgress", kb]        per-kb progress map
//   ["calendar", kb]               W2.10 — per-UTC-day event density for the
//                                  gallery's activity calendar
//                                  (`fetchCalendar`, `GET .../history/
//                                  calendar`). Bridged directly (not
//                                  burst-gated) on `history.recorded` below,
//                                  same non-gated shape as the
//                                  readingProgress/resurface/lists
//                                  invalidations it sits beside — a fresh
//                                  visit/search/comment shifts today's
//                                  bucket, and `history.recorded` itself
//                                  only fires on an actual INSERT (#8), so
//                                  it's already not a bursty event stream.
//   ["timeline", kb, from, to]     W3.C-b — the reflection canvas's four
//                                  day-bucketed lanes (`GET /api/kb/{kb}/
//                                  timeline?from=&to=`, `fetchTimeline`,
//                                  `components/ReflectionCanvas.tsx`).
//                                  staleTime Infinity, window PINNED at
//                                  mount (an unpinned trailing `to` would
//                                  change the key every render tick — the
//                                  same rule ["calendar", kb] follows). The
//                                  from/to slots are part of the key because
//                                  the wire resolves each lane's artifact-id
//                                  set for the REQUESTED WINDOW: the brush
//                                  is pure client state over already-fetched
//                                  buckets (dragging refetches NOTHING), and
//                                  only *activating* the brush→gallery pivot
//                                  fetches the brushed sub-window — a cache
//                                  HIT when the brush is the whole window,
//                                  since both bounds snap to whole UTC days.
//                                  Bridged by PREFIX (["timeline", kb]) on
//                                  the three handlers below that can move a
//                                  lane: history.recorded (read + comment),
//                                  artifact churn (created), session.captured
//                                  (session). NO SSE subscription of its own
//                                  (#23/#24)
//   ["daycard", kb]                 Unit 2 — the desk radiator (`/ambient`,
//                                  `GET /api/kb/{kb}/daycard?format=json`,
//                                  `fetchDaycard`). A 4th documented
//                                  exception to the SSE-bridge rule (see
//                                  "Deliberately NOT bridged" below): this
//                                  route wants a slow wall-clock refresh
//                                  (`refetchInterval`), not SSE-precise
//                                  invalidation — it's an explicitly ambient
//                                  display, not a live collaboration feed.
//   ["desk"] / ["desk", kb]        kb desk v2 — GET /api/desk (fleet-wide,
//                                  or ?kb= for one corpus). Joins the
//                                  documented no-SSE-tie set (daycard /
//                                  live tail / presence / doclens): finite
//                                  staleTime (60s) + refetchOnWindowFocus,
//                                  NO SSE wiring. See "Deliberately NOT
//                                  bridged" below and hooks/useDesk.ts.

//   ["staleAnchors"]               stale-anchor dashboard rows
//   ["exclusions", kb]             per-file exclusion rows (v0.24 X4)
//   ["search", ...args]            full search-page results (Track F)
//   ["search", "probe", ...]       W1.search — bounded zero-hit-recovery
//                                  probes (other mode / scope=all /
//                                  filters-cleared); nested under the
//                                  ["search"] prefix so artifact churn
//                                  invalidates them for free, no bridge
//                                  change needed
//   ["zeroHitQueries", kb, limit]  W1.search — per-kb zero-hit query ring
//                                  (volatile, resets per daemon boot —
//                                  staleTime 0, not bridged; same shape
//                                  as the pre-existing ["queries", kb, n])
//   ["zeroHitQueries", "all", limit] W1.pulse — cross-kb fan-out twin
//                                  (`GET /api/queries/zero-hit`), backing
//                                  the census panel's fleet-wide list.
//                                  Same volatile/staleTime-0/not-bridged
//                                  shape as the per-kb variant above — the
//                                  literal "all" in the kb slot keeps it a
//                                  distinct cache entry.
//   ["stats", kb]                  W1.pulse — per-kb doc-count/error stats
//                                  (`GET /api/kb/{kb}/stats`) for the
//                                  vital-signs strip. Plain useQuery,
//                                  staleTime 0 (background data — cheap to
//                                  refetch on mount, not worth a bridge
//                                  wire for a quiet count that's wrong for
//                                  at most one visit)
//   ["atlasLabels", kb]            c-TF-IDF cluster labels (W1.atlas) —
//                                  bridged on atlas.recompute.complete /
//                                  atlas.recluster.complete below
//   ["atlasPoints", kb]            W3.M-c — the FULL-corpus atlas point set
//                                  (`GET /api/kb/{kb}/atlas/points`,
//                                  `useAtlasPoints`), the fix for the atlas
//                                  view silently drawing only the gallery's
//                                  first paged `useDocs` fetch. Bridged on
//                                  the same atlas.recompute.complete /
//                                  atlas.recluster.complete pair as
//                                  ["atlasLabels", kb] above — both change
//                                  together (coords/cluster rewrite).
//   ["atlasHistory", kb]           W3.T-c — the atlas time-lapse's FRAME LIST
//                                  (`GET /api/kb/{kb}/atlas/history`,
//                                  `useAtlasHistory`), newest first, metadata
//                                  only. Bridged on `atlas.snapshot.recorded`
//                                  below (the one event that appends a frame).
//                                  Lazy: AtlasView passes `undefined` until
//                                  the operator opens the scrubber, so a plain
//                                  atlas visit never fetches it.
//   ["atlasFrame", kb, id]         W3.T-c — ONE frame's points, already
//                                  Procrustes-aligned SERVER-SIDE against the
//                                  newest frame (`GET .../atlas/history/{id}`,
//                                  `useAtlasFrame`). Fetched only once a frame
//                                  is actually scrubbed to; a stored frame's
//                                  own points never change, so the entry is
//                                  immutable in practice and a played-through
//                                  time-lapse replays out of cache. Bridged on
//                                  the same `atlas.snapshot.recorded` as the
//                                  list above — NOT for its own sake but
//                                  because a new frame changes the DEFAULT
//                                  align_to (the newest frame), which is baked
//                                  into every cached frame's coordinates.
//   ["atlasField", kb]             W3.F-c — the OPERATOR FIELD: the raw JSON
//                                  Canvas sidecar a hand placed (`GET
//                                  /api/kb/{kb}/atlas/field`,
//                                  `hooks/useAtlasField.ts`). Bridged on
//                                  `atlas.field.updated` below (the daemon
//                                  emits it on every PUT — including this
//                                  SPA's own debounced drag-end write, so a
//                                  second tab/CLI edit converges without a
//                                  poll). Lazy: AtlasView passes `undefined`
//                                  until the onion-skin overlay is switched
//                                  on, so a plain atlas visit costs nothing.
//   ["atlasField", kb, "disagreement"] W3.F-c — the same field JOINED against
//                                  the machine layout: per-doc displacement,
//                                  Procrustes-aligned SERVER-SIDE (`GET
//                                  .../atlas/field/disagreement`). Nested
//                                  under the sidecar's key ON PURPOSE — every
//                                  invalidation that moves EITHER half of the
//                                  comparison is a prefix match on
//                                  ["atlasField", kb]: `atlas.field.updated`
//                                  (the operator half) and the
//                                  atlas.recompute/recluster pair below (the
//                                  machine half — new coords mean a new fit).
//                                  The SPA must never re-run that alignment
//                                  itself; the daemon owns it so `kb atlas
//                                  field diff` and the map agree exactly.
//   ["atlasSimilar", kb, id]       true (embedding-space) neighbors for one
//                                  atlas dot (W2.3b true-neighbors rail,
//                                  `GET /api/kb/{kb}/atlas/similar/{id}`).
//                                  Plain useQuery, NOT bridged: there is no
//                                  per-artifact "embedding changed" event,
//                                  and the same atlas.recompute.complete
//                                  that would invalidate it already forces
//                                  AtlasView's docs refetch (which remounts
//                                  the inspector's selection) — a page
//                                  revisit or new selection naturally
//                                  refetches this key.
//   ["echoes", kb]                 on-this-day + session-echoes strip
//                                  (W2.2, `GET /api/kb/{kb}/echoes`) — plain
//                                  useQuery, NO bridge wiring: nothing SSE
//                                  marks an anniversary stale intra-day (the
//                                  date join is a pure function of the
//                                  clock), so a page revisit or the next
//                                  natural remount recomputes it for free —
//                                  same non-bridged shape as the
//                                  funnel/research-rollup entries above
//   ["slates"]                     SL4 — GET /api/slates: every slate's slug,
//                                  head seq, topics and per-section counts.
//                                  ONE entry shared by the /slates list and
//                                  the nav attention chip (the Z4 ["inbox"]
//                                  shape). staleTime Infinity; bridged on
//                                  slate.updated / slate.deleted below.
//   ["slate", slug]                SL4 — GET …/{slug}?view=board, the board
//                                  projection. ["slate", slug, "topic", t] is
//                                  the ?topic= filtered variant (its own
//                                  segment, so a topic named "history" cannot
//                                  collide with the key below) and rides the
//                                  same prefix.
//   ["slate", slug, "history"]     SL4 — GET …/{slug}/history, the drawer's
//                                  dropped/superseded rows. `enabled` only
//                                  while the drawer is open; a strict
//                                  extension of the key above, so one
//                                  ["slate", slug] invalidation reaches it.
//   ["slateGrounding", kb, path, line]
//                                  SL7d/D29 — kb-code's codelens answer for
//                                  ONE `path:` ref on a found/tried card
//                                  (`useSlateGrounding`). DELIBERATELY NOT a
//                                  ["slate", …] extension: it is a
//                                  cross-daemon read that a slate append
//                                  cannot change, so a slate.updated
//                                  invalidation must not refetch it. Joins
//                                  the doclens no-SSE-tie set below (30s
//                                  staleTime, retry: false, nothing
//                                  persisted); fetched only when the active
//                                  kb has a code_url.
//   ["prompt", kb, id]             W2.11 — one artifact's stored generation
//                                  prompt (`GET /api/kb/{kb}/artifacts/{id}/
//                                  prompt`), staleTime Infinity. Plain
//                                  useQuery, NOT bridged: prompts only
//                                  change on reindex (the source HTML's
//                                  `<template id="kb-prompt">` was edited),
//                                  and the existing broad `artifact.indexed`
//                                  invalidation (below) already covers that
//                                  — no dedicated event, same reasoning as
//                                  `["atlasSimilar", kb, id]` above.

/// Leading-edge + trailing throttle, per key — preserves the gallery's
/// 1500 ms cooldown semantics for bursty event types (the daemon's
/// initial walk can flood artifact.indexed; one invalidation per window
/// per kb is plenty, with one trailing call so the last burst lands).
const BURST_COOLDOWN_MS = 1500;
function makeBurstGate(fire: (key: string) => void) {
  const timers = new Map<string, ReturnType<typeof setTimeout>>();
  const pending = new Set<string>();
  return (key: string) => {
    if (timers.has(key)) {
      pending.add(key);
      return;
    }
    fire(key);
    const tick = () => {
      if (pending.delete(key)) {
        fire(key);
        timers.set(key, setTimeout(tick, BURST_COOLDOWN_MS));
      } else {
        timers.delete(key);
      }
    };
    timers.set(key, setTimeout(tick, BURST_COOLDOWN_MS));
  };
}

/// Start the SSE → invalidation bridge. Idempotent by convention (call
/// once from main.tsx); returns a teardown for symmetry/HMR.
export function startSseInvalidationBridge(): () => void {
  const offs: Array<() => void> = [];
  const on = (
    type: string,
    fn: (payload: Record<string, unknown>) => void,
  ) => {
    offs.push(sse.subscribeEvent(type, fn));
  };
  const kbOf = (p: Record<string, unknown>): string | undefined =>
    typeof p.kb === "string" ? p.kb : undefined;
  const invalidate = (queryKey: readonly unknown[]) => {
    void queryClient.invalidateQueries({ queryKey });
  };

  // Artifact churn — bursty (initial walk, reindex): throttled per kb.
  const docsGate = makeBurstGate((kb) => {
    invalidate(kb ? ["docs", kb] : ["docs"]);
    invalidate(kb ? ["doc", kb] : ["doc"]);
    // FS3 — the tag facet list (gallery + search rail) shifts as artifacts
    // are (re)indexed; refresh it on the same burst-gated churn.
    invalidate(kb ? ["tags", kb] : ["tags"]);
    // v0.22 — the gallery left-rail category facet + the reader's
    // "Explore from here" chips read /facets; the rail's versions-count badge
    // reads the version timeline. Both shift on (re)index — refresh on the
    // same gate (there is no version.* event; artifact.indexed is the signal).
    invalidate(kb ? ["facets", kb] : ["facets"]);
    invalidate(kb ? ["folders", kb] : ["folders"]);
    invalidate(kb ? ["versions", kb] : ["versions"]);
    // The reader rail's Links badge/neighbor lists read the corpus-wide
    // edge list (["edges", kb]); links shift as artifacts are (re)indexed
    // and there is no dedicated edge.* event — refresh on the same churn.
    invalidate(kb ? ["edges", kb] : ["edges"]);
    // DCB W1.D — code refs shift as artifacts are (re)indexed; no
    // dedicated code_refs.* event (record_code_refs never bumps the
    // generation, amendment 2 — the churn signal is artifact.indexed
    // itself, same reasoning as ["edges", kb] above). Prefix match:
    // ["codeRefs", kb, docId] is invalidated by the ["codeRefs", kb]
    // prefix, same mechanism every other per-doc-under-per-kb key in this
    // bridge already relies on.
    invalidate(kb ? ["codeRefs", kb] : ["codeRefs"]);
    // Track F — the full search page's results may now be stale. Invalidate
    // the whole ["search"] prefix (every q/mode/scope/filter variant);
    // only the mounted query refetches, the rest are no-ops.
    invalidate(["search"]);
    // W3.C-b — the reflection canvas's `created` lane buckets on mtime, so
    // artifact churn moves it (prefix match hits every from/to window).
    invalidate(kb ? ["timeline", kb] : ["timeline"]);
  });
  // v0.24 X4 — artifact.excluded/included join the churn gate: the exclusion
  // cascade's artifact.removed / re-include's artifact.indexed ride the ingest
  // pipeline asynchronously, so the intent-level kinds ALSO kick the gallery
  // (belt-and-braces; the burst gate collapses the pair to one refetch).
  for (const t of [
    "artifact.indexed",
    "artifact.removed",
    "artifact.excluded",
    "artifact.included",
  ]) {
    on(t, (p) => docsGate(kbOf(p) ?? ""));
  }
  // The Settings → Excluded pane's row set — targeted, low-rate events, so a
  // direct (un-gated) invalidation keeps the pane snappy after each action.
  for (const t of ["artifact.excluded", "artifact.included"]) {
    on(t, (p) => {
      const kb = kbOf(p);
      invalidate(kb ? ["exclusions", kb] : ["exclusions"]);
    });
  }

  // Z4 — the fleet-wide inbox (Header badge + /inbox route) shares one
  // ["inbox"] query across every corpus, refreshed by a federated fan-out.
  // Comment mutations arrive one-per-HTTP-call (a triage session = up to 2N
  // events for N comments), so collapse a storm to one leading + one trailing
  // fan-out per window through the same burst gate the docs churn uses.
  const inboxGate = makeBurstGate(() => invalidate(["inbox"]));
  // Low-rate, targeted events: direct invalidation.
  on("comments.updated", (p) => {
    const kb = kbOf(p);
    const id = typeof p.artifact_id === "string" ? p.artifact_id : undefined;
    // The open document's own review data stays immediate (the reader is
    // watching it); only the fleet inbox aggregation is burst-gated.
    invalidate(kb && id ? ["review", kb, id] : ["review"]);
    inboxGate("inbox");
    // Resurface strip — open-comment counts are half its score.
    invalidate(kb ? ["resurface", kb] : ["resurface"]);
  });
  for (const t of ["note.created", "note.updated", "note.deleted"]) {
    on(t, () => invalidate(["notes"]));
  }
  for (const t of ["anchor.added", "anchor.removed"]) {
    on(t, () => invalidate(["anchors"]));
  }
  // RL-track — every list.* / list.entry.* kind refreshes the index and
  // the targeted detail. Header events carry `id`; entry events carry
  // `list_id` (the payload contract the routes + ListAnchorHook pin).
  const listTargets = (p: Record<string, unknown>) => {
    invalidate(["lists"]);
    const kb = kbOf(p);
    const id =
      typeof p.list_id === "string"
        ? p.list_id
        : typeof p.id === "string"
          ? p.id
          : undefined;
    invalidate(kb && id ? ["list", kb, id] : ["list"]);
  };
  for (const t of [
    "list.created",
    "list.updated",
    "list.deleted",
    "list.entry.added",
    "list.entry.updated",
    "list.entry.removed",
    "list.entry.anchor_stale",
    "list.entry.anchor_resolved",
  ]) {
    on(t, listTargets);
  }
  // W2.4 — board.updated {kb, list_id}: one whole-doc PUT already
  // debounced client-side (BoardCanvas.tsx), so a direct (un-gated)
  // invalidation is enough — mirrors comments.updated's per-artifact
  // targeting above.
  on("board.updated", (p) => {
    const kb = kbOf(p);
    const listId = typeof p.list_id === "string" ? p.list_id : undefined;
    invalidate(kb && listId ? ["board", kb, listId] : ["board"]);
  });
  // W2.15b — proposal.created (submit) / proposal.resolved (approve or
  // reject): one write per human/agent action, so a direct invalidation is
  // enough (no burst risk — mirrors board.updated above).
  for (const t of ["proposal.created", "proposal.resolved"]) {
    on(t, () => invalidate(["proposals"]));
  }
  // LSC-4 — session.state fires only on an actual derived-state TRANSITION
  // (never per beat, per LSC-2's server-side doc comment), so a direct
  // (un-gated) invalidation is enough — the same low-rate shape as
  // board.updated/proposal.* just above. Sharpens the Now band / Header
  // chip the moment a session flips working<->waiting<->finished, rather
  // than waiting out useLiveSessions' own ~10s poll.
  on("session.state", () => invalidate(["sessions", "live"]));
  // memory.* in full, plus session.captured for the session lens (its
  // queries key under the same ["memories", …] prefix) and
  // artifact.removed (a forgotten memory is a removed artifact).
  //
  // Every ["memories","related",…] entry is backed by a server-side EMBEDDING
  // recall, and session.captured fires at every agent Stop (invariant #11
  // multi-capture) and in a burst during `kb import claude-history`. Route the
  // whole ["memories"] invalidation through a burst gate (one fixed key) so a
  // capture/ingest storm collapses to one leading + one trailing refetch per
  // window — same shape as the docs churn — instead of re-running recall for
  // every open reader per event.
  //
  // artifact.indexed/excluded/included ALSO ride this gate (dual-purpose,
  // same pattern as artifact.removed above): a salience PATCH or a
  // soft-forget deliberately emits no SSE of its own (see
  // crates/kb-server/src/routes/memory.rs's `patch_salience` doc comment) —
  // the only AUTHORITATIVE signal that state actually changed is the
  // watcher's debounced reindex completing and emitting artifact.indexed
  // (or, for an exclude/include toggle, the matching artifact.excluded/
  // included). Without this, the ["memories"] query is only ever
  // invalidated by earlier, racier triggers, and if that early refetch
  // lands before the reindex commits, nothing invalidates again — the
  // /memory view shows stale salience/un-forgotten rows forever.
  const memoriesGate = makeBurstGate(() => invalidate(["memories"]));
  for (const t of [
    "memory.ingested",
    "memory.stale",
    "memory.resolved",
    "memory.forgotten",
    "memory.linked",
    "memory.unlinked",
    "session.captured",
    "artifact.removed",
    "artifact.indexed",
    "artifact.excluded",
    "artifact.included",
  ]) {
    on(t, () => memoriesGate("memories"));
  }
  on("history.recorded", (p) => {
    const kb = kbOf(p);
    invalidate(kb ? ["readingProgress", kb] : ["readingProgress"]);
    // Resurface strip — a new visit moves the unfinished-read term (visit
    // grain only, like the lists below; scroll/dwell grain has no SSE, #19).
    invalidate(kb ? ["resurface", kb] : ["resurface"]);
    // RL-track — derived read-state moves at VISIT grain (this event is
    // INSERT-only, invariant #8); scroll/dwell-grain changes surface on
    // the next visit or list mutation by design. Invalidating a key
    // nobody holds is a no-op, so this is cheap when no list view is up.
    invalidate(["lists"]);
    invalidate(kb ? ["list", kb] : ["list"]);
    // W2.10 — a new visit/search/comment shifts today's density bucket.
    invalidate(kb ? ["calendar", kb] : ["calendar"]);
    // W3.C-b — and the canvas's `read` + `comment` lanes read the same
    // `history` rows (both are one `history_counts_by_day` scan server-side).
    invalidate(kb ? ["timeline", kb] : ["timeline"]);
  });
  // W3.C-b — the canvas's `session` lane. session.captured already rides the
  // memories burst gate above (with memory.*/artifact.removed); this is its
  // own one-line subscription rather than a line inside that loop so a
  // memory.* event doesn't needlessly refetch four lanes. Many SUBSCRIPTIONS
  // are free — invariant #24 is about CONNECTIONS, and this adds none (it
  // rides the same `sse` facade as every handler above).
  on("session.captured", (p) => {
    const kb = kbOf(p);
    invalidate(kb ? ["timeline", kb] : ["timeline"]);
  });
  // SL4 — kb-slate/1. `slate.updated {slug, seq, kind, id, topic, re, hide,
  // pin}` fires ONCE PER APPEND (routes/slates.rs emits it inside the
  // per-slug lock), and an append is a human or an agent typing a line —
  // human-paced, like board.updated/proposal.* above, so a DIRECT (un-gated)
  // invalidation is right and a burst gate would only add latency to the one
  // surface whose whole point is "what changed since you last looked".
  //
  // Three keys move on one append, and all three are invalidated by PREFIX:
  //   ["slates"]                  the list + the nav attention chip (an
  //                               append can flip a hand to acknowledged or
  //                               a take to contested, both of which are
  //                               counts on GET /api/slates)
  //   ["slate", slug]             the board (prefix — also hits the
  //                               per-topic entries ["slate", slug, topic])
  //   ["slate", slug, "history"]  the drawer, because a drop or a supersede
  //                               ADDS a row there at the same instant it
  //                               removes one from the board. It is a strict
  //                               EXTENSION of ["slate", slug], so the
  //                               prefix above already covers it — it is
  //                               named here so the ledger stays honest
  //                               about what the one call reaches.
  // A payload with no slug (a malformed or future event) falls back to the
  // whole ["slate"] prefix rather than silently invalidating nothing.
  on("slate.updated", (p) => {
    const slug = typeof p.slug === "string" ? p.slug : undefined;
    invalidate(["slates"]);
    invalidate(slug ? ["slate", slug] : ["slate"]);
  });
  // `slate.deleted {slug}` — a purge (loopback-only). The board's own entry
  // is dropped along with the row; invalidating both keeps a stale board
  // from surviving its slate.
  on("slate.deleted", (p) => {
    const slug = typeof p.slug === "string" ? p.slug : undefined;
    invalidate(["slates"]);
    invalidate(slug ? ["slate", slug] : ["slate"]);
  });

  // W1.atlas — a cluster (re)label pass rides atlas recompute/recluster.
  // Both fire one completion event per manual click (not bursty like
  // artifact churn), so a direct invalidation is enough.
  for (const t of ["atlas.recompute.complete", "atlas.recluster.complete"]) {
    on(t, (p) => {
      const kb = kbOf(p);
      invalidate(kb ? ["atlasLabels", kb] : ["atlasLabels"]);
      // W3.M-c — the full-corpus point set shifts coords/cluster on the
      // same two completions.
      invalidate(kb ? ["atlasPoints", kb] : ["atlasPoints"]);
      // W3.F-c — and so does the operator-vs-machine comparison: the
      // sidecar didn't move, but the frame it is fitted onto did, so every
      // aligned position + distance is stale. Prefix match, so this hits
      // ["atlasField", kb, "disagreement"] (the sidecar entry itself is
      // unaffected by a recompute and just refetches identical bytes).
      invalidate(kb ? ["atlasField", kb] : ["atlasField"]);
    });
  }
  // W3.F-c — `atlas.field.updated {kb}`: one whole-document PUT, already
  // debounced client-side (AtlasView's place mode), so a direct un-gated
  // invalidation is enough — the same shape `board.updated` gets, for the
  // same reason (both are a JSON Canvas sidecar written at human pace).
  on("atlas.field.updated", (p) => {
    const kb = kbOf(p);
    invalidate(kb ? ["atlasField", kb] : ["atlasField"]);
  });
  // W3.T-c — `atlas.snapshot.recorded {kb, id, points}` fires once per
  // recompute/recluster that actually appended a frame (the daemon emits it
  // only when the newest frame id moved — routes/atlas.rs's
  // `emit_snapshot_recorded_if_new`), so it is at most as bursty as the two
  // completions above: a direct invalidation is enough. Both time-lapse keys
  // shift together — the list grows by one, and every cached frame body was
  // aligned against the PREVIOUS newest frame (the route's default
  // `align_to`), so those coordinates are now stale even though the stored
  // frames themselves are immutable.
  on("atlas.snapshot.recorded", (p) => {
    const kb = kbOf(p);
    invalidate(kb ? ["atlasHistory", kb] : ["atlasHistory"]);
    invalidate(kb ? ["atlasFrame", kb] : ["atlasFrame"]);
  });
  // Deliberately NOT bridged:
  //   ["sessions"]   — useSessions owns its session.* wiring: a fresh
  //                    capture collapses pagination back to page 1, a
  //                    semantic this all-pages-refetch bridge can't express.
  //   ["note", …]    — useNote owns note.* for its open note: the
  //                    editor-dirty guard must suppress reconciles, and
  //                    that's per-editor state.
  //   stale anchors  — useStaleAnchors is event-SOURCED (live events
  //                    carry richer metadata than the cold-load), not
  //                    fetch-shaped server state.
  //   ["daycard", …] — AmbientRoute (`/ambient`) polls on a slow plain
  //                    timer instead (`refetchInterval`); it is an
  //                    explicitly ambient display, not a surface that
  //                    needs SSE-precise freshness.
  //   useLiveTail    — 3s visibility-gated poll of the live transcript
  //                    tail; component-owned by design (Tier-1 follow
  //                    mode), not server-state shaped for SSE invalidation.
  //   ["sessions","presence"] — 30s refetchInterval probe of
  //                    GET /api/sessions/presence (useSessionPresence);
  //                    deliberately interval-driven, not event-bridged.
  //   ["doclens", …] / ["doclensScorecard", …] — DCB W1.D, the FOURTH
  //                    documented #23 exception. kb-code is a SEPARATE
  //                    daemon with no wiring into kb's `sse` facade at all
  //                    (unlike sessions/note/staleAnchors above, which own
  //                    bespoke SSE wiring against kb's OWN bridge, this
  //                    query has no "own wiring" to speak of — only its
  //                    structural absence, same category as `["daycard",
  //                    …]`/useLiveTail/sessions-presence just above). Finite
  //                    staleTime (30s) + a manual refresh affordance next to
  //                    the rendered `resolved_unix` stand in for SSE-
  //                    precise freshness on a live git-tree read.
  //   ["slateGrounding", …] — SL7d/D29, the slate board's groundedness
  //                    captions. The SAME cross-daemon category as
  //                    ["doclens", …] just above (kb-code has no wiring into
  //                    kb's `sse` facade at all), with the same 30s
  //                    staleTime and `retry: false`; a failed fetch is an
  //                    honest `unknown` caption, never a retried guess.
  //   ["desk"] / ["desk", kb] — kb desk v2 (`GET /api/desk`, `useDesk`).
  //                    Joins this no-SSE-tie set: finite staleTime (60s) +
  //                    refetchOnWindowFocus, no new SSE kind. Handoffs are
  //                    ephemeral drafts whose freshness is a wall-clock
  //                    concern, not a live collaboration feed.

  // Gap: the events inside the gap are unknowable — every cached
  // server-state entry may be stale, so invalidate the world. Active
  // queries refetch; inactive ones refetch on next mount.
  offs.push(sse.onResync(() => void queryClient.invalidateQueries()));

  return () => offs.forEach((off) => off());
}
