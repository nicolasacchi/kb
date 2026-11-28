import { describe, expect, it } from "vitest";
import type { TermRow } from "../../api/terms";
import type { LiveStatusRow } from "../../api/generated/LiveStatusRow";
import { joinHub, needsYouIds, orderHub, type HubPrefs } from "../hub/termHub";
import { buildSwitcher } from "./switcherModel";

function term(id: string, o: Partial<TermRow> = {}): TermRow {
  return {
    id,
    harness: "claude",
    cwd: `/work/${id}`,
    started_unix: 1000,
    resume_sid: null,
    pane_pid: 1,
    cols: 80,
    rows: 24,
    bracket_paste_flag: false,
    pane_dead: false,
    exit_code: null,
    grid_owner: null,
    conf_version: 1,
    anchor_pid: null,
    stale: false,
    session_id: `s-${id}`,
    state: null,
    blocked: false,
    ...o,
  };
}

const NO_PREFS: HubPrefs = { labels: {}, pins: new Set() };
const live: LiveStatusRow[] = [];

describe("buildSwitcher", () => {
  const terms = [
    term("a", { cwd: "/work/p", started_unix: 10 }),
    term("b", { cwd: "/work/p", started_unix: 20 }),
    term("c", { cwd: "/work/q", started_unix: 30 }),
    term("d", { cwd: "/work/p", started_unix: 40, pane_dead: true }),
  ];

  it("groups by the current row's project and excludes ended rows", () => {
    const m = buildSwitcher(terms, live, NO_PREFS, {}, "a");
    expect(m.groups.map((g) => g.key)).toEqual(["project", "everywhere"]);
    expect(m.groups[0].label).toBe("This project");
    expect(m.groups[0].rows.map((r) => r.id)).toEqual(["b", "a"]);
    expect(m.groups[1].label).toBe("Everywhere");
    expect(m.groups[1].rows.map((r) => r.id)).toEqual(["c"]);
    expect(m.flat.map((r) => r.id)).toEqual(["b", "a", "c"]);
    expect(m.count).toBe(3);
  });

  it("puts the needs-you row first inside its group and counts it", () => {
    const t = [...terms.slice(0, 3)];
    t[0] = term("a", { cwd: "/work/p", started_unix: 10, blocked: true });
    const m = buildSwitcher(t, live, NO_PREFS, {}, "b");
    expect(m.groups[0].rows.map((r) => r.id)).toEqual(["a", "b"]);
    expect(m.groups[0].rows[0]).toMatchObject({ asks: true, finished: false, tier: "needs" });
    expect(m.needs).toBe(1);
  });

  it("an asks mark from the attention store flags the row but never re-sorts or badges", () => {
    const m = buildSwitcher(terms.slice(0, 3), live, NO_PREFS, { c: { asks: true, finishedUnseen: false } }, "a");
    const ref = orderHub(joinHub(terms.slice(0, 3), live, NO_PREFS));
    expect(m.flat.map((r) => r.id).sort()).toEqual(ref.rows.slice(0, ref.endedStart).map((r) => r.term.id).sort());
    expect(m.flat.map((r) => r.id)).toEqual(["b", "a", "c"]);
    expect(m.flat.find((r) => r.id === "c")).toMatchObject({ asks: true, tier: expect.not.stringMatching(/^needs$/) });
    expect(m.needs).toBe(0);
  });

  it("asks beats finished; finished never shows on the current terminal", () => {
    const marks = {
      a: { asks: true, finishedUnseen: true },
      b: { asks: false, finishedUnseen: true },
      c: { asks: false, finishedUnseen: true },
    };
    const m = buildSwitcher(terms.slice(0, 3), live, NO_PREFS, marks, "b");
    const r = (id: string) => m.flat.find((x) => x.id === id);
    expect(r("a")).toMatchObject({ asks: true, finished: false });
    expect(r("b")).toMatchObject({ asks: false, finished: false });
    expect(r("c")).toMatchObject({ asks: false, finished: true });
  });

  it("needs counts tier-needs rows exactly like the hub door badge, current included", () => {
    const t = [term("a", { blocked: true }), term("b")];
    const door = needsYouIds(orderHub(joinHub(t, live, NO_PREFS)).rows).length;
    expect(buildSwitcher(t, live, NO_PREFS, {}, "a").needs).toBe(door);
    expect(buildSwitcher(t, live, NO_PREFS, {}, "b").needs).toBe(door);
    expect(door).toBe(1);
  });

  it("an unknown current id puts everything under Everywhere", () => {
    const m = buildSwitcher(terms, live, NO_PREFS, {}, "zzz");
    expect(m.groups.map((g) => g.key)).toEqual(["everywhere"]);
    expect(m.flat.every((r) => !r.current)).toBe(true);
    expect(buildSwitcher(terms, live, NO_PREFS, {}, null).groups.map((g) => g.key)).toEqual(["everywhere"]);
  });

  it("flat order equals orderHub's live order for a shuffled fixture without marks", () => {
    const many = [
      term("e", { cwd: "/w/x", started_unix: 5 }),
      term("f", { cwd: "/w/x", started_unix: 50, blocked: true }),
      term("g", { cwd: "/w/x", started_unix: 25, state: "working" }),
      term("h", { cwd: "/w/x", started_unix: 99 }),
    ];
    const shuffled = [many[2], many[0], many[3], many[1]];
    const ref = orderHub(joinHub(shuffled, live, NO_PREFS));
    const want = ref.rows.slice(0, ref.endedStart).map((r) => r.term.id);
    const m = buildSwitcher(shuffled, live, NO_PREFS, {}, "e");
    expect(m.flat.map((r) => r.id)).toEqual(want);
    expect(m.flat[0].id).toBe("f");
  });

  it("empty input yields no groups and zero counts", () => {
    const m = buildSwitcher([], live, NO_PREFS, {}, "a");
    expect(m).toMatchObject({ groups: [], flat: [], count: 0, needs: 0 });
  });
});
