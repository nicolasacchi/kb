// V70-A6 — provisional panes: the rule, in one place.
import { describe, expect, it } from "vitest";
import {
  AUTO_PIN_AFTER,
  isProvisional,
  markProvisional,
  NO_PROVISIONAL,
  noteInteraction,
  paneModifiers,
  PANE_MODIFIER_CAP,
  pin,
} from "./provisionalPane";

describe("the preference is the switch, and it defaults OFF", () => {
  it("with the gradient off, a promotion yields an ordinary pinned pane", () => {
    expect(markProvisional(2, false)).toEqual(NO_PROVISIONAL);
  });
  it("with it on, the pane is marked", () => {
    const s = markProvisional(2, true);
    expect(isProvisional(s, 2)).toBe(true);
    expect(isProvisional(s, 1)).toBe(false);
  });
});

describe("auto-pin on the second interaction", () => {
  it("pins at exactly AUTO_PIN_AFTER", () => {
    let s = markProvisional(2, true);
    for (let i = 1; i < AUTO_PIN_AFTER; i++) {
      s = noteInteraction(s, 2);
      expect(isProvisional(s, 2)).toBe(true);
    }
    s = noteInteraction(s, 2);
    expect(s).toEqual(NO_PROVISIONAL);
  });

  it("an interaction with the OTHER pane is not a commitment to this one", () => {
    let s = markProvisional(2, true);
    s = noteInteraction(s, 1);
    s = noteInteraction(s, 1);
    expect(isProvisional(s, 2)).toBe(true);
  });

  it("`p` pins explicitly, and only the pane it names", () => {
    const s = markProvisional(2, true);
    expect(pin(s, 1)).toBe(s);
    expect(pin(s, 2)).toEqual(NO_PROVISIONAL);
  });
});

describe("the pane's chip row is capped at two, in a FIXED order", () => {
  it("caps", () => {
    expect(paneModifiers(["trail", "follow", "frame", "provisional"])).toHaveLength(
      PANE_MODIFIER_CAP,
    );
  });
  it("orders by the table, never by the caller's array", () => {
    // Spatial stability is the whole reason Patchworks beat Code Bubbles: a
    // chip row that reshuffles as state changes is a moving target.
    expect(paneModifiers(["trail", "provisional"])).toEqual(["provisional", "trail"]);
    expect(paneModifiers(["frame", "provisional"])).toEqual(["provisional", "frame"]);
  });
  it("renders nothing when nothing is active", () => {
    expect(paneModifiers([])).toEqual([]);
  });
});
