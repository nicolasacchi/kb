// TanStack Query foundation — same cache philosophy as kb's own
// `web/src/api/queryClient.ts` (SSE-driven freshness: `staleTime: Infinity`,
// no time-based refetch, invalidation happens when the daemon SAYS
// something changed), simplified for kb-code's single-daemon world.
//
// **Simplification vs. kb's SharedWorker SSE architecture (invariant #24
// in kb's own CLAUDE.md):** kb runs MANY daemons, so a browser tab needs
// exactly ONE SSE connection shared across every open kb tab (hence the
// SharedWorker — one socket per daemon per BROWSER, not per tab). kb-code
// is a SINGLE daemon per SPA instance with a small, low-churn event
// vocabulary (`mirror.updated` / `repo.head_moved` — see
// `crates/kb-code-server/src/routes.rs`'s `events` doc: "kb-code's Wave-1
// event vocabulary is exactly two types"). A plain per-tab `EventSource` is
// the right-sized primitive here: N tabs open N connections to the SAME
// local daemon, which is cheap for a loopback, single-operator process —
// the SharedWorker's whole reason to exist (many REMOTE daemons, expensive
// duplicate sockets) doesn't apply. If kb-code ever grows kb's multi-daemon
// shape, revisit this the same way kb's own sse/core.ts documents.

import { notifyManager, QueryClient } from "@tanstack/react-query";
import { ApiError } from "./client";

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

