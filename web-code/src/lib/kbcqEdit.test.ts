import { describe, expect, it } from "vitest";
import { FILTER_SPECS, GROUP_KEYS, LANE_ORDER, parse } from "./kbcq";
import {
  appendClause,
  hasClause,
  LANE_PREFIXES,
  removeClause,
  setFacets,
  setGroup,
  setLane,
  splitClause,
  stripLanePrefix,
  toggleClause,
} from "./kbcqEdit";

describe("kbcqEdit — the one query writer", () => {
  /// THE dead-surface walk for this module. Every key the grammar declares
  /// must be a key this writer can toggle: a facet row (or a chip, or a
  /// grouping control) that appends a clause the writer cannot then SEE is
  /// a control that silently stops working the moment you click it twice.
  it("can round-trip a clause for every declared filter key", () => {
    for (const spec of FILTER_SPECS) {
      const value = spec.values ? spec.values[0] : "probe";
      const clause = `${spec.key}:${value}`;
      const withIt = appendClause("needle", clause);
      expect(parse(withIt).diagnostics, `${clause} did not parse cleanly`).toEqual([]);
      expect(hasClause(withIt, clause), `hasClause is blind to ${clause}`).toBe(true);
      // Appending twice is a no-op — a facet click is idempotent.
      expect(appendClause(withIt, clause)).toBe(withIt);
      const without = removeClause(withIt, clause);
      expect(hasClause(without, clause), `removeClause left ${clause} behind`).toBe(false);
      expect(parse(without).query).toBe("needle");
    }
  });

  it("toggles a clause on and back off", () => {
    const on = toggleClause("order", "lang:ruby");
    expect(on).toBe("order lang:ruby");
    expect(toggleClause(on, "lang:ruby")).toBe("order");
  });

  /// The whole point of parsing rather than substring-testing: text inside
  /// a quoted phrase is a TERM, and only the parser knows that.
  it("does not mistake a quoted phrase for an active filter", () => {
    const q = 'find "lang:ruby" in docs';
    expect(hasClause(q, "lang:ruby")).toBe(false);
    expect(appendClause(q, "lang:ruby")).toBe('find "lang:ruby" in docs lang:ruby');
  });

  it("removes only the exact token, never a term that contains it", () => {
    expect(removeClause("pathfinder path:app/", "path:app/")).toBe("pathfinder");
    // An alternation is not narrowed by guesswork — dropping `ext:rb` from
    // `ext:rb|erb` would silently lose `erb` too.
    expect(removeClause("x ext:rb|erb", "ext:rb")).toBe("x ext:rb|erb");
  });

  it("writes a lane prefix for every lane, idempotently", () => {
    for (const lane of LANE_ORDER) {
      const once = setLane("order", lane);
      expect(parse(once).lanes, `${lane} prefix did not select it`).toEqual([lane]);
      expect(parse(once).query).toBe("order");
      expect(setLane(once, lane)).toBe(once);
      // And a bare prefix (no query yet) still selects the lane.
      expect(parse(setLane("", lane)).lanes).toEqual([lane]);
      expect(LANE_PREFIXES[lane].length).toBeGreaterThan(0);
    }
  });

  it("leaves the query alone for a lane name this build does not know", () => {
    // An older SPA against a newer daemon that grew a seventh lane: the
    // facet row must be inert, never write `undefinedorder` into the box.
    expect(setLane("order", "structural" as unknown as (typeof LANE_ORDER)[number])).toBe("order");
  });

  it("replaces one lane prefix with another rather than stacking them", () => {
    expect(setLane(setLane("order", "symbols"), "files")).toBe("#order");
    expect(stripLanePrefix("~~gorgonzola")).toBe("gorgonzola");
    expect(stripLanePrefix("plain words")).toBe("plain words");
  });

  it("sets every group key and can clear the token entirely", () => {
    for (const key of GROUP_KEYS) {
      const q = setGroup("order", key);
      expect(parse(q).group, `group:${key} did not land`).toBe(key);
      // Switching groups replaces, never appends a second token.
      const swapped = setGroup(q, "lane");
      expect(parse(swapped).group).toBe("lane");
      expect(swapped.match(/group:/g)?.length).toBe(1);
    }
    // Clearing is DISTINCT from `group:none` — the grammar keeps them apart
    // and so must the writer.
    expect(setGroup("order group:dir", null)).toBe("order");
    expect(parse(setGroup("order", "none")).group).toBe("none");
  });

  it("turns facets on and off without disturbing the rest of the query", () => {
    const on = setFacets("order lang:ruby", true);
    expect(on).toBe("order lang:ruby facets:1");
    expect(setFacets(on, true)).toBe(on);
    expect(setFacets(on, false)).toBe("order lang:ruby");
  });

  it("splitClause rejects anything that is not a key:value token", () => {
    expect(splitClause("#")).toBeNull();
    expect(splitClause("plainword")).toBeNull();
    expect(splitClause("-path:spec/")).toEqual({ key: "path", value: "spec/", negated: true });
    // `Foo::bar` IS key-shaped, and the grammar splits it the same way —
    // what stops it from being a filter is that `Foo` is not a declared
    // key, which is a question for `hasClause`, not for the splitter. This
    // module must not grow a second opinion about the key set.
    expect(splitClause("Foo::bar")).toEqual({ key: "Foo", value: ":bar", negated: false });
    expect(hasClause("x Foo::bar", "Foo::bar")).toBe(false);
  });
});
