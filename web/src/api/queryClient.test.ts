// Pins invariant #23 — the SSE → TanStack Query invalidation MAPPING (the
// bridge itself, not the SSE transport wire format, which #24 covers). The
// `./sse` facade is replaced with an in-memory fake so a test can fire a
// synthetic event of any kind and assert exactly which query-key prefixes
// invalidate — including the v0.24 X3/X4 additions (artifact.excluded/
// included joining the docs-churn gate PLUS the dedicated
// ["exclusions", kb] pane refresh) and the dual-purpose artifact.removed
// (both a docs-churn AND a memories-churn signal). artifact.indexed/
// excluded/included ALSO now double as memories-churn signals: a salience
// PATCH / soft-forget emits no SSE of its own, so the watcher's debounced
// reindex (landing as artifact.indexed, or artifact.excluded/included for
// an exclude toggle) is the only authoritative refresh trigger for the
// ["memories"] query — see queryClient.ts's memoriesGate comment.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type EventCb = (payload: Record<string, unknown>, daemonUrl: string) => void;
const listeners = new Map<string, Set<EventCb>>();
const resyncListeners = new Set<() => void>();

vi.mock("./sse", () => ({
  sse: {
    subscribeEvent: (type: string, fn: EventCb) => {
      let set = listeners.get(type);
      if (!set) {
        set = new Set();
        listeners.set(type, set);
      }
      set.add(fn);
      return () => set!.delete(fn);
    },
    onResync: (fn: () => void) => {
      resyncListeners.add(fn);
      return () => resyncListeners.delete(fn);
    },
  },
}));

function emit(type: string, payload: Record<string, unknown> = {}) {
  listeners.get(type)?.forEach((fn) => fn(payload, "http://daemon"));
}
function emitGap() {
  resyncListeners.forEach((fn) => fn());
}

import { queryClient, startSseInvalidationBridge } from "./queryClient";

