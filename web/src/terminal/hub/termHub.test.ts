import { describe, expect, it } from "vitest";
import type { TermRow } from "../../api/terms";
import type { LiveStatusRow } from "../../api/generated/LiveStatusRow";
import type { LiveState } from "../../api/generated/LiveState";
import { openTermParam } from "../../components/TermList";
import {
  HUB_PATH,
  buildHubUrl,
  filterHub,
  hubFilterOptions,
  joinHub,
  needsYouIds,
  orderHub,
  parseHubUrl,
  type HubFilter,
  type HubPrefs,
  type HubRow,
  type HubUrl,
} from "./termHub";

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

function live(sid: string, o: Partial<LiveStatusRow> = {}): LiveStatusRow {
  return {
    session_id: sid,
    harness: "claude",
    holder: "agent",
    state: "waiting",
    source: "hook",
    confidence: "exact",
    since_unix: 500,
    since_secs: 10,
    resume: "r",
    blocked: false,
    ...o,
  } as LiveStatusRow;
}

const NOPREFS: HubPrefs = { labels: {}, pins: new Set() };
const prefs = (labels: Record<string, string>, pins: string[]): HubPrefs => ({
  labels,
  pins: new Set(pins),
});

describe("joinHub", () => {
  it("joins bound terms, keeps unbound, drops orphan live rows", () => {
    const rows = joinHub(
      [term("a"), term("b", { session_id: null }), term("c", { session_id: undefined })],
      [live("s-a", { title: "Fix it" }), live("s-zzz")],
      NOPREFS,
    );
    expect(rows.map((r) => r.term.id)).toEqual(["a", "b", "c"]);
    expect(rows[0].live?.session_id).toBe("s-a");
    expect(rows[0].title).toBe("Fix it");
    expect(rows[1].live).toBeNull();
    expect(rows[1].title).toBe("starting…");
    expect(rows[2].live).toBeNull();
  });

  it("falls back to the project name when bound but no live row", () => {
    const [r] = joinHub([term("a", { cwd: "/srv/app/" })], [], NOPREFS);
    expect(r.title).toBe("app");
    expect(r.project).toBe("app");
  });

  it("project is '/' for the root or an empty cwd", () => {
    const rows = joinHub([term("a", { cwd: "/" }), term("b", { cwd: "" })], [], NOPREFS);
    expect(rows.map((r) => r.project)).toEqual(["/", "/"]);
  });

  it("first live match wins on a duplicate session id", () => {
    const [r] = joinHub([term("a")], [live("s-a", { title: "one" }), live("s-a", { title: "two" })], NOPREFS);
    expect(r.live?.title).toBe("one");
  });

  it("label wins the title; pin flag is carried", () => {
    const [r] = joinHub([term("a")], [live("s-a", { title: "t" })], prefs({ a: "mine" }, ["a"]));
    expect(r.title).toBe("mine");
    expect(r.label).toBe("mine");
    expect(r.pinned).toBe(true);
  });
});

describe("tier table", () => {
  const deadOpts = [false, true];
  const blockedSrc = ["none", "term", "live"] as const;
  const states: Array<LiveState | null> = [null, "working", "stalled", "waiting", "cold"];
  for (const dead of deadOpts)
    for (const b of blockedSrc)
      for (const pinned of [false, true])
        for (const st of states) {
          const expected = dead
            ? "ended"
            : b !== "none"
              ? "needs"
              : pinned
                ? "pinned"
                : st === "working" || st === "stalled"
                  ? "working"
                  : "idle";
          it(`dead=${dead} blocked=${b} pinned=${pinned} state=${st} -> ${expected}`, () => {
            const t = term("a", { pane_dead: dead, blocked: b === "term", state: st });
            const l = live("s-a", { blocked: b === "live", state: st ?? "waiting" });
            const [r] = joinHub([t], st === null && b !== "live" ? [] : [l], prefs({}, pinned ? ["a"] : []));
            expect(r.tier).toBe(expected);
          });
        }

  it("needsSince falls back blocked_since -> since -> started", () => {
    const t = term("a", { blocked: true, started_unix: 7 });
    expect(joinHub([t], [], NOPREFS)[0].needsSince).toBe(7);
    expect(joinHub([t], [live("s-a", { blocked: true })], NOPREFS)[0].needsSince).toBe(500);
    expect(
      joinHub([t], [live("s-a", { blocked: true, blocked_since_unix: 42 })], NOPREFS)[0].needsSince,
    ).toBe(42);
    expect(joinHub([term("b")], [], NOPREFS)[0].needsSince).toBeNull();
  });
});