// --- query-key conventions ----------------------------------------------
//
//   ["repos"]                              GET /api/repos
//   ["tree", repo, path, ref]              GET /api/tree
//   ["file", repo, path, ref]              GET /api/file
//   ["refs", repo]                         GET /api/refs
//   ["diff", repo, path, from, to]         GET /api/diff
//   ["blame", repo, path, ref]             GET /api/blame
//   ["why-line", repo, path, sha]          GET /api/why?line= (cached by sha)
//   ["story", repo, path]                  GET /api/story (CT-E2 — the
//                                          provenance tab's File-story
//                                          section; fetched only while
//                                          that section is mounted)
//   ["blame-timeline", repo, path, line]   GET /api/blame/timeline
//   ["annotations", repo, path]            GET /api/annotations
//   ["session-diff", sessionId, repo]      GET /api/session-diff
//   ["compare", repo, from, to, threeDot,
//    attribution]                          GET /api/compare
//   ["branches", repo]                     GET /api/branches
//   ["merge-check", repo, from, to]        GET /api/merge-check
//   ["range-diff", repo, old, new]         GET /api/range-diff
//   ["repo-state", repo]                   GET /api/repo-state — invalidated
//                                          by the generic per-repo predicate
//                                          below (queryKey[1] === repo), no
//                                          bridge change needed
//   ["prs", repo]                          GET /api/prs
//   ["pr-comments", repo, number]          GET /api/prs/{number}/comments
//   ["sets", repo]                         GET /api/sets (Phase E4)
//   ["sets", repo, id]                     GET /api/sets/{id} — the `repo`
//                                          prefix (not sent over the wire —
//                                          added by the CALLER, `hooks/
//                                          useSets.ts`) is what lets the
//                                          `set.changed` SSE handler below
//                                          invalidate both with one call
//   ["bookmarks", repo]                    GET /api/bookmarks (Phase N)
//   ["todos", repo, marker, scope, …]      GET /api/todos (Phase N)
//   ["scopes"]                             GET /api/scopes (Phase N)
//   ["reviews", repo, …]                   GET /api/reviews* (V3.R2) —
//                                          `review.changed` invalidates the
//                                          `["reviews", repo]` prefix
//                                          (covers `reason:"verdict"`)
//   ["reviews", repo, "comments", id, …]   GET /api/reviews/{id}/comments
//                                          (V4.C4) — also invalidated by
//                                          `annotation.changed` when
//                                          `review_id` is present
//   ["behavioral", "hotspots", repo, …]    GET /api/behavioral/* (V3.2-B1/B3)
//   ["reviews", repo, "risk", id]          GET /api/reviews/{id}/risk (B2/B3)
//   ["doclens", kb, docId, repo]           GET /api/doc-lens (DCB W2.B) —
//                                          keyed on `kb`/`docId` at
//                                          positions [1]/[2], NOT `repo` at
//                                          [1] — so the per-repo SSE bridge
//                                          above (which matches on
//                                          `queryKey[1] === repo`) never
//                                          touches it. Structurally
//                                          un-bridgeable anyway: the result
//                                          depends on kb's OWN content
//                                          (reindex), an event kb-code's bus
//                                          never carries — a finite
//                                          `staleTime` (`hooks/
//                                          useDocLens.ts`) + a manual
//                                          refresh affordance next to
//                                          `resolved_unix` (`Scorecard.tsx`)
//                                          stand in for SSE invalidation.
//                                          NOTE (W2.B.R fix 14): `invalidate
//                                          ForRepo`'s predicate is position-
//                                          ONLY (`queryKey[1] === repo`,
//                                          never checking `queryKey[0]`), so
//                                          if a configured kb name happens
//                                          to literally equal a configured
//                                          repo name, a `mirror.updated`/
//                                          `repo.head_moved` event for that
//                                          repo WILL also invalidate this
//                                          doclens entry — a harmless extra
//                                          refetch (the lens data is still
//                                          correct either way), not worth a
//                                          `queryKey[0]`-typed predicate for
//                                          a coincidental name collision.
//   ["doclens-repos", kb, docId]           GET /api/doc-lens/repos (DCB W2.B)
//   ["docRefs", repo, path]                GET /api/doc-refs (DCB W3.B,
//                                          `hooks/useDocRefs.ts`'s
//                                          `docRefsQueryKey`) — the ONE
//                                          camelCase key in this whole list
//                                          (every sibling above is a bare
//                                          noun); kept as `docRefs` rather
//                                          than renamed to match, since a
//                                          rename only churns every call
//                                          site for a cosmetic mismatch, not
//                                          a behavior change (m8, W3.B.R
//                                          review). Position [1] === `repo`
//                                          means the generic per-repo bridge
//                                          below (`invalidateForRepo`'s
//                                          `queryKey[1] === repo` predicate)
//                                          ALSO matches this key — same
//                                          coincidental-bridge shape the
//                                          doclens row above documents for
//                                          `kb`-vs-`repo` name collisions,
//                                          except here it's load-bearing on
//                                          purpose: a `mirror.updated`/
//                                          `repo.head_moved` event for this
//                                          repo invalidates `docRefs` too
//                                          (the `live` flag depends on the
//                                          file's current existence), on top
//                                          of — never a substitute for — the
//                                          hook's own finite `staleTime`
//                                          (see `useDocRefs.ts`'s own doc:
//                                          the OTHER half of freshness here,
//                                          a doc's citations changing kb-
//                                          side, has no dedicated event).
//
// ── PRR-U2 ── kb v0.39 "The PR Room," unit U2 (Room cockpit + Report tab) ──
//   ["reviews", repo, "report", id]        GET /api/reviews/{id}/report
//   ["reviews", repo, "findings", id, …]   GET /api/reviews/{id}/findings
//                                          (ps/disposition/include_superseded
//                                          in the tail — see `useReviews.ts`'s
//                                          `reviewFindingsKey`)
//   ["reviews", repo, "checks", id]        GET /api/prs/{number}/checks —
//                                          deliberately keyed under the
//                                          `reviews` PREFIX (not `prs`),
//                                          even though the underlying route
//                                          is PR-scoped, so it rides the
//                                          SAME `review.changed` prefix-
//                                          invalidation every other
//                                          review-surface query gets for
//                                          free — no new bridge handler.
//                                          `annotation.changed`'s existing
//                                          `review_id` branch (see that
//                                          handler above) additionally
//                                          covers `["reviews", repo,
//                                          "findings", …]` and disposition
//                                          writes: a disposition mutation
//                                          IS an annotation-adjacent write
//                                          on the finding's own row, and
//                                          `review.changed{reason:
//                                          "disposition"|"findings_import"}`
//                                          also prefix-matches `["reviews",
//                                          repo]` regardless.
//   ["reviews", repo, "pr-reviews", id]    GET /api/prs/{number}/reviews —
//                                          kept under the `reviews` PREFIX
//                                          per this unit's hook-naming
//                                          convention (`usePrReviews`, like
//                                          its four siblings above), but
//                                          this one is GitHub-ORIGIN data —
//                                          `review.changed` prefix-
//                                          invalidation is a harmless bonus
//                                          here, not the real freshness
//                                          mechanism. Same finite-staleness
//                                          "documented exception" `["prs",
//                                          repo]` already is (doclens
//                                          precedent, `BEHAVIORAL_STALE_MS`):
//                                          `GithubConversationCard` sets an
//                                          explicit `staleTime` + shows a
//                                          `fetched_at` caption + a manual
//                                          refresh button, exactly like
//                                          `useReviewMap`/`useReviewReadingOrder`
//                                          already do for their own 404-vs-
//                                          absent degrade. `["pr-comments",
//                                          repo, number]` (existing, `hooks/
//                                          usePrs.ts`) is reused as-is for
//                                          the comment half — no new key.
//
// ── PRR-U1 ── kb v0.39 "The PR Room," unit U1 (Review Room landing) ────────
//   ["reviews", repo, "inbox", state]     GET /api/reviews/inbox (design doc
//                                          §2 S1, `hooks/useReviews.ts`'s
//                                          `reviewInboxKey`) — under the same
//                                          `["reviews", repo]` PREFIX as
//                                          every other review query above,
//                                          so `review.changed` invalidation
//                                          (this file's own handler below)
//                                          covers it for free; a `pr_bound`
//                                          reason (POST /api/reviews/pr) or
//                                          a disposition/findings-import
//                                          reason both refresh the queue
//                                          with no new bridge wiring. The
//                                          landing route joins this query's
//                                          rows against `["reviews", repo,
//                                          "list", "all"]` (existing key,
//                                          `useReviews(repo, null)`) client-
//                                          side for `pr_meta`/`has_report`/
//                                          `report_risk_score` — see
//                                          `lib/reviewInbox.ts`'s module doc.
//
// ── PRR-F ── kb v0.39 T2 frontier unit — GitHub threads, recurring-finding
// memory, reviewer X-ray (design-ui.md §12 items 1/2/4, design-addendum-2 §A):
//   ["reviews", repo, "github-threads", id]  GET /api/reviews/{id}/github-threads
//                                          — SAME finite-staleness "documented
//                                          exception" as `["reviews", repo,
//                                          "pr-reviews", id]` just above
//                                          (`useGithubThreads`, `BEHAVIORAL_
//                                          STALE_MS`) — GitHub-origin data, a
//                                          `fetched_at` caption + manual
//                                          refresh is the real freshness
//                                          mechanism, not SSE. Kept under the
//                                          `reviews` PREFIX anyway (harmless
//                                          bonus invalidation on
//                                          `review.changed`, same rationale).
//   ["reviews", repo, "findings-recurrence", id]
//                                          GET /api/reviews/{id}/findings/
//                                          recurrence — computed fresh per
//                                          request (never persisted); default
//                                          `staleTime` + the ordinary
//                                          `review.changed`/`annotation.changed`
//                                          prefix-invalidation, same as
//                                          `["reviews", repo, "findings", …]`.
//   ["reviews", repo, "impact", id, path]  GET /api/reviews/{id}/impact?path=
//                                          — lazy (fetched only once a diff
//                                          file's SECTION expands, `useReview
//                                          Impact`'s own doc), default
//                                          `staleTime` under the same prefix.

