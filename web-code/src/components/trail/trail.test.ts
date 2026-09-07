import { describe, expect, it } from "vitest";
import { appendTrail, parseTrailLink, stripTrail } from "../../lib/codeUrl";
import { normalizeTrailMode, TRAIL_MODES } from "../../hooks/useTrails";
import { dwellLabel, trailStepLabel } from "./TrailRail";
import type { TrailStepOut } from "../../api/types";

// V74-L3b — the pure half of the trail surface: the link grammar's `src`
// discriminator, the fail-closed mode normaliser, and the two label functions.

function step(over: Partial<TrailStepOut> = {}): TrailStepOut {
  return {
    ordinal: 0,
    via: "manual",
    state: "pinned",
    dwell_secs: 0,
    day: "2026-09-07",
    ...over,
  };
}

describe("the `src` discriminator on the trail link grammar", () => {
  it("is OMITTED at its default, so every pre-V74-L3b link is byte-identical", () => {
    const base = "/r/kb/a.rb";
    expect(appendTrail(base, { id: "abc", step: 2 })).toBe("/r/kb/a.rb?trail=abc&step=2");
    expect(appendTrail(base, { id: "abc", step: 2, src: "local" })).toBe(
      "/r/kb/a.rb?trail=abc&step=2",
    );
  });

  it("is appended LAST, after `via`", () => {
    expect(
      appendTrail("/r/kb/a.rb", { id: "abc", step: 1, via: "usage_of", src: "tour" }),
    ).toBe("/r/kb/a.rb?trail=abc&step=1&via=usage_of&src=tour");
  });

  it("round-trips both server sources", () => {
    for (const src of ["trail", "tour"] as const) {
      const url = appendTrail("/r/kb/a.rb", { id: "x", step: 3, src });
      const link = parseTrailLink(new URLSearchParams(url.split("?")[1]));
      expect(link).toEqual({ id: "x", step: 3, src });
    }
  });

  it("degrades an unknown source to the DEFAULT rather than fabricating one", () => {
    const link = parseTrailLink(new URLSearchParams("trail=x&step=0&src=elsewhere"));
    // `src` absent means local — a link claiming a source this build cannot
    // resolve would render a chip pointing at nothing.
    expect(link).toEqual({ id: "x", step: 0 });
    expect(parseTrailLink(new URLSearchParams("trail=x&step=0&src=local"))).toEqual({
      id: "x",
      step: 0,
    });
  });

  it("is STRIPPED with the rest of the linkage, so two locations that differ only in it are one place", () => {
    const url = appendTrail("/r/kb/a.rb?line=4", { id: "x", step: 1, src: "tour" });
    expect(stripTrail(url)).toBe("/r/kb/a.rb?line=4");
  });
});

describe("normalizeTrailMode", () => {
  it("passes every mode the daemon declares", () => {
    for (const m of TRAIL_MODES) expect(normalizeTrailMode(m)).toBe(m);
  });

  it("fails CLOSED — an unknown mode reads as off, exactly as the daemon does", () => {
    // An indicator saying "recording" for a mode this build cannot interpret
    // is the one lie this surface exists to prevent.
    expect(normalizeTrailMode("something-a-newer-daemon-wrote")).toBe("off");
    expect(normalizeTrailMode(undefined)).toBe("off");
    expect(normalizeTrailMode(null)).toBe("off");
    expect(normalizeTrailMode("Recording")).toBe("off");
  });
});

describe("trailStepLabel", () => {
  it("names the place, and says so when there is no file", () => {
    expect(trailStepLabel(step({ path: "a.rb", line_start: 12 }))).toBe("a.rb:12");
    expect(trailStepLabel(step({ path: "a.rb" }))).toBe("a.rb");
    expect(trailStepLabel(step())).toBe("(no file)");
    expect(trailStepLabel(step({ path: "a.rb", symbol: "Order#total" }))).toContain("Order#total");
  });
});

describe("dwellLabel", () => {
  it("prints a zero dwell as the honest fact it is", () => {
    // A hop shorter than the daemon's granularity floor records the VISIT
    // with a zero dwell; hiding it would hide that the visit happened.
    expect(dwellLabel(0)).toBe("under the dwell floor");
    expect(dwellLabel(-1)).toBe("under the dwell floor");
  });

  it("reads seconds, then minutes", () => {
    expect(dwellLabel(12)).toBe("12s");
    expect(dwellLabel(60)).toBe("1m");
    expect(dwellLabel(95)).toBe("1m 35s");
  });
});
