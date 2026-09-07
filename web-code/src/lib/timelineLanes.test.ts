import { describe, expect, it } from "vitest";
import { cycleLaneStep, laneLabel, TIMELINE_LANES, toggleLane } from "./timelineLanes";

describe("TIMELINE_LANES", () => {
  it("has exactly the eleven lanes review-timeline/2 defines", () => {
    expect(TIMELINE_LANES.length).toBe(11);
    expect(TIMELINE_LANES).toEqual([
      "lifecycle",
      "pr_body",
      "findings",
      "verdict",
      "comments",
      "wt_comments",
      "document",
      "report",
      "claims",
      "github",
      "turns",
    ]);
  });
});

describe("laneLabel", () => {
  it("has a human label for every lane", () => {
    for (const lane of TIMELINE_LANES) {
      expect(laneLabel(lane).length).toBeGreaterThan(0);
    }
  });

  it("degrades an unrecognized lane to itself", () => {
    expect(laneLabel("something_future")).toBe("something_future");
  });
});

describe("toggleLane", () => {
  it("adds a lane not yet hidden", () => {
    const out = toggleLane(new Set(), "comments");
    expect([...out]).toEqual(["comments"]);
  });

  it("removes a lane already hidden", () => {
    const out = toggleLane(new Set(["comments"]), "comments");
    expect([...out]).toEqual([]);
  });

  it("never mutates its input", () => {
    const input = new Set(["comments"]);
    toggleLane(input, "verdict");
    expect([...input]).toEqual(["comments"]);
  });
});

describe("cycleLaneStep", () => {
  it("toggles the lane at the cursor and advances it", () => {
    const step0 = cycleLaneStep(new Set(), 0);
    expect(step0.lane).toBe("lifecycle");
    expect([...step0.hidden]).toEqual(["lifecycle"]);
    expect(step0.cursor).toBe(1);

    const step1 = cycleLaneStep(step0.hidden, step0.cursor);
    expect(step1.lane).toBe("pr_body");
    expect([...step1.hidden].sort()).toEqual(["lifecycle", "pr_body"]);
    expect(step1.cursor).toBe(2);
  });

  it("wraps at the end of the lane list", () => {
    const last = TIMELINE_LANES.length - 1;
    const step = cycleLaneStep(new Set(), last);
    expect(step.lane).toBe(TIMELINE_LANES[last]);
    expect(step.cursor).toBe(0);
  });

  it("re-toggling the same lane after a full cycle un-hides it", () => {
    let hidden = new Set<string>();
    let cursor = 0;
    for (let i = 0; i < TIMELINE_LANES.length; i++) {
      const step = cycleLaneStep(hidden, cursor);
      hidden = step.hidden;
      cursor = step.cursor;
    }
    // Every lane toggled ON once — all eleven now hidden.
    expect(hidden.size).toBe(TIMELINE_LANES.length);
    // One more full pass toggles every lane back OFF.
    for (let i = 0; i < TIMELINE_LANES.length; i++) {
      const step = cycleLaneStep(hidden, cursor);
      hidden = step.hidden;
      cursor = step.cursor;
    }
    expect(hidden.size).toBe(0);
  });
});