// --- W4.5 / Wave E — the live-mirror registry ------------------------------
//
// `useLiveMirror` (per open PANE) registers itself here so the bridge below
// can consult "is the CURRENTLY OPEN file's viewer dirty (scrolled/
// selected)" without React state leaking into this module.
//
// Wave E refactor: this was a single-slot registration (one reader pane per
// tab, a later registration simply replacing the earlier one) — with
// splits, TWO panes can have two DIFFERENT files open simultaneously, so it
// is now a keyed `Map<paneId, …>`. `paneId` is caller-chosen (Reader.tsx
// uses `"pane1"`/`"pane2"`) and opaque here; `unregister` only clears ITS
// OWN key if it still owns the slot (guards a stale cleanup — e.g. a fast
// path switch within the same pane — from clobbering a newer registration
// under the same id).
interface LiveMirrorRegistration {
  repo: string;
  path: string;
  isDirty: () => boolean;
}
const liveMirrorRegistrations = new Map<string, LiveMirrorRegistration>();

export function registerOpenFileForLiveMirror(
  paneId: string,
  repo: string,
  path: string,
  isDirty: () => boolean,
): () => void {
  liveMirrorRegistrations.set(paneId, { repo, path, isDirty });
  return () => {
    const cur = liveMirrorRegistrations.get(paneId);
    if (cur && cur.repo === repo && cur.path === path) {
      liveMirrorRegistrations.delete(paneId);
    }
  };
}

