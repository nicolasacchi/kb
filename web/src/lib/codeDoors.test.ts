import { describe, expect, it } from "vitest";
import {
  buildCodeDoors,
  codeDoorFor,
  doorRepo,
  doorsLive,
  httpCodeUrl,
  rehypeCodeDoors,
  type CodeDoor,
  type DoorCtx,
} from "./codeDoors";
import { lensOf, ref, row } from "./codeDoors.fixtures";

const CODE = "https://code.example";

const CFG = ref({
  ordinal: 0,
  kind: "path_line",
  raw: "crates/kb-core/src/config.rs:120",
  path_hint: "crates/kb-core/src/config.rs",
  line_start: 120,
});
const BARE = ref({
  ordinal: 1,
  kind: "path",
  raw: "crates/kb-core/src/bare.rs",
  path_hint: "crates/kb-core/src/bare.rs",
});
const SYM = ref({
  ordinal: 2,
  kind: "symbol_method",
  raw: "Algolia::SearchService#listable",
  symbol_container: "Algolia::SearchService",
  symbol_member: "listable",
});
const ISSUE = ref({ ordinal: 3, kind: "issue", raw: "https://github.com/a/b/issues/1", path_hint: "a/b" });
const EXT = ref({ ordinal: 4, kind: "external", raw: "vendor/gems/x.rb", path_hint: "vendor/gems/x.rb" });
const DECL = ref({
  ordinal: 5,
  kind: "path_line",
  raw: "crates/kb-core/src/coderefs.rs#L42",
  path_hint: "crates/kb-core/src/coderefs.rs",
  line_start: 42,
  declared: true,
});
const ALL = [CFG, BARE, SYM, ISSUE, EXT, DECL];

const base = (over: Partial<DoorCtx> = {}): DoorCtx => ({ codeUrl: CODE, repo: "app", ...over });

describe("httpCodeUrl", () => {
  it("accepts only absolute http(s)", () => {
    expect(httpCodeUrl(CODE)).toBe(CODE);
    expect(httpCodeUrl("http://127.0.0.1:4747")).toBe("http://127.0.0.1:4747");
    expect(httpCodeUrl(null)).toBeNull();
    expect(httpCodeUrl("/relative")).toBeNull();
    expect(httpCodeUrl("not a url")).toBeNull();
    expect(httpCodeUrl("javascript:alert(1)")).toBeNull();
    expect(httpCodeUrl("ftp://code.example")).toBeNull();
  });
});

describe("doorRepo truth table", () => {
  it("focused pane: ?repo= wins, then the pin", () => {
    expect(doorRepo({ focused: true, repoParam: "a", pinnedRepo: "b" })).toBe("a");
    expect(doorRepo({ focused: true, repoParam: null, pinnedRepo: "b" })).toBe("b");
    expect(doorRepo({ focused: true, repoParam: "", pinnedRepo: "b" })).toBe("b");
  });
  it("unfocused pane ignores ?repo=", () => {
    expect(doorRepo({ focused: false, repoParam: "a", pinnedRepo: "b" })).toBe("b");
    expect(doorRepo({ focused: false, repoParam: "a", pinnedRepo: null })).toBeNull();
  });
  it("never auto-selects a checkout", () => {
    expect(doorRepo({ focused: true, repoParam: null, pinnedRepo: null })).toBeNull();
    expect(doorRepo({ focused: true, repoParam: undefined, pinnedRepo: undefined })).toBeNull();
  });
});

describe("doorsLive truth table", () => {
  const ok = { codeUrl: CODE, activePage: "", historical: false };
  it("is live for a valid code_url on the entry page of a live render", () => {
    expect(doorsLive(ok)).toBe(true);
  });
  it("is dark without a valid code_url", () => {
    expect(doorsLive({ ...ok, codeUrl: null })).toBe(false);
    expect(doorsLive({ ...ok, codeUrl: "javascript:alert(1)" })).toBe(false);
    expect(doorsLive({ ...ok, codeUrl: "nope" })).toBe(false);
  });
  it("is dark on a ?p= sub-page and on a historical render", () => {
    expect(doorsLive({ ...ok, activePage: "chapter2.html" })).toBe(false);
    expect(doorsLive({ ...ok, historical: true })).toBe(false);
  });
});