function fixture(): HubRow[] {
  const terms = [
    term("n-old", { blocked: true, started_unix: 10 }),
    term("n-new", { blocked: true, started_unix: 20 }),
    term("pin-b", { started_unix: 5 }),
    term("pin-a", { started_unix: 6 }),
    term("w-old", { state: "working", started_unix: 100 }),
    term("w-new", { state: "working", started_unix: 200 }),
    term("idle", { started_unix: 300 }),
    term("dead", { pane_dead: true, started_unix: 999 }),
  ];
  const lv = [
    live("s-n-old", { blocked: true, blocked_since_unix: 50 }),
    live("s-n-new", { blocked: true, blocked_since_unix: 90 }),
  ];
  return joinHub(terms, lv, prefs({ "pin-a": "Alpha", "pin-b": "Beta", dead: "x" }, ["pin-a", "pin-b", "dead"]));
}

describe("orderHub", () => {
  const want = ["n-old", "n-new", "pin-a", "pin-b", "w-new", "w-old", "idle", "dead"];
  it("orders every tier, ended last, pin never above needs", () => {
    const { rows, endedStart } = orderHub(fixture());
    expect(rows.map((r) => r.term.id)).toEqual(want);
    expect(endedStart).toBe(7);
    expect(rows[7].pinned).toBe(true);
    expect(rows[7].tier).toBe("ended");
  });

  it("is invariant under every permutation of the input", () => {
    const base = fixture();
    const perms: HubRow[][] = [];
    const permute = (a: HubRow[], n: number): void => {
      if (n === 1) {
        perms.push([...a]);
        return;
      }
      for (let i = 0; i < n; i++) {
        permute(a, n - 1);
        const j = n % 2 === 0 ? i : 0;
        [a[j], a[n - 1]] = [a[n - 1], a[j]];
      }
    };
    permute([...base], base.length);
    expect(perms.length).toBe(40320);
    for (const p of perms) expect(orderHub(p).rows.map((r) => r.term.id)).toEqual(want);
  });

  it("endedStart is the length when nothing is ended", () => {
    const rows = joinHub([term("a"), term("b")], [], NOPREFS);
    expect(orderHub(rows).endedStart).toBe(2);
  });

  it("ties break by id", () => {
    const rows = joinHub([term("b"), term("a")], [], NOPREFS);
    expect(orderHub(rows).rows.map((r) => r.term.id)).toEqual(["a", "b"]);
  });

  it("does not depend on since_secs or any clock", () => {
    const mk = (secs: number): string[] => {
      const rows = joinHub(
        [term("a", { state: "working" }), term("b", { state: "working" }), term("c", { blocked: true })],
        [live("s-a", { since_secs: secs }), live("s-b", { since_secs: 1e6 - secs }), live("s-c", { since_secs: secs * 3 })],
        NOPREFS,
      );
      return orderHub(rows).rows.map((r) => r.term.id);
    };
    const a = mk(1);
    expect(mk(99999)).toEqual(a);
    expect(mk(500)).toEqual(a);
  });

  it("needsYouIds is oldest block first", () => {
    expect(needsYouIds(fixture())).toEqual(["n-old", "n-new"]);
  });
});

describe("filters", () => {
  const rows = joinHub(
    [
      term("a", { cwd: "/w/one", harness: "claude", blocked: true }),
      term("b", { cwd: "/w/one", harness: "omp" }),
      term("c", { cwd: "/w/two", harness: "claude" }),
      term("d", { cwd: "/w/two", harness: "codex", blocked: true }),
    ],
    [],
    NOPREFS,
  );
  const f = (p: Partial<HubFilter>): string[] =>
    filterHub(rows, { project: null, harness: null, needs: false, ...p }).map((r) => r.term.id);
  it("filters singly and ANDed", () => {
    expect(f({})).toEqual(["a", "b", "c", "d"]);
    expect(f({ project: "one" })).toEqual(["a", "b"]);
    expect(f({ harness: "claude" })).toEqual(["a", "c"]);
    expect(f({ needs: true })).toEqual(["a", "d"]);
    expect(f({ project: "two", needs: true })).toEqual(["d"]);
    expect(f({ project: "one", harness: "claude", needs: true })).toEqual(["a"]);
    expect(f({ project: "one", harness: "codex" })).toEqual([]);
  });
  it("options are sorted, unique, non-empty", () => {
    expect(hubFilterOptions(rows)).toEqual({
      projects: ["one", "two"],
      harnesses: ["claude", "codex", "omp"],
    });
    expect(hubFilterOptions([])).toEqual({ projects: [], harnesses: [] });
  });
});