/// Every registered pane (any paneId) whose OWN open file is named by
/// `paths` for `repo`, AND whose viewer is currently dirty (scrolled/
/// selected — `useLiveMirror`'s heuristic, `lib/liveMirror.ts`) — i.e. the
/// set of panes the W4.5 carve-out below must protect from a silent
/// refetch. With splits, both panes are checked independently; either,
/// neither, or both can match (two panes CAN have the same file open, e.g.
/// right after `Ctrl-w v`'s "split with self").
function dirtyOpenPanesTouchedBy(repo: string, paths: string[]): LiveMirrorRegistration[] {
  const hits: LiveMirrorRegistration[] = [];
  for (const reg of liveMirrorRegistrations.values()) {
    if (reg.repo === repo && paths.includes(reg.path) && reg.isDirty()) hits.push(reg);
  }
  return hits;
}

/// Both of kb-code's Wave-1 event types invalidate the SAME broad surface
/// (a mirror update can touch tree/file/symbols/refs for the repo it named;
/// a HEAD move changes what "the current ref" resolves to for every open
/// view) — unlike kb's own fine-grained per-artifact invalidation, kb-code's
/// event vocabulary is too coarse to target narrower keys yet, so this
/// invalidates every query keyed on that repo's name. `event.repo` is
/// carried in the SSE payload (`mirror::Sink`'s publish call — see that
/// module) for both event types.
///
/// **W4.5 carve-out (Wave E: now checks EVERY registered pane):** for each
/// dirty pane `dirtyOpenPanesTouchedBy` returns, that ONE `["file", …]`
/// query is deliberately invalidated with `refetchType: "none"` — marked
/// stale so the next real navigation/remount picks up fresh content, but
/// NOT force-refetched here, so its content doesn't change out from under
/// the user. `useLiveMirror`'s toast is what gives them an explicit
/// opt-in refresh (or, when pristine, `useLiveMirror` itself calls the
/// query's own `refetch()` — this carve-out and that call are two sides of
/// the same heuristic, kept in sync by both reading `lib/liveMirror.ts`).
function invalidateForRepo(repo: string | undefined, paths?: string[]): void {
  if (!repo) {
    // No repo named (shouldn't happen for these two event types, but stay
    // defensive) — invalidate everything rather than silently going stale.
    void queryClient.invalidateQueries();
    return;
  }
  const dirtyHits = paths ? dirtyOpenPanesTouchedBy(repo, paths) : [];
  const protectedPaths = new Set(dirtyHits.map((h) => h.path));

  void queryClient.invalidateQueries({
    predicate: (q) => {
      if (q.queryKey.length < 2 || q.queryKey[1] !== repo) return false;
      if (q.queryKey[0] === "file" && protectedPaths.has(q.queryKey[2] as string)) {
        return false; // handled by the `refetchType: "none"` call below instead
      }
      return true;
    },
  });
  void queryClient.invalidateQueries({ queryKey: ["repos"] });
  for (const path of protectedPaths) {
    void queryClient.invalidateQueries({
      predicate: (q) => q.queryKey[0] === "file" && q.queryKey[1] === repo && q.queryKey[2] === path,
      refetchType: "none",
    });
  }
}