describe("codeDoorFor - lens rows", () => {
  it("present + confirmed line -> exact reader door at the resolved line", () => {
    const lens = lensOf([
      row({
        ordinal: 0,
        kind: "path_line",
        raw: CFG.raw,
        path_state: "present",
        line_state: "confirmed",
        reader: { repo: "app", path: "crates/kb-core/src/config.rs", line: 120 },
      }),
    ]);
    const d = codeDoorFor(CFG, base({ lens }))!;
    expect(d.cls).toBe("exact");
    expect(d.href).toBe("https://code.example/r/app/crates/kb-core/src/config.rs?line=120");
    expect(d.ordinal).toBe(0);
    expect(d.raw).toBe(CFG.raw);
    expect(d.declared).toBe(false);
  });

  it("present, kind with no line -> exact", () => {
    const lens = lensOf([
      row({
        ordinal: 1,
        kind: "path",
        raw: BARE.raw,
        path_state: "present",
        reader: { repo: "app", path: "crates/kb-core/src/bare.rs", line: null },
      }),
    ]);
    const d = codeDoorFor(BARE, base({ lens }))!;
    expect(d.cls).toBe("exact");
    expect(d.href).toBe("https://code.example/r/app/crates/kb-core/src/bare.rs");
  });

  it("present + drifted -> moved, at the resolved line", () => {
    const lens = lensOf([
      row({
        ordinal: 0,
        kind: "path_line",
        raw: CFG.raw,
        path_state: "present",
        line_state: "drifted",
        resolved_line: 131,
        reader: { repo: "app", path: "crates/kb-core/src/config.rs", line: 131 },
      }),
    ]);
    const d = codeDoorFor(CFG, base({ lens }))!;
    expect(d.cls).toBe("moved");
    expect(d.href).toBe("https://code.example/r/app/crates/kb-core/src/config.rs?line=131");
  });

  it("present + unverifiable or line-absent -> unsure", () => {
    for (const line_state of ["unverifiable", "absent"] as const) {
      const lens = lensOf([
        row({
          ordinal: 0,
          kind: "path_line",
          raw: CFG.raw,
          path_state: "present",
          line_state,
          reader: { repo: "app", path: "crates/kb-core/src/config.rs", line: 120 },
        }),
      ]);
      expect(codeDoorFor(CFG, base({ lens }))!.cls).toBe("unsure");
    }
  });

  it("ambiguous / absent with a search -> unsure search door; title names the count or the miss", () => {
    const amb = lensOf([
      row({
        ordinal: 1,
        kind: "path",
        raw: BARE.raw,
        path_state: "ambiguous",
        candidate_count: 7,
        search: { q: "bare.rs", repo: "app" },
      }),
    ]);
    const a = codeDoorFor(BARE, base({ lens: amb }))!;
    expect(a.cls).toBe("unsure");
    expect(a.href).toBe("https://code.example/search?q=bare.rs&repo=app");
    expect(a.title).toContain("7 candidates");

    const abs = lensOf([
      row({
        ordinal: 1,
        kind: "path",
        raw: BARE.raw,
        path_state: "absent",
        search: { q: "bare.rs", repo: "app" },
        note: "no such file at HEAD",
      }),
    ]);
    const b = codeDoorFor(BARE, base({ lens: abs }))!;
    expect(b.cls).toBe("unsure");
    expect(b.title).toContain("not found in app");
    expect(b.title).toContain("no such file at HEAD");
  });

  it("ambiguous / absent without a search -> no door (never a fabricated hint over an answer)", () => {
    for (const path_state of ["ambiguous", "absent"] as const) {
      const lens = lensOf([row({ ordinal: 1, kind: "path", raw: BARE.raw, path_state })]);
      expect(codeDoorFor(BARE, base({ lens }))).toBeNull();
    }
  });

  it("a unique symbol hit -> exact reader door in the lens repo", () => {
    for (const symbol_state of ["hit_unique", "hit_container_matched"] as const) {
      const lens = lensOf([
        row({
          ordinal: 2,
          kind: "symbol_method",
          raw: SYM.raw,
          symbol_state,
          symbol_hit_count: 1,
          symbol_hits: [
            { path: "app/services/search.rb", line_start: 14, line_end: 20, kind: "method", container: null },
          ],
        }),
      ]);
      const d = codeDoorFor(SYM, base({ lens }))!;
      expect(d.cls).toBe("exact");
      expect(d.href).toBe("https://code.example/r/app/app/services/search.rb?line=14");
    }
  });

  it("an ambiguous or missing symbol -> no door", () => {
    const amb = lensOf([
      row({
        ordinal: 2,
        kind: "symbol_method",
        raw: SYM.raw,
        symbol_state: "hit_ambiguous",
        symbol_hit_count: 2,
        symbol_hits: [
          { path: "a.rb", line_start: 1, line_end: 2, kind: "method", container: null },
          { path: "b.rb", line_start: 1, line_end: 2, kind: "method", container: null },
        ],
      }),
    ]);
    expect(codeDoorFor(SYM, base({ lens: amb }))).toBeNull();
    expect(codeDoorFor(SYM, base({ lens: lensOf([row({ ordinal: 2, kind: "symbol_method", raw: SYM.raw })]) }))).toBeNull();
  });

  it("external path_state in the lens -> no door", () => {
    const lens = lensOf([row({ ordinal: 1, kind: "path", raw: BARE.raw, path_state: "external" })]);
    expect(codeDoorFor(BARE, base({ lens }))).toBeNull();
  });

  it("declared refs keep raw and declared verbatim", () => {
    const lens = lensOf([
      row({
        ordinal: 5,
        kind: "path_line",
        raw: DECL.raw,
        declared: true,
        path_state: "present",
        line_state: "confirmed",
        reader: { repo: "app", path: "crates/kb-core/src/coderefs.rs", line: 42 },
      }),
    ]);
    const d = codeDoorFor(DECL, base({ lens }))!;
    expect(d.raw).toBe("crates/kb-core/src/coderefs.rs#L42");
    expect(d.declared).toBe(true);
    const h = codeDoorFor(DECL, base())!;
    expect(h.raw).toBe("crates/kb-core/src/coderefs.rs#L42");
    expect(h.declared).toBe(true);
  });
});

