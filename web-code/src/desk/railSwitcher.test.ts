import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { RAIL_STRIPE_META, railStripeMeta } from "./Desk";
import { RAIL_TABS } from "./deskState";
import { TRAIL_MODE_LABEL } from "../components/trail/TrailIndicator";

// OL-P5 — on desktop the right Stripe is the ONE rail switcher: the panel's own
// tab strip is hidden there, so the Stripe must offer every home the strip did.
describe("the right Stripe is the rail's one desktop switcher", () => {
  it("offers every rail tab, in rail order", () => {
    expect(RAIL_STRIPE_META.map((m) => m.tab)).toEqual([...RAIL_TABS]);
  });

  it("gates Review like the old panel strip: hidden without a review context", () => {
    const tabs = (ctx: boolean, sel: (typeof RAIL_TABS)[number]) =>
      railStripeMeta(ctx, sel).map((m) => m.tab);
    expect(tabs(false, "all")).toEqual(RAIL_TABS.filter((t) => t !== "review"));
    expect(tabs(true, "all")).toEqual([...RAIL_TABS]);
    // a persisted Review selection never silently turns into another tab
    expect(tabs(false, "review")).toEqual([...RAIL_TABS]);
  });

  it("the panel's own strip is hidden on desktop only (the mobile sheet keeps it)", () => {
    const css = readFileSync(new URL("../styles/provenance.css", import.meta.url), "utf8");
    expect(css).toMatch(
      /\.kbc-desk:not\(\[data-desk-mobile\]\)\s+\.kbc-inspector__icons\s*\{\s*display:\s*none;/,
    );
  });
});

describe("the trail indicator reads as a self-explaining chip", () => {
  it("every mode has a capitalised visible label", () => {
    expect(TRAIL_MODE_LABEL).toEqual({
      off: "Trail off",
      recording: "Recording",
      paused: "Trail paused",
    });
  });
});
