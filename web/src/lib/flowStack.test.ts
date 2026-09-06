import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  FLOW_CAP,
  consumeFlowSeed,
  flowClear,
  flowDropAbove,
  flowLabel,
  flowList,
  flowPeek,
  flowPop,
  flowPush,
  flowSnapshot,
  subscribeFlow,
  truncateLabel,
  writeFlowSeed,
  type FlowEntry,
} from "./flowStack";

// vitest runs in the node env (no DOM) — stub a minimal in-memory
// sessionStorage, same pattern as `registers.test.ts` / `marks.test.ts`
// use for localStorage.
function stubSessionStorage() {
  const store: Record<string, string> = {};
  vi.stubGlobal("sessionStorage", {
    getItem: (k: string) => (k in store ? store[k] : null),
    setItem: (k: string, v: string) => {
      store[k] = v;
    },
    removeItem: (k: string) => {
      delete store[k];
    },
    clear: () => {
      for (const k of Object.keys(store)) delete store[k];
    },
  });
  return store;
}

function entry(over: Partial<FlowEntry> = {}): FlowEntry {
  return {
    kb: "canon",
    id: "aaaaaaaaaaaa",
    sourceRelative: "pm/00-summary.html",
    title: "INC-0315 · Summary",
    scrollY: 420,
    ts: 1_700_000_000_000,
    ...over,
  };
}

let store: Record<string, string>;
beforeEach(() => {
  store = stubSessionStorage();
});

describe("push / peek / pop", () => {
  it("starts empty", () => {
    expect(flowList()).toEqual([]);
    expect(flowPeek()).toBeUndefined();
    expect(flowPop()).toBeUndefined();
  });

  it("pushes oldest-first with the newest on top", () => {
    flowPush(entry({ sourceRelative: "a.html", title: "A" }));
    flowPush(entry({ sourceRelative: "b.html", title: "B" }));
    expect(flowList().map((e) => e.title)).toEqual(["A", "B"]);
    expect(flowPeek()?.title).toBe("B");
  });

  it("pops the newest and shortens the stack", () => {
    flowPush(entry({ sourceRelative: "a.html", title: "A" }));
    flowPush(entry({ sourceRelative: "b.html", title: "B" }));
    expect(flowPop()?.title).toBe("B");
    expect(flowList().map((e) => e.title)).toEqual(["A"]);
    expect(flowPop()?.title).toBe("A");
    expect(flowList()).toEqual([]);
  });

  it("collapses a consecutive duplicate (same kb + path) rather than stacking it", () => {
    flowPush(entry({ scrollY: 10 }));
    flowPush(entry({ scrollY: 99 }));
    expect(flowList()).toHaveLength(1);
    expect(flowPeek()?.scrollY).toBe(99);
  });

  it("keeps a NON-consecutive repeat (a real A → B → A descent)", () => {
    flowPush(entry({ sourceRelative: "a.html" }));
    flowPush(entry({ sourceRelative: "b.html" }));
    flowPush(entry({ sourceRelative: "a.html" }));
    expect(flowList().map((e) => e.sourceRelative)).toEqual([
      "a.html",
      "b.html",
      "a.html",
    ]);
  });

  it("preserves the optional section", () => {
    flowPush(entry({ sec: "findings" }));
    expect(flowPeek()?.sec).toBe("findings");
  });

  it("ignores a malformed push", () => {
    // @ts-expect-error — deliberately wrong shape (a caller bug / old blob).
    flowPush({ kb: "canon" });
    expect(flowList()).toEqual([]);
  });

  it("caps at FLOW_CAP, dropping the OLDEST", () => {
    for (let i = 0; i < FLOW_CAP + 5; i++) {
      flowPush(entry({ sourceRelative: `p${i}.html`, title: `T${i}` }));
    }
    const list = flowList();
    expect(list).toHaveLength(FLOW_CAP);
    expect(list[0].title).toBe("T5");
    expect(list[list.length - 1].title).toBe(`T${FLOW_CAP + 4}`);
  });

  it("clears", () => {
    flowPush(entry());
    flowClear();
    expect(flowList()).toEqual([]);
  });
});

describe("flowDropAbove — the popover's jump-to-row", () => {
  beforeEach(() => {
    flowPush(entry({ sourceRelative: "a.html", title: "A" }));
    flowPush(entry({ sourceRelative: "b.html", title: "B" }));
    flowPush(entry({ sourceRelative: "c.html", title: "C" }));
  });

  it("depth 0 is a no-op returning the current top", () => {
    expect(flowDropAbove(0)?.title).toBe("C");
    expect(flowList()).toHaveLength(3);
  });

  it("drops the rows above the target so it becomes the top", () => {
    expect(flowDropAbove(2)?.title).toBe("A");
    expect(flowList().map((e) => e.title)).toEqual(["A"]);
  });

  it("empties the stack when the depth runs past the bottom", () => {
    expect(flowDropAbove(99)).toBeUndefined();
    expect(flowList()).toEqual([]);
  });
});