describe("codeDoorFor - the hint door", () => {
  it("is kb-code's #file search, scoped to the repo when one is chosen", () => {
    const d = codeDoorFor(CFG, base({ repo: "app" }))!;
    expect(d.cls).toBe("hint");
    expect(d.href).toBe("https://code.example/search?q=%23crates%2Fkb-core%2Fsrc%2Fconfig.rs&repo=app");
    const u = codeDoorFor(CFG, base({ repo: null }))!;
    expect(u.href).toBe("https://code.example/search?q=%23crates%2Fkb-core%2Fsrc%2Fconfig.rs");
  });

  it("says why it is unverified and how to upgrade it", () => {
    const none = codeDoorFor(CFG, base({ repo: null }))!;
    expect(none.title).toContain("no checkout chosen");
    expect(none.title).toContain("pick a checkout in Links");
    const loading = codeDoorFor(CFG, base({ lensLoading: true }))!;
    expect(loading.title).toContain("not checked yet");
    const down = codeDoorFor(CFG, base({ lensError: new Error("Failed to fetch") }))!;
    expect(down.title).toContain("kb-code did not answer");
    const refused = codeDoorFor(
      CFG,
      base({ lensError: Object.assign(new Error("repo is indexing"), { reason: "repo_indexing" }) }),
    )!;
    expect(refused.title).toContain("repo is indexing");
  });

  it("never uses the word verified except negated", () => {
    const ctxs: DoorCtx[] = [
      base(),
      base({ repo: null }),
      base({ lensLoading: true }),
      base({ lensError: new Error("x") }),
      base({ lensError: Object.assign(new Error("m"), { reason: "r" }) }),
    ];
    for (const c of ctxs) {
      const t = codeDoorFor(CFG, c)!.title;
      expect(t.replace(/not verified/g, "")).not.toMatch(/verified/);
    }
  });

  it("has no door for a pathless symbol, an issue, an external ref or an empty raw", () => {
    expect(codeDoorFor(SYM, base())).toBeNull();
    expect(codeDoorFor(ISSUE, base())).toBeNull();
    expect(codeDoorFor(EXT, base())).toBeNull();
    expect(codeDoorFor({ ...CFG, raw: "" }, base())).toBeNull();
  });
});

describe("the never-fake-fresh table", () => {
  const staleLens = lensOf(
    ALL.map((r) => row({ ordinal: r.ordinal, kind: r.kind, raw: r.raw + "-older", path_state: "present", line_state: "confirmed", reader: { repo: "app", path: "x.rs", line: 1 } })),
  );
  const states: Array<[string, Partial<DoorCtx>]> = [
    ["lens undefined", { lens: undefined }],
    ["lens null", { lens: null }],
    ["an Error", { lensError: new Error("Failed to fetch") }],
    ["an Error with .reason", { lensError: Object.assign(new Error("m"), { reason: "r" }) }],
    ["loading", { lensLoading: true }],
    ["a lens whose row raw differs", { lens: staleLens }],
    ["a lens whose row declared differs", {
      lens: lensOf(ALL.map((r) => row({ ordinal: r.ordinal, kind: r.kind, raw: r.raw, declared: !r.declared, path_state: "present", reader: { repo: "app", path: "x.rs", line: 1 } }))),
    }],
  ];
  for (const [name, over] of states) {
    it(`never mints exact/moved/unsure for any fixture ref: ${name}`, () => {
      for (const r of ALL) {
        const d = codeDoorFor(r, base(over));
        if (d) expect(d.cls, `${r.raw}`).toBe("hint");
      }
    });
  }
});

