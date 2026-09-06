// V70-A6 — Trail v0's goldens (the pure half; the sessionStorage wrapper is
// exercised by `e2e/nav-ramp.spec.ts` against a real browser).
import { describe, expect, it } from "vitest";
import {
  appendStep,
  newTrail,
  originChipText,
  stepAt,
  TRAIL_STEPS_CAP,
  TRAIL_VIA,
  VIA_LABEL,
  visitCount,
  type Trail,
} from "./trail";
import { appendTrail, parseTrailLink, stripTrail } from "./codeUrl";

function base(): Trail {
  return newTrail("kb", "/r/kb/a.rs?line=1", "a.rs:1", 1000, "t1");
}

describe("the via vocabulary is one closed list", () => {
  it("every runtime kind has a display label", () => {
    for (const v of TRAIL_VIA) expect(VIA_LABEL[v]).toBeTypeOf("string");
  });
  it("has no label without a kind (the two are hand-mirrored)", () => {
    expect(Object.keys(VIA_LABEL).sort()).toEqual([...TRAIL_VIA].sort());
  });
});

describe("appendStep", () => {
  it("returns the ordinal the destination's `?step=` will carry", () => {
    let t = base();
    const a = appendStep(t, { from: "/x", fromLabel: "x", to: "/y", via: "usage_of" }, 1);
    expect(a.ordinal).toBe(0);
    t = a.trail;
    const b = appendStep(t, { from: "/y", fromLabel: "y", to: "/z", via: "blame" }, 2);
    expect(b.ordinal).toBe(1);
    expect(b.trail.steps).toHaveLength(2);
  });

  it("caps from the FRONT and keeps the new hop's ordinal valid", () => {
    let t = base();
    let last = { ordinal: -1 };
    for (let i = 0; i < TRAIL_STEPS_CAP + 5; i++) {
      const r = appendStep(t, { from: `/f${i}`, fromLabel: `f${i}`, to: `/t${i}` }, i);
      t = r.trail;
      last = r;
    }
    expect(t.steps).toHaveLength(TRAIL_STEPS_CAP);
    // The link we just minted still indexes the hop we just recorded.
    expect(stepAt(t, last.ordinal)?.to).toBe(`/t${TRAIL_STEPS_CAP + 4}`);
    // …and the oldest hops are gone, honestly: they resolve to null rather
    // than to somebody else's journey.
    expect(stepAt(t, TRAIL_STEPS_CAP + 10)).toBeNull();
  });
});

describe("originChipText", () => {
  it("renders the design's own example", () => {
    const step = {
      from: "/r/kb/app/models/order.rb?line=88",
      fromLabel: "app/models/order.rb:88",
      to: "/r/kb/app/services/x.rb",
      via: "usage_of" as const,
      subject: "Order#total",
      trust: "exact",
      at: 0,
    };
    expect(originChipText(step)).toBe("from app/models/order.rb:88 (usage of Order#total, exact)");
  });

  it("omits every part it was not given, and never invents one", () => {
    expect(originChipText({ from: "/a", fromLabel: "a.rs:1", to: "/b", at: 0 })).toBe("from a.rs:1");
    expect(
      originChipText({ from: "/a", fromLabel: "a.rs:1", to: "/b", via: "blame", at: 0 }),
    ).toBe("from a.rs:1 (blame)");
    expect(
      originChipText({ from: "/a", fromLabel: "a.rs:1", to: "/b", trust: "likely", at: 0 }),
    ).toBe("from a.rs:1 (likely)");
  });

  it("is null for a hop that could not be resolved — no chip beats a dead chip", () => {
    expect(originChipText(null)).toBeNull();
    expect(originChipText(stepAt(base(), 3))).toBeNull();
  });
});

describe("visitCount", () => {
  it("counts LANDINGS, not the places you were standing", () => {
    let t = base();
    t = appendStep(t, { from: "/r/kb/a.rs", fromLabel: "a", to: "/r/kb/b.rs?line=1" }, 1).trail;
    t = appendStep(t, { from: "/r/kb/b.rs", fromLabel: "b", to: "/r/kb/c.rs" }, 2).trail;
    t = appendStep(t, { from: "/r/kb/c.rs", fromLabel: "c", to: "/r/kb/b.rs?line=9" }, 3).trail;
    expect(visitCount(t, (u) => u.startsWith("/r/kb/b.rs"))).toBe(2);
    // `a.rs` is where the reader STARTED — never a landing, so never a visit.
    expect(visitCount(t, (u) => u.startsWith("/r/kb/a.rs"))).toBe(0);
    expect(visitCount(null, () => true)).toBe(0);
  });
});

describe("the `?trail=&step=&via=` grammar (lib/codeUrl.ts)", () => {
  it("is APPENDED, so every pre-A6 URL is byte-identical without it", () => {
    expect(appendTrail("/r/kb/a.rs?line=4", null)).toBe("/r/kb/a.rs?line=4");
    expect(appendTrail("/r/kb/a.rs?line=4", undefined)).toBe("/r/kb/a.rs?line=4");
  });
  it("appends after every existing param, in a fixed order", () => {
    expect(appendTrail("/r/kb/a.rs?line=4", { id: "t1", step: 2, via: "usage_of" })).toBe(
      "/r/kb/a.rs?line=4&trail=t1&step=2&via=usage_of",
    );
    expect(appendTrail("/r/kb/a.rs", { id: "t1", step: 0 })).toBe("/r/kb/a.rs?trail=t1&step=0");
  });
  it("refuses to emit an unknown via rather than inventing a kind", () => {
    expect(
      appendTrail("/x", { id: "t", step: 0, via: "telepathy" as unknown as "search" }),
    ).toBe("/x?trail=t&step=0");
  });
  it("clamps a nonsense step instead of writing it out", () => {
    expect(appendTrail("/x", { id: "t", step: -7 })).toBe("/x?trail=t&step=0");
    expect(appendTrail("/x", { id: "t", step: 2.7 })).toBe("/x?trail=t&step=2");
  });
  it("parses back, or refuses a half link", () => {
    expect(parseTrailLink(new URLSearchParams("trail=t1&step=2&via=blame"))).toEqual({
      id: "t1",
      step: 2,
      via: "blame",
    });
    expect(parseTrailLink(new URLSearchParams("trail=t1&step=2&via=nope"))).toEqual({
      id: "t1",
      step: 2,
    });
    expect(parseTrailLink(new URLSearchParams("step=2"))).toBeNull();
    expect(parseTrailLink(new URLSearchParams("trail=t1"))).toBeNull();
    expect(parseTrailLink(new URLSearchParams("trail=t1&step=x"))).toBeNull();
  });
  it("strips WITHOUT re-encoding the rest — a comparison must not rewrite bytes", () => {
    const url = "/r/kb/a.rs?line=4&pane2=src%2Fb.rs%40%3A7&trail=t&step=0&via=search";
    expect(stripTrail(url)).toBe("/r/kb/a.rs?line=4&pane2=src%2Fb.rs%40%3A7");
    expect(stripTrail("/r/kb/a.rs")).toBe("/r/kb/a.rs");
    expect(stripTrail("/r/kb/a.rs?trail=t&step=0")).toBe("/r/kb/a.rs");
  });
});
