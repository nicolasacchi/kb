// V76-R2a — the findings rail's reducer: resize/persist round-trips, the
// clamp ladder, corrupt-blob recovery, and the Desk-submode key routing.
// Runs in the node environment with an injected StorageLike (the
// `deskState.test.ts` posture — no DOM, no ambient).
import { describe, expect, it } from "vitest";
import {
  REVIEW_RAIL_DEFAULT,
  REVIEW_RAIL_DEFAULT_WIDTH,
  REVIEW_RAIL_MAX_WIDTH,
  REVIEW_RAIL_MIN_WIDTH,
  REVIEW_RAIL_STORAGE_KEY,
  applyRailResize,
  clampRailWidth,
  loadReviewRail,
  railKeyResize,
  railWidthFromLayout,
  resetRail,
  roomLayout,
  saveReviewRail,
  toggleRail,
  type ReviewRailState,
} from "./reviewRail";
import { RESIZE_FAR, RESIZE_STEP } from "../desk/resizeSubmode";
import type { StorageLike } from "../desk/deskState";

function memStorage(initial?: Record<string, string>): StorageLike & { map: Map<string, string> } {
  const map = new Map<string, string>(Object.entries(initial ?? {}));
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, String(v)),
    removeItem: (k) => void map.delete(k),
  };
}

describe("clampRailWidth", () => {
  it("clamps into [MIN, MAX] and defaults the non-finite", () => {
    expect(clampRailWidth(0)).toBe(REVIEW_RAIL_MIN_WIDTH);
    expect(clampRailWidth(999)).toBe(REVIEW_RAIL_MAX_WIDTH);
    expect(clampRailWidth(30)).toBe(30);
    expect(clampRailWidth(Number.NaN)).toBe(REVIEW_RAIL_DEFAULT_WIDTH);
    expect(clampRailWidth("wide")).toBe(REVIEW_RAIL_DEFAULT_WIDTH);
    expect(clampRailWidth(undefined)).toBe(REVIEW_RAIL_DEFAULT_WIDTH);
  });
});

describe("applyRailResize", () => {
  it("applies a delta through the clamp", () => {
    const s: ReviewRailState = { width: 25, collapsed: false };
    expect(applyRailResize(s, 5).width).toBe(30);
    expect(applyRailResize(s, -50).width).toBe(REVIEW_RAIL_MIN_WIDTH);
    expect(applyRailResize(s, 50).width).toBe(REVIEW_RAIL_MAX_WIDTH);
  });

  it("is a no-op on a collapsed rail", () => {
    const s: ReviewRailState = { width: 25, collapsed: true };
    expect(applyRailResize(s, 5)).toEqual(s);
  });
});

describe("railKeyResize — the Desk's resize submode, focus: rail", () => {
  const open: ReviewRailState = { width: 25, collapsed: false };

  it("ArrowLeft/h GROWS the rail (its boundary is on its left)", () => {
    expect(railKeyResize(open, "ArrowLeft").state.width).toBe(25 + RESIZE_STEP);
    expect(railKeyResize(open, "h").state.width).toBe(25 + RESIZE_STEP);
  });

  it("ArrowRight/l shrinks it; shifted keys take the FAR step", () => {
    expect(railKeyResize(open, "ArrowRight").state.width).toBe(25 - RESIZE_STEP);
    // 25 - FAR would land under MIN and clamp — start wider to see the FAR
    // step itself, and assert the clamp separately.
    expect(railKeyResize({ width: 40, collapsed: false }, "L").state.width).toBe(40 - RESIZE_FAR);
    expect(railKeyResize(open, "L").state.width).toBe(REVIEW_RAIL_MIN_WIDTH);
  });

  it("= resets to the default width (the submode's equalise)", () => {
    const wide: ReviewRailState = { width: 40, collapsed: false };
    const r = railKeyResize(wide, "=");
    expect(r.command.t).toBe("equalise");
    expect(r.state.width).toBe(REVIEW_RAIL_DEFAULT_WIDTH);
  });

  it("j/k are no-ops with focus on the rail (no boundary that way)", () => {
    const r = railKeyResize(open, "j");
    expect(r.command.t).toBe("noop");
    expect(r.handled).toBe(false);
    expect(r.state).toEqual(open);
  });

  it("Escape is claimed (exit) but changes no geometry", () => {
    const r = railKeyResize(open, "Escape");
    expect(r.command.t).toBe("exit");
    expect(r.handled).toBe(true);
    expect(r.state).toEqual(open);
  });
});

describe("toggleRail / resetRail", () => {
  it("toggle keeps the width; reset restores the default AND un-collapses", () => {
    const s: ReviewRailState = { width: 33, collapsed: false };
    expect(toggleRail(s)).toEqual({ width: 33, collapsed: true });
    expect(toggleRail(toggleRail(s))).toEqual(s);
    expect(resetRail()).toEqual({
      width: REVIEW_RAIL_DEFAULT_WIDTH,
      collapsed: false,
    });
  });
});

describe("persistence", () => {
  it("round-trips a state through StorageLike", () => {
    const store = memStorage();
    const s: ReviewRailState = { width: 31, collapsed: true };
    saveReviewRail(s, store);
    expect(store.map.has(REVIEW_RAIL_STORAGE_KEY)).toBe(true);
    expect(loadReviewRail(store)).toEqual(s);
  });

  it("degrades every bad blob to the default, never a throw", () => {
    expect(loadReviewRail(memStorage())).toEqual(REVIEW_RAIL_DEFAULT);
    expect(loadReviewRail(memStorage({ [REVIEW_RAIL_STORAGE_KEY]: "not json" }))).toEqual(
      REVIEW_RAIL_DEFAULT,
    );
    expect(loadReviewRail(memStorage({ [REVIEW_RAIL_STORAGE_KEY]: "42" }))).toEqual(
      REVIEW_RAIL_DEFAULT,
    );
    expect(loadReviewRail(null)).toEqual(REVIEW_RAIL_DEFAULT);
  });

  it("clamps a stored width and coerces a stored collapsed flag", () => {
    const store = memStorage({
      [REVIEW_RAIL_STORAGE_KEY]: JSON.stringify({ v: 1, width: 99, collapsed: 1 }),
    });
    expect(loadReviewRail(store)).toEqual({ width: REVIEW_RAIL_MAX_WIDTH, collapsed: false });
  });
});

describe("layout maps", () => {
  it("roomLayout sums to 100 and railWidthFromLayout reads it back", () => {
    const l = roomLayout(30);
    expect(l["room-main"] + l["room-rail"]).toBe(100);
    expect(railWidthFromLayout(l)).toBe(30);
  });

  it("railWidthFromLayout is null on a foreign layout and clamps a wild one", () => {
    expect(railWidthFromLayout({ dock: 18 })).toBeNull();
    expect(railWidthFromLayout({ "room-rail": 500 })).toBe(REVIEW_RAIL_MAX_WIDTH);
  });
});