describe("buildCodeDoors", () => {
  it("returns [] for a null, unparsable or javascript: code_url", () => {
    for (const codeUrl of [null, undefined, "nope", "javascript:alert(1)"]) {
      expect(buildCodeDoors(ALL, base({ codeUrl }))).toEqual([]);
    }
  });

  it("orders by ordinal and skips refs with nowhere to go", () => {
    const doors = buildCodeDoors([...ALL].reverse(), base());
    expect(doors.map((d) => d.ordinal)).toEqual([0, 1, 5]);
    expect(doors.every((d) => d.cls === "hint")).toBe(true);
  });
});

describe("rehypeCodeDoors", () => {
  type N = { type: string; tagName?: string; value?: string; properties?: Record<string, unknown>; children?: N[] };
  const t = (value: string): N => ({ type: "text", value });
  const code = (text: string): N => ({ type: "element", tagName: "code", children: [t(text)] });
  const el = (tagName: string, children: N[], properties: Record<string, unknown> = {}): N => ({
    type: "element",
    tagName,
    properties,
    children,
  });
  const door = (ordinal: number, raw: string, cls: CodeDoor["cls"] = "hint"): CodeDoor => ({
    ordinal,
    raw,
    declared: false,
    href: `https://code.example/search?q=${ordinal}`,
    cls,
    title: `door ${ordinal}`,
  });
  const run = (tree: N, doors: CodeDoor[]) => {
    rehypeCodeDoors(doors)(tree);
    return tree;
  };
  const anchors = (n: N): N[] =>
    (n.children ?? []).flatMap((c) => [...(c.tagName === "a" ? [c] : []), ...anchors(c)]);

  it("gives two identical inline codes their ordinals in order", () => {
    const tree = el("root", [
      el("p", [code("crates/kb-core/src/a.rs"), t(" and "), code("crates/kb-core/src/a.rs")]),
    ]);
    run(tree, [door(0, "crates/kb-core/src/a.rs"), door(3, "crates/kb-core/src/a.rs")]);
    expect(anchors(tree).map((a) => a.properties?.dataKbCoderef)).toEqual(["0", "3"]);
  });

  it("does not wrap a code inside a link but consumes its ordinal", () => {
    const tree = el("root", [
      el("p", [
        el("a", [code("crates/kb-core/src/a.rs")], { href: "https://x.example" }),
        t(" "),
        code("crates/kb-core/src/a.rs"),
      ]),
    ]);
    run(tree, [door(0, "crates/kb-core/src/a.rs"), door(1, "crates/kb-core/src/a.rs")]);
    const wrapped = anchors(tree).filter((a) => a.properties?.dataKbCoderef !== undefined);
    expect(wrapped.map((a) => a.properties?.dataKbCoderef)).toEqual(["1"]);
  });

  it("leaves a fenced block alone unless its whole text is the ref", () => {
    const listing = el("pre", [code("see crates/kb-core/src/a.rs:7 here\n")]);
    const whole = el("pre", [code("crates/kb-core/src/a.rs\n")]);
    const tree = el("root", [listing, whole]);
    run(tree, [door(0, "crates/kb-core/src/a.rs")]);
    expect(anchors(listing)).toHaveLength(0);
    expect(anchors(whole)).toHaveLength(1);
  });

  it("does not match a nested code twice", () => {
    const tree = el("root", [
      el("p", [{ type: "element", tagName: "code", children: [t("x "), code("crates/kb-core/src/a.rs")] }]),
    ]);
    run(tree, [door(0, "crates/kb-core/src/a.rs")]);
    expect(anchors(tree)).toHaveLength(0);
  });

  it("renders anchors carrying href, target, rel, title, class and ordinal", () => {
    const tree = el("root", [el("p", [code("crates/kb-core/src/a.rs")])]);
    run(tree, [door(7, "crates/kb-core/src/a.rs", "unsure")]);
    const a = anchors(tree)[0];
    expect(a.properties).toMatchObject({
      href: "https://code.example/search?q=7",
      target: "_blank",
      rel: ["noopener", "noreferrer"],
      title: "door 7",
      className: ["kb-coderef-door", "kb-coderef-door--unsure"],
      dataKbCoderef: "7",
    });
    expect(a.children?.[0].tagName).toBe("code");
  });

  it("is a no-op without doors and ignores declared doors", () => {
    const tree = el("root", [el("p", [code("crates/kb-core/src/a.rs")])]);
    const before = JSON.stringify(tree);
    rehypeCodeDoors(undefined)(tree);
    rehypeCodeDoors([])(tree);
    rehypeCodeDoors([{ ...door(0, "crates/kb-core/src/a.rs"), declared: true }])(tree);
    expect(JSON.stringify(tree)).toBe(before);
  });
});