export interface SsePayload {
  repo?: string;
  paths?: string[];
  path?: string;
  old?: string | null;
  new?: string;
  /// V4.C1/C4 — present on review-scoped annotation events.
  review_id?: number;
  batch?: boolean;
  /// V4.C2 — `review.changed` reason (`verdict` | `patchset` | `meta` | `deleted`).
  reason?: string;
}

/// Parse one SSE `data:` line into a payload, or `null` for a malformed/
/// non-JSON line (keep-alive comments never reach here — `EventSource`
/// only surfaces named `message`/`<event>` frames, never `:`-comment lines).
export function parseSseData(raw: string): SsePayload | null {
  try {
    const parsed = JSON.parse(raw) as { payload?: SsePayload };
    return parsed.payload ?? null;
  } catch {
    return null;
  }
}

let bridgeStarted = false;

/// One `EventSource` → query-invalidation bridge for the tab's lifetime.
/// Idempotent (a second call is a no-op) — mirrors kb's own
/// `startSseInvalidationBridge`'s call-once contract, called from
/// `main.tsx`. Beyond query invalidation, every named event is ALSO
/// re-dispatched as a `window` `CustomEvent` (`kbc:<type>`, `detail` = the
/// parsed payload) — the same "root-mounted event, several components care"
/// idiom `app.tsx`'s `kbc:omnibox.open` already uses — so W4.5's live-mirror
/// UI (`useLiveMirror`) and W4.6's annotations panel can react to the SAME
/// SSE frames without a second connection.
export function startSseInvalidationBridge(): void {
  if (bridgeStarted) return;
  bridgeStarted = true;
  const source = new EventSource("/api/events");

  source.addEventListener("mirror.updated", (ev: MessageEvent<string>) => {
    const payload = parseSseData(ev.data);
    window.dispatchEvent(new CustomEvent("kbc:mirror.updated", { detail: payload }));
    invalidateForRepo(payload?.repo, payload?.paths);
  });
  source.addEventListener("repo.head_moved", (ev: MessageEvent<string>) => {
    const payload = parseSseData(ev.data);
    window.dispatchEvent(new CustomEvent("kbc:repo.head_moved", { detail: payload }));
    invalidateForRepo(payload?.repo);
  });
  source.addEventListener("annotation.changed", (ev: MessageEvent<string>) => {
    const payload = parseSseData(ev.data);
    window.dispatchEvent(new CustomEvent("kbc:annotation.changed", { detail: payload }));
    if (payload?.repo && payload.path) {
      void queryClient.invalidateQueries({ queryKey: ["annotations", payload.repo, payload.path] });
    }
    // Batch form: `{repo, paths:[], batch:true, review_id?}`.
    if (payload?.repo && payload.paths) {
      for (const p of payload.paths) {
        void queryClient.invalidateQueries({ queryKey: ["annotations", payload.repo, p] });
      }
    }
    // Review-scoped writes also refresh the threaded comments query
    // (`["reviews", repo, "comments", …]`). `review.changed` already
    // prefix-invalidates `["reviews", repo]` (covers `reason:"verdict"`);
    // do not duplicate that here.
    if (payload?.repo && payload.review_id != null) {
      void queryClient.invalidateQueries({ queryKey: ["reviews", payload.repo, "comments"] });
    }
    // V70-A10 — the event payload carries `repo`/`path` (or `paths`), never
    // `set_id` (`routes::emit_annotation_changed`'s doc — deliberately NOT
    // widened for this unit; a workspace note's own repo/path already
    // trigger the invalidations above). A coarse, UNSCOPED prefix
    // invalidation of every open `["workspace-notes", …]` query on ANY
    // annotation write is the honest v0 trade-off: correct (a workspace
    // note IS an annotation write), just broader than a `set_id`-scoped
    // invalidation would be — acceptable at this crate's single-operator
    // scale (same posture `set.changed`'s bare `{repo}` payload already
    // takes below).
    void queryClient.invalidateQueries({ queryKey: ["workspace-notes"] });
  });
  // V4.S2 — a suggestion APPLY is a working-tree write announced as its
  // own event (`suggestions.rs`): refresh the thread (applied chip), the
  // per-path annotations, and the file content the diff/reader show. The
  // applying tab already invalidates from its mutation; this handler is
  // what keeps OTHER tabs live (#23: the bridge owns invalidation).
  source.addEventListener("suggestion.applied", (ev: MessageEvent<string>) => {
    const payload = parseSseData(ev.data);
    window.dispatchEvent(new CustomEvent("kbc:suggestion.applied", { detail: payload }));
    if (payload?.repo && payload.path) {
      void queryClient.invalidateQueries({ queryKey: ["annotations", payload.repo, payload.path] });
      void queryClient.invalidateQueries({ queryKey: ["file", payload.repo, payload.path] });
    }
    if (payload?.repo && payload.review_id != null) {
      void queryClient.invalidateQueries({ queryKey: ["reviews", payload.repo, "comments"] });
    }
  });
  // Phase E4 — reading sets. Every mutation (`reading_sets.rs`'s doc) emits
  // a bare `{repo}` payload (a set has no single `path` of its own to
  // report, unlike `annotation.changed` above) — invalidating the
  // `["sets", repo]` PREFIX reaches both the list query and every open
  // set's own detail query (`hooks/useSets.ts`'s key convention) in one call
  // (TanStack Query's default `invalidateQueries` match is a prefix match).
  source.addEventListener("set.changed", (ev: MessageEvent<string>) => {
    const payload = parseSseData(ev.data);
    window.dispatchEvent(new CustomEvent("kbc:set.changed", { detail: payload }));
    if (payload?.repo) {
      void queryClient.invalidateQueries({ queryKey: ["sets", payload.repo] });
    }
  });
  // Phase N — bookmarks. Same bare `{repo}` payload as `set.changed`
  // (`bookmarks.rs`); invalidate `["bookmarks", repo]` (prefix match covers
  // any future per-id keys that share the prefix).
  source.addEventListener("bookmark.changed", (ev: MessageEvent<string>) => {
    const payload = parseSseData(ev.data);
    window.dispatchEvent(new CustomEvent("kbc:bookmark.changed", { detail: payload }));
    if (payload?.repo) {
      void queryClient.invalidateQueries({ queryKey: ["bookmarks", payload.repo] });
    }
  });
  // V3.R2 — review sessions. Payload carries `{repo, review_id}` (see
  // `reviews.rs`); prefix-invalidate `["reviews", repo]` so list + detail +
  // files + annotations + interdiff all refresh in one call.
  source.addEventListener("review.changed", (ev: MessageEvent<string>) => {
    const payload = parseSseData(ev.data);
    window.dispatchEvent(new CustomEvent("kbc:review.changed", { detail: payload }));
    if (payload?.repo) {
      void queryClient.invalidateQueries({ queryKey: ["reviews", payload.repo] });
    }
    // S2-A — the unified inbox (`hooks/useUnifiedInbox.ts`) spans every
    // repo's review lane, so ANY review change anywhere invalidates it too
    // (no `payload.repo` gate — the query key carries no repo to match
    // against). The annotations/kb lanes stay on the hook's own 30s
    // staleTime; see that hook's doc for why.
    void queryClient.invalidateQueries({ queryKey: ["inbox", "unified"] });
  });
  // `EventSource` auto-reconnects on a network drop; a resumed stream can
  // skip events it doesn't replay (no `Last-Event-ID` bookkeeping here,
  // unlike kb's SharedWorker gap/resync path — W4.1 scope stops short of
  // that), so treat every reconnect as "something might have changed."
  source.onopen = () => invalidateForRepo(undefined);
}
