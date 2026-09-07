import { describe, expect, it } from "vitest";
import { parseStep as parseBoardStep } from "./boardsUrl";
import {
  parseTourFlag,
  parseTourStep,
  parseToursStatus,
  tourHref,
  toursHref,
} from "./toursUrl";

// V74-L3b — the tour surface's URL grammar. Two families of test: the Location
// Contract's own rules (total parsers, params omitted at their default, a
// fixed order so one view produces one string), and the AGREEMENT with
// `lib/boardsUrl.ts` — the two surfaces share a step model on the server, so a
// `?step=` that meant different things on the two pages would be the drift the
// one-step-model ruling exists to prevent.

describe("parseTourStep", () => {
  it("is 1-based on the wire and 0-based internally", () => {
    expect(parseTourStep("1", 3)).toBe(0);
    expect(parseTourStep("3", 3)).toBe(2);
  });

  it("is TOTAL — junk reads as no step, never as step 0", () => {
    for (const raw of ["", "  ", "0", "-1", "4", "1.5", "abc", "01x", null]) {
      expect(parseTourStep(raw, 3), `${JSON.stringify(raw)}`).toBeNull();
    }
  });

  it("accepts JS-numeric spellings of an in-range step, exactly as the board parser does", () => {
    // `Number("1e0") === 1`, so this reads as step one on BOTH surfaces. Not a
    // defect and not a tour-only quirk: it is `Number()`'s own grammar, and
    // pinning it here is what stops a future "tighten the tour parser" edit
    // from silently making the two pages disagree about one URL.
    expect(parseTourStep("1e0", 3)).toBe(0);
    expect(parseTourStep("1e0", 3)).toBe(parseBoardStep("1e0", 3));
    expect(parseTourStep(" 2 ", 3)).toBe(parseBoardStep(" 2 ", 3));
  });

  it("agrees with the BOARD parser on every input", () => {
    const inputs = ["1", "2", "3", "0", "-1", "4", "", "x", null];
    for (const raw of inputs) {
      expect(parseTourStep(raw, 3), `${JSON.stringify(raw)}`).toBe(parseBoardStep(raw, 3));
    }
  });
});

describe("parseTourFlag", () => {
  it("accepts exactly the daemon's own truths", () => {
    expect(parseTourFlag("1")).toBe(true);
    expect(parseTourFlag("true")).toBe(true);
    expect(parseTourFlag("yes")).toBe(true);
    expect(parseTourFlag("0")).toBe(false);
    expect(parseTourFlag("on")).toBe(false);
    expect(parseTourFlag(null)).toBe(false);
  });
});

describe("parseToursStatus", () => {
  const statuses = ["pending", "draft", "accepted", "archived"];
  it("passes a known status and degrades an unknown one to every status", () => {
    expect(parseToursStatus("draft", statuses)).toBe("draft");
    expect(parseToursStatus("nonsense", statuses)).toBeNull();
    expect(parseToursStatus(null, statuses)).toBeNull();
    expect(parseToursStatus("", statuses)).toBeNull();
  });
});

describe("tourHref", () => {
  it("omits every param at its default", () => {
    expect(tourHref("kb", "checkout")).toBe("/r/kb/~tours/checkout");
    expect(tourHref("kb", "checkout", { step: null, ctx: false })).toBe("/r/kb/~tours/checkout");
  });

  it("emits step 1-based and in a FIXED order", () => {
    expect(tourHref("kb", "checkout", { step: 0 })).toBe("/r/kb/~tours/checkout?step=1");
    expect(tourHref("kb", "checkout", { step: 2, ctx: true })).toBe(
      "/r/kb/~tours/checkout?step=3&ctx=1",
    );
  });

  it("round-trips through the parser", () => {
    const href = tourHref("kb", "checkout", { step: 4, ctx: true });
    const qs = new URLSearchParams(href.split("?")[1]);
    expect(parseTourStep(qs.get("step"), 9)).toBe(4);
    expect(parseTourFlag(qs.get("ctx"))).toBe(true);
  });

  it("encodes a slug that would need it", () => {
    expect(tourHref("kb", "a/b")).toBe("/r/kb/~tours/a%2Fb");
  });
});

describe("toursHref", () => {
  it("carries a status only when one is filtered", () => {
    expect(toursHref("kb")).toBe("/r/kb/~tours");
    expect(toursHref("kb", null)).toBe("/r/kb/~tours");
    expect(toursHref("kb", "draft")).toBe("/r/kb/~tours?status=draft");
  });
});