describe("URL builder", () => {
  it("goldens", () => {
    expect(HUB_PATH).toBe("/terminals");
    expect(buildHubUrl({})).toBe("/terminals");
    expect(buildHubUrl()).toBe("/terminals");
    expect(buildHubUrl({ id: "abc12" })).toBe("/terminals/abc12");
    expect(buildHubUrl({ id: "Bad/Id" })).toBe("/terminals");
    expect(buildHubUrl({ filter: { project: "my app", harness: "omp", needs: true } })).toBe(
      "/terminals?project=my+app&harness=omp&needs=1",
    );
    expect(buildHubUrl({ id: "t1", filter: { needs: true } })).toBe("/terminals/t1?needs=1");
    expect(buildHubUrl({ launch: true, resume: "sess-1.a" })).toBe("/terminals?launch=1&resume=sess-1.a");
    expect(buildHubUrl({ resume: "sess-1" })).toBe("/terminals");
    expect(buildHubUrl({ launch: true, resume: "bad value" })).toBe("/terminals?launch=1");
    expect(buildHubUrl({ filter: { harness: "nope" } })).toBe("/terminals");
    expect(
      buildHubUrl({ filter: { needs: true, harness: "claude", project: "p" }, launch: true, resume: "x" }),
    ).toBe("/terminals?project=p&harness=claude&needs=1&launch=1&resume=x");
  });
  it("never emits a cwd", () => {
    const u = buildHubUrl({ launch: true, resume: "x", filter: { project: "/etc" } } as never);
    expect(u).not.toMatch(/cwd/);
  });
});