describe("SSE → TanStack invalidation bridge (invariant #23)", () => {
  let spy: ReturnType<typeof vi.spyOn>;
  let teardown: () => void;

  beforeEach(() => {
    spy = vi.spyOn(queryClient, "invalidateQueries").mockResolvedValue(undefined);
    // Fresh bridge (and fresh burst-gate closures) per test, so the first
    // invalidate() of each test is always the gate's synchronous LEADING
    // edge — no fake timers needed to observe it.
    teardown = startSseInvalidationBridge();
  });
  afterEach(() => {
    teardown();
    listeners.clear();
    resyncListeners.clear();
    spy.mockRestore();
  });

  function keys(): unknown[] {
    return spy.mock.calls.map(
      (call: unknown[]) => (call[0] as { queryKey: unknown[] }).queryKey,
    );
  }

  // The trailing ["memories"] pins the fix for the stale-salience/
  // stale-forget bug: patch_salience/soft-forget emit no SSE of their own,
  // so artifact.indexed landing (from the watcher's debounced reindex) is
  // the only authoritative signal that ["memories"] must refetch.
  it("artifact.indexed burst-invalidates the whole docs-churn key set for its kb, AND the memories gate", () => {
    emit("artifact.indexed", { kb: "demo" });
    expect(keys()).toEqual([
      ["docs", "demo"],
      ["doc", "demo"],
      ["tags", "demo"],
      ["facets", "demo"],
      ["folders", "demo"],
      ["versions", "demo"],
      ["edges", "demo"],
      ["codeRefs", "demo"],
      ["search"],
      ["timeline", "demo"],
      ["memories"],
    ]);
  });

  // invariant:23 — v0.24 X3/X4: exclusion intent kinds ride the SAME docs
  // churn gate AND directly refresh the Settings → Excluded pane's own key.
  // They ALSO ride the memories gate (an excluded/re-included artifact can
  // be a memory), same dual-purpose shape as artifact.removed below.
  it("artifact.excluded ALSO invalidates [\"exclusions\", kb] and the memories gate, beyond the docs churn", () => {
    emit("artifact.excluded", { kb: "demo" });
    expect(keys()).toEqual([
      ["docs", "demo"],
      ["doc", "demo"],
      ["tags", "demo"],
      ["facets", "demo"],
      ["folders", "demo"],
      ["versions", "demo"],
      ["edges", "demo"],
      ["codeRefs", "demo"],
      ["search"],
      ["timeline", "demo"],
      ["exclusions", "demo"],
      ["memories"],
    ]);
  });

  it("artifact.included mirrors artifact.excluded's key set", () => {
    emit("artifact.included", { kb: "demo" });
    expect(keys()).toEqual([
      ["docs", "demo"],
      ["doc", "demo"],
      ["tags", "demo"],
      ["facets", "demo"],
      ["folders", "demo"],
      ["versions", "demo"],
      ["edges", "demo"],
      ["codeRefs", "demo"],
      ["search"],
      ["timeline", "demo"],
      ["exclusions", "demo"],
      ["memories"],
    ]);
  });

  it("comments.updated targets the open document's review + the fleet inbox + resurface", () => {
    emit("comments.updated", { kb: "demo", artifact_id: "a1" });
    expect(keys()).toEqual([
      ["review", "demo", "a1"],
      ["inbox"],
      ["resurface", "demo"],
    ]);
  });

  it("note.* invalidates the shared notes store, no kb scoping", () => {
    for (const t of ["note.created", "note.updated", "note.deleted"]) {
      emit(t, { kb: "demo" });
    }
    expect(keys()).toEqual([["notes"], ["notes"], ["notes"]]);
  });

  it("anchor.added/removed invalidate the daemon-wide corkboard", () => {
    emit("anchor.added", {});
    emit("anchor.removed", {});
    expect(keys()).toEqual([["anchors"], ["anchors"]]);
  });

  it("list.updated invalidates the index plus the targeted list detail", () => {
    emit("list.updated", { kb: "demo", id: "L1" });
    expect(keys()).toEqual([["lists"], ["list", "demo", "L1"]]);
  });

  it("list.entry.added keys its detail off `list_id`, not `id`", () => {
    emit("list.entry.added", { kb: "demo", list_id: "L1" });
    expect(keys()).toEqual([["lists"], ["list", "demo", "L1"]]);
  });

  it("memory.ingested burst-invalidates the memories prefix", () => {
    emit("memory.ingested", {});
    expect(keys()).toEqual([["memories"]]);
  });

  // W3.C-b — session.captured now ALSO refreshes the reflection canvas's
  // `session` lane, from its own one-line subscription (NOT a line inside
  // the memories-gate loop, so a memory.* event doesn't refetch four lanes).
  // A kb-less payload falls back to the un-scoped prefix, like every other
  // handler here.
  it("session.captured rides the memories gate AND refreshes the timeline lanes", () => {
    emit("session.captured", { kb: "demo" });
    expect(keys()).toEqual([["memories"], ["timeline", "demo"]]);
  });

  it("session.captured without a kb invalidates the whole timeline prefix", () => {
    emit("session.captured", {});
    expect(keys()).toEqual([["memories"], ["timeline"]]);
  });

  // artifact.removed is dual-purpose: a docs-churn signal (row leaves the
  // gallery) AND a memories-churn signal (a forgotten memory is a removed
  // artifact) — both listeners are registered on the one event kind.
  it("artifact.removed fires BOTH the docs churn and the memories gate", () => {
    emit("artifact.removed", { kb: "demo" });
    expect(keys()).toEqual([
      ["docs", "demo"],
      ["doc", "demo"],
      ["tags", "demo"],
      ["facets", "demo"],
      ["folders", "demo"],
      ["versions", "demo"],
      ["edges", "demo"],
      ["codeRefs", "demo"],
      ["search"],
      ["timeline", "demo"],
      ["memories"],
    ]);
  });

  it("history.recorded refreshes reading progress, resurface, lists, the calendar and the timeline", () => {
    emit("history.recorded", { kb: "demo" });
    expect(keys()).toEqual([
      ["readingProgress", "demo"],
      ["resurface", "demo"],
      ["lists"],
      ["list", "demo"],
      ["calendar", "demo"],
      // W3.C-b — the canvas's read + comment lanes read the same history rows.
      ["timeline", "demo"],
    ]);
  });

  // LSC-4 — session.state fires only on a derived-state TRANSITION (never
  // per beat), so a direct un-gated invalidation of the live-status query
  // is enough — no burst gate, unlike the bursty artifact-churn kinds above.
  it("session.state directly invalidates the live-sessions cockpit query", () => {
    emit("session.state", { session_id: "sid-1", kb: "demo" });
    expect(keys()).toEqual([["sessions", "live"]]);
  });

  // SL4 — the slate bridge. These four cases pin the EXACT key set: a fifth
  // key appearing here (or one of these going missing) is the bug the
  // design's "with the ledger comment and the vitest case that pins the
  // exact key set" line is guarding against.
  it("slate.updated directly invalidates the list and the named slate's board", () => {
    emit("slate.updated", { slug: "kb", seq: 66, kind: "found" });
    expect(keys()).toEqual([["slates"], ["slate", "kb"]]);
  });

  // ["slate", slug, "history"] is a strict EXTENSION of ["slate", slug], so
  // the drawer refreshes on the same one call — asserted here as a key-shape
  // fact so nobody "fixes" it by adding a third invalidation.
  it("the board key is a prefix of the history key, so one invalidation reaches both", () => {
    const board = ["slate", "kb"];
    const history = ["slate", "kb", "history"];
    expect(history.slice(0, board.length)).toEqual(board);
  });

  it("slate.deleted invalidates the list and the purged slate", () => {
    emit("slate.deleted", { slug: "kb" });
    expect(keys()).toEqual([["slates"], ["slate", "kb"]]);
  });

  it("a slate event with no slug falls back to the whole slate prefix", () => {
    emit("slate.updated", {});
    expect(keys()).toEqual([["slates"], ["slate"]]);
  });

  it("slate.updated is NOT burst-gated — two appends invalidate twice", () => {
    emit("slate.updated", { slug: "kb", seq: 1 });
    emit("slate.updated", { slug: "kb", seq: 2 });
    expect(keys()).toEqual([
      ["slates"],
      ["slate", "kb"],
      ["slates"],
      ["slate", "kb"],
    ]);
  });

  it("a gap/resync invalidates the whole cache with no key filter", () => {
    emitGap();
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith();
  });
});