describe("corrupt / foreign storage", () => {
  it("degrades to empty on invalid JSON", () => {
    store["kb:flow:v1"] = "{not json";
    expect(flowList()).toEqual([]);
  });

  it("degrades to empty on a wrong schema version", () => {
    store["kb:flow:v1"] = JSON.stringify({ v: 2, stack: [entry()] });
    expect(flowList()).toEqual([]);
  });

  it("drops individual malformed rows, keeping the valid ones", () => {
    store["kb:flow:v1"] = JSON.stringify({
      v: 1,
      stack: [entry({ title: "keep" }), { kb: "canon" }, null, 7],
    });
    expect(flowList().map((e) => e.title)).toEqual(["keep"]);
  });

  it("survives a push on top of a corrupt blob", () => {
    store["kb:flow:v1"] = "garbage";
    flowPush(entry({ title: "fresh" }));
    expect(flowList().map((e) => e.title)).toEqual(["fresh"]);
  });
});

describe("snapshot stability (useSyncExternalStore contract)", () => {
  it("returns the SAME reference until the stack changes", () => {
    flowPush(entry());
    const a = flowSnapshot();
    const b = flowSnapshot();
    expect(a).toBe(b);
    flowPush(entry({ sourceRelative: "other.html" }));
    expect(flowSnapshot()).not.toBe(a);
  });

  it("notifies subscribers on every mutation and stops after unsubscribe", () => {
    const seen = vi.fn();
    const off = subscribeFlow(seen);
    flowPush(entry());
    flowPop();
    expect(seen).toHaveBeenCalledTimes(2);
    off();
    flowPush(entry());
    expect(seen).toHaveBeenCalledTimes(2);
  });
});

describe("the consume-once scroll seed", () => {
  it("round-trips for the artifact it names — exactly once", () => {
    writeFlowSeed({ kb: "canon", sourceRelative: "pm/00-summary.html", y: 812 });
    expect(consumeFlowSeed("canon", "pm/00-summary.html")).toEqual({
      kb: "canon",
      sourceRelative: "pm/00-summary.html",
      y: 812,
    });
    // Consumed — a second read (e.g. a remount) must NOT re-seed.
    expect(consumeFlowSeed("canon", "pm/00-summary.html")).toBeNull();
  });

  it("leaves a seed for a DIFFERENT artifact in place", () => {
    writeFlowSeed({ kb: "canon", sourceRelative: "pm/00-summary.html", y: 5 });
    expect(consumeFlowSeed("canon", "other.html")).toBeNull();
    expect(consumeFlowSeed("research", "pm/00-summary.html")).toBeNull();
    expect(consumeFlowSeed("canon", "pm/00-summary.html")?.y).toBe(5);
  });

  it("the newest seed wins (only one return is ever in flight)", () => {
    writeFlowSeed({ kb: "canon", sourceRelative: "a.html", y: 1 });
    writeFlowSeed({ kb: "canon", sourceRelative: "b.html", y: 2 });
    expect(consumeFlowSeed("canon", "a.html")).toBeNull();
    expect(consumeFlowSeed("canon", "b.html")?.y).toBe(2);
  });

  it("drops a corrupt seed instead of throwing", () => {
    store["kb:flow:seed"] = "{oops";
    expect(consumeFlowSeed("canon", "a.html")).toBeNull();
    expect(store["kb:flow:seed"]).toBeUndefined();
  });

  it("ignores a malformed write", () => {
    // @ts-expect-error — deliberately wrong shape.
    writeFlowSeed({ kb: "canon" });
    expect(consumeFlowSeed("canon", "a.html")).toBeNull();
  });
});

describe("labels", () => {
  it("prefers the title, falls back to the filename", () => {
    expect(flowLabel(entry({ title: "Summary" }))).toBe("Summary");
    expect(flowLabel(entry({ title: "" }))).toBe("00-summary.html");
    expect(flowLabel(entry({ title: "", sourceRelative: "root.html" }))).toBe(
      "root.html",
    );
  });

  it("truncates with an ellipsis, never below the cap", () => {
    expect(truncateLabel("short", 10)).toBe("short");
    expect(truncateLabel("0123456789abc", 10)).toBe("012345678…");
    expect(truncateLabel("0123456789abc", 10)).toHaveLength(10);
  });
});