describe("parseHubUrl", () => {
  const D: HubUrl = {
    id: null,
    filter: { project: null, harness: null, needs: false },
    launch: false,
    resume: null,
  };
  it("parses the grammar", () => {
    expect(parseHubUrl("/terminals", "")).toEqual(D);
    expect(parseHubUrl("/terminals/", "?needs=1").filter.needs).toBe(true);
    expect(parseHubUrl("/terminals/abc", "project=p&harness=codex&needs=1")).toEqual({
      id: "abc",
      filter: { project: "p", harness: "codex", needs: true },
      launch: false,
      resume: null,
    });
    expect(parseHubUrl("/terminals", new URLSearchParams("launch=1&resume=s1")).resume).toBe("s1");
  });
  it("degrades malformed input to defaults", () => {
    expect(parseHubUrl("/elsewhere/abc", "")).toEqual(D);
    expect(parseHubUrl("/terminals/ABC", "")).toEqual(D);
    expect(parseHubUrl("/terminals/a/b", "")).toEqual(D);
    expect(parseHubUrl("/terminals/new", "").id).toBe("new"); // a valid id shape; the route decides
    expect(parseHubUrl("/terminals", "needs=yes&launch=true&harness=zzz").filter).toEqual(D.filter);
    expect(parseHubUrl("/terminals", "resume=s1").resume).toBeNull();
    expect(parseHubUrl("/terminals", "launch=1&resume=a b").resume).toBeNull();
    expect(parseHubUrl("/terminals", "project=a%00b").filter.project).toBeNull();
    expect(parseHubUrl("/terminals", "project=" + "x".repeat(300)).filter.project).toBeNull();
  });
  it("first of a repeated param wins", () => {
    expect(parseHubUrl("/terminals", "harness=omp&harness=codex").filter.harness).toBe("omp");
  });

  const hostile: Array<[unknown, unknown]> = [
    ["", ""], ["/", "?"], ["//terminals", "x"], ["/terminals//", "&&&"], ["/terminals/%", "%"],
    ["/terminals/%zz", "project=%zz&resume=%E0%A4%A"], ["/terminals/\u0000", "\u0000"],
    ["/terminals/" + "a".repeat(100000), "project=" + "b".repeat(100000)],
    [null, null], [undefined, undefined], [42, 42], [{}, []], [[], {}], [Symbol.iterator, 1],
    ["/terminals", "launch=1&resume=" + "r".repeat(1e5)], ["/terminals", "=&=&=="],
    ["/terminals", "needs=1&needs=0&needs"], ["/Terminals", ""], ["/terminals/-a", ""],
    ["/terminals?x", "?"], ["/terminals#a", ""], ["\n/terminals", "\n"], ["/terminals/a\n", ""],
    ["/terminals", "project[]=a"], ["/terminals", "__proto__=x&constructor=y"],
    ["/terminals", "launch=1&resume=__proto__"], ["/terminals", "%FF%FE"], ["/terminals", "a=b;c=d"],
    ["/terminals", "launch=1&resume=../../etc/passwd"], ["/terminals", "harness=../../x"],
    ["/terminals", "\ud800"], ["/terminals/\ud800", ""], ["/terminals", "+++"],
  ];
  it("is total over hostile inputs", () => {
    expect(hostile.length).toBeGreaterThanOrEqual(30);
    for (const [p, s] of hostile) {
      expect(() => parseHubUrl(p as string, s as string)).not.toThrow();
      const r = parseHubUrl(p as string, s as string);
      expect(Object.keys(r).sort()).toEqual(["filter", "id", "launch", "resume"]);
    }
  });
  it("is total over 500 seeded random strings", () => {
    let seed = 12345;
    const rnd = (): number => {
      seed = (seed * 1664525 + 1013904223) >>> 0;
      return seed / 2 ** 32;
    };
    const alphabet = "abcXYZ019-._~%/?&=+# \u0000\né😀\\<>'\"";
    const str = (): string => {
      let s = "";
      const n = Math.floor(rnd() * 40);
      for (let i = 0; i < n; i++) s += alphabet[Math.floor(rnd() * alphabet.length)];
      return s;
    };
    for (let i = 0; i < 500; i++) {
      const p = rnd() < 0.5 ? "/terminals/" + str() : str();
      const s = rnd() < 0.5 ? "launch=1&resume=" + str() + "&" + str() : str();
      expect(() => parseHubUrl(p, s)).not.toThrow();
    }
  });

  it("H4: no cwd and no unknown harness survives an adversarial query", () => {
    const qs = [
      "launch=1&resume=x&cwd=/etc&harness=../../x",
      "cwd=/etc&launch=1&resume=x&harness=evil",
      "launch=1&resume=x&cwd=%2Fetc%2Fpasswd&harness=claude;rm",
      "project=/etc&launch=1&cwd=/root",
    ];
    for (const q of qs) {
      const r = parseHubUrl("/terminals", q);
      const ser = JSON.stringify(r);
      expect(ser).not.toContain("/etc");
      expect(ser).not.toContain("/root");
      expect(ser).not.toContain("evil");
      expect(ser).not.toContain("..");
      expect(ser).not.toContain("cwd");
      expect(r.filter.harness === null || r.filter.harness === "claude").toBe(true);
      expect(buildHubUrl(r)).not.toMatch(/cwd|evil|\.\./);
    }
  });
});

describe("round trip", () => {
  it("parse(build(u)) equals the normalised u", () => {
    const ids = [null, "a", "term-1", "x".repeat(32)];
    const projects = [null, "app", "my app", "a&b=c", "é"];
    const harnesses = [null, "claude", "omp"];
    const resumes = [null, "s1", "sess.1-a_b"];
    let n = 0;
    for (const id of ids)
      for (const project of projects)
        for (const harness of harnesses)
          for (const needs of [false, true])
            for (const launch of [false, true])
              for (const resume of resumes) {
                const u: HubUrl = { id, filter: { project, harness, needs }, launch, resume };
                const url = buildHubUrl(u);
                const [path, search = ""] = url.split("?");
                const expected: HubUrl = { ...u, resume: launch ? resume : null };
                expect(parseHubUrl(path, search)).toEqual(expected);
                n++;
              }
    expect(n).toBe(4 * 5 * 3 * 2 * 2 * 3);
  });
});

describe("id grammar agrees with openTermParam", () => {
  it("on a corpus", () => {
    const corpus = [
      "a", "0", "abc-1", "-a", "A", "a_b", "a b", "", "a".repeat(32), "a".repeat(33), "ab/c",
      "é", "a.b", "1-2-3", "z".repeat(31), "a\n", "new",
    ];
    for (const c of corpus) {
      const viaParam = openTermParam(c);
      expect(parseHubUrl(`/terminals/${c}`, "").id).toBe(c.includes("/") ? null : viaParam);
      expect(buildHubUrl({ id: c })).toBe(viaParam === null ? "/terminals" : `/terminals/${c}`);
    }
  });
});
