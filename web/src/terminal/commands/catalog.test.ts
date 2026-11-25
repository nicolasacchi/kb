// CMDPURE — the pure command-catalogue logic. The merge cases live in
// merge.golden.json, which the Rust twin
// (`merge_matches_the_shared_golden`, kb-server `terminal/command_store.rs`)
// also loads, so the two merges cannot drift.

import { describe, expect, it } from "vitest";
import type { EffItem } from "../../api/generated/EffItem";
import { KEY_NAMES } from "../phone/keys";
import {
  effectiveCatalogue,
  forHarness,
  groupEntries,
  mergeCatalogue,
  searchEntries,
  toKeystrokes,
  type DefaultsDoc,
  type Effective,
  type Entry,
  type OverlayDoc,
} from "./catalog";
import { DEFAULT_CATALOGUE } from "./defaults";
import golden from "./merge.golden.json";
import { CMD_SCHEMA, validateCatalogue } from "./schema";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const nodeFs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string => decodeURIComponent(new URL(rel, import.meta.url).pathname);

interface Case {
  name: string;
  defaults: DefaultsDoc;
  overlay: OverlayDoc;
  effective: Effective;
  validates?: boolean;
}
const CASES = (golden as unknown as { cases: Case[] }).cases;

function deepFreeze<T>(o: T): T {
  if (typeof o === "object" && o !== null) {
    for (const v of Object.values(o)) deepFreeze(v);
    Object.freeze(o);
  }
  return o;
}
const clone = <T>(o: T): T => JSON.parse(JSON.stringify(o)) as T;

describe("merge golden", () => {
  it("merge_golden_has_at_least_14_cases_with_unique_names", () => {
    expect(CASES.length).toBeGreaterThanOrEqual(14);
    expect(new Set(CASES.map((c) => c.name)).size).toBe(CASES.length);
  });

  it("merge_golden_cases", () => {
    for (const c of CASES) {
      expect(mergeCatalogue(c.defaults, c.overlay), c.name).toEqual(c.effective);
    }
  });

  it("merge_does_not_mutate", () => {
    for (const c of CASES) {
      const d = deepFreeze(clone(c.defaults));
      const o = deepFreeze(clone(c.overlay));
      expect(mergeCatalogue(d, o), c.name).toEqual(c.effective);
      expect(d).toEqual(c.defaults);
      expect(o).toEqual(c.overlay);
    }
  });

  it("merge_output_shares_no_arrays_with_its_inputs", () => {
    const withArgs = {
      harnesses: {
        claude: {
          groups: [
            {
              id: "g",
              label: "G",
              items: [{ id: "a", kind: "command", label: "A", text: "x {p}", args: [{ name: "p", hint: "h" }] }],
            },
          ],
        },
      },
    };
    const eff = mergeCatalogue(withArgs, null);
    const item = eff.harnesses.claude.groups[0].items[0];
    expect(item.args).toEqual([{ name: "p", hint: "h" }]);
    expect(item.args).not.toBe(withArgs.harnesses.claude.groups[0].items[0].args);
  });

  it("merged_effectives_validate_clean", () => {
    for (const c of CASES) {
      if (c.validates === false) continue;
      const stripped: Record<string, unknown> = {};
      for (const [h, v] of Object.entries(c.effective.harnesses)) {
        stripped[h] = {
          groups: v.groups.map((g) => ({
            id: g.id,
            label: g.label,
            items: g.items.map((i) => {
              const { origin: _origin, ...rest } = i;
              void _origin;
              return rest;
            }),
          })),
        };
      }
      expect(validateCatalogue({ schema: CMD_SCHEMA, harnesses: stripped }), c.name).toEqual([]);
    }
  });

  it("empty_overlay_equals_defaults_all_origin_default", () => {
    for (const overlay of [null, { harnesses: {}, hidden: {} }]) {
      const eff = effectiveCatalogue(overlay);
      expect(Object.keys(eff.harnesses).sort()).toEqual(Object.keys(DEFAULT_CATALOGUE.harnesses).sort());
      for (const [h, v] of Object.entries(eff.harnesses)) {
        const want = DEFAULT_CATALOGUE.harnesses[h].groups;
        expect(v.groups.length).toBe(want.length);
        v.groups.forEach((g, gi) => {
          expect(g.origin).toBe("default");
          expect(g.id).toBe(want[gi].id);
          expect(g.label).toBe(want[gi].label);
          const { items: wantItems } = want[gi];
          expect(g.items.map((i) => i.id)).toEqual(wantItems.map((i) => i.id));
          g.items.forEach((i, ii) => {
            const { origin, ...rest } = i;
            expect(origin).toBe("default");
            expect(rest).toEqual(wantItems[ii]);
          });
        });
      }
    }
  });
});

describe("filter and group", () => {
  const eff = effectiveCatalogue(null);

  it("for_harness_claude_vs_omp", () => {
    const claude = forHarness(eff, "claude");
    const omp = forHarness(eff, "omp");
    expect(claude.length).toBeGreaterThan(0);
    expect(omp.length).toBeGreaterThan(0);
    expect(claude.every((e) => e.harness === "claude")).toBe(true);
    expect(omp.every((e) => e.harness === "omp")).toBe(true);
    expect(claude.map((e) => e.item.id)).not.toEqual(omp.map((e) => e.item.id));
    expect(claude[0].groupOrigin).toBe("default");
  });

  it("for_harness_unknown_is_empty_and_never_throws", () => {
    expect(forHarness(eff, "codex")).toEqual([]);
    expect(forHarness(eff, "__proto__")).toEqual([]);
    expect(forHarness({ harnesses: {} }, "claude")).toEqual([]);
  });

  it("group_entries_first_seen_order", () => {
    const entries = forHarness(eff, "claude");
    const groups = groupEntries(entries);
    expect(groups.map((g) => g.id)).toEqual([...new Set(entries.map((e) => e.groupId))]);
    expect(groups.flatMap((g) => g.entries)).toHaveLength(entries.length);
    expect(groups.every((g) => g.entries.length > 0)).toBe(true);
    expect(groupEntries([])).toEqual([]);
    const mk = (g: string, id: string): Entry => ({
      harness: "claude",
      groupId: g,
      groupLabel: g.toUpperCase(),
      groupOrigin: "default",
      item: { id, kind: "reply", label: id, text: id, origin: "default" },
    });
    const g2 = groupEntries([mk("b", "1"), mk("a", "2"), mk("b", "3")]);
    expect(g2.map((g) => [g.id, g.entries.map((e) => e.item.id)])).toEqual([
      ["b", ["1", "3"]],
      ["a", ["2"]],
    ]);
  });
});

function entry(id: string, label: string, description?: string): Entry {
  const item: EffItem = { id, kind: "reply", label, text: id, origin: "default" };
  if (description !== undefined) item.description = description;
  return { harness: "claude", groupId: "g", groupLabel: "G", groupOrigin: "default", item };
}

describe("search", () => {
  it("search_tiers_and_ties", () => {
    const es = [
      entry("d", "Other", "runs a comp thing"), // description only
      entry("sub", "xcompx"), // label substring
      entry("ws", "run comp"), // word start
      entry("pre", "/compact"), // prefix
      entry("ex", "/comp"), // exact
      entry("idm", "Zed", "nothing"), // id match only for "idm"
      entry("sq", "c-o-m-p"), // subsequence only
    ];
    const ids = (q: string): string[] => searchEntries(es, q).map((e) => e.item.id);
    expect(ids("comp")).toEqual(["ex", "pre", "ws", "sub", "d", "sq"]);
    expect(ids("/comp")).toEqual(ids("comp"));
    expect(ids("  COMP ")).toEqual(ids("comp"));
    expect(ids("idm")).toEqual(["idm"]);
    expect(ids("qqq")).toEqual([]);
    expect(ids("")).toEqual(es.map((e) => e.item.id));
    expect(searchEntries(es, "")).not.toBe(es);
  });

  it("search_compact_ranks_before_a_description_only_hit", () => {
    const eff = effectiveCatalogue(null);
    const claude = forHarness(eff, "claude");
    const hits = searchEntries(claude, "comp").map((e) => e.item.label);
    const first = hits.findIndex((l) => l.toLowerCase().includes("compact"));
    expect(first).toBe(0);
  });

  it("equal_tier_and_length_keeps_catalogue_order_and_a_shuffle_resorts_identically", () => {
    const es = [entry("a", "aaa1"), entry("b", "aaa2"), entry("c", "aaa3"), entry("d", "aaa")];
    const out = searchEntries(es, "aa").map((e) => e.item.id);
    expect(out).toEqual(["d", "a", "b", "c"]);
    const shuffled = [es[2], es[0], es[3], es[1]];
    const once = searchEntries(shuffled, "aa");
    expect(searchEntries(once, "aa").map((e) => e.item.id)).toEqual(once.map((e) => e.item.id));
    expect(searchEntries([...es], "aa").map((e) => e.item.id)).toEqual(out);
  });

  it("search_normalises_nfkc_and_counts_code_points", () => {
    const es = [entry("a", "\u{1D518}bc"), entry("b", "ＡBC")];
    // NFKC folds the full-width A (and the fraktur U), so only the full-width label matches "ab".
    expect(searchEntries(es, "ab").map((e) => e.item.id)).toEqual(["b"]);
    expect(searchEntries(es, "\u{1D518}b").map((e) => e.item.id)).toContain("a");
  });
});

function item(over: Partial<EffItem>): EffItem {
  return { id: "x", kind: "command", label: "X", text: "/x", origin: "default", ...over };
}

describe("toKeystrokes", () => {
  it("to_keystrokes_paste_default_has_enter_false_and_no_trailing_newline", () => {
    const k = toKeystrokes(item({ text: "/compact" }));
    expect(k).toEqual({ kind: "text", text: "/compact", enter: false, unverified: false });
    expect(toKeystrokes(item({ text: "/compact", mode: "paste" }))).toEqual(k);
    if (k.kind === "text") expect(/[\r\n]$/.test(k.text)).toBe(false);
  });

  it("to_keystrokes_run_sets_enter_without_appending_a_newline", () => {
    const k = toKeystrokes(item({ text: "/clear", mode: "run" }));
    expect(k).toEqual({ kind: "text", text: "/clear", enter: true, unverified: false });
  });

  it("to_keystrokes_accepts_an_entry_as_well_as_an_item", () => {
    const e = entry("a", "A");
    expect(toKeystrokes(e)).toEqual(toKeystrokes(e.item));
  });

  it("to_keystrokes_substitutes_args_default_and_escapes", () => {
    const it1 = item({
      text: "/model {name} {{raw}} {n}",
      args: [
        { name: "name", hint: "model" },
        { name: "n", hint: "count", default: "3" },
      ],
    });
    expect(toKeystrokes(it1, { name: "opus" })).toEqual({
      kind: "text",
      text: "/model opus {raw} 3",
      enter: false,
      unverified: false,
    });
    expect(toKeystrokes(it1, { name: "o", n: "9" })).toMatchObject({ text: "/model o {raw} 9" });
    // A value is substituted once and never re-expanded.
    expect(toKeystrokes(it1, { name: "{n}" })).toMatchObject({ text: "/model {n} {raw} 3" });
  });

  it("to_keystrokes_incomplete_lists_missing_in_declared_order", () => {
    const it1 = item({
      text: "/x {b} {a}",
      args: [
        { name: "b", hint: "b" },
        { name: "a", hint: "a" },
        { name: "c", hint: "c", default: "" },
      ],
    });
    expect(toKeystrokes(it1)).toEqual({ kind: "incomplete", missing: ["b", "a"] });
    expect(toKeystrokes(it1, { a: "1" })).toEqual({ kind: "incomplete", missing: ["b"] });
  });

  it("to_keystrokes_refuses_control_chars_in_args", () => {
    const run = item({ text: "/x {a}", mode: "run", args: [{ name: "a", hint: "a" }] });
    const paste = item({ text: "/x {a}", args: [{ name: "a", hint: "a" }] });
    const bad = { kind: "invalid", reason: "arg-control-char" };
    expect(toKeystrokes(run, { a: "1\n2" })).toEqual(bad);
    expect(toKeystrokes(run, { a: "1\r" })).toEqual(bad);
    expect(toKeystrokes(run, { a: "1\t" })).toEqual(bad);
    expect(toKeystrokes(paste, { a: "1\r" })).toEqual(bad);
    expect(toKeystrokes(paste, { a: "\u001b[A" })).toEqual(bad);
    expect(toKeystrokes(paste, { a: "1\n2\t3" })).toMatchObject({ kind: "text", text: "/x 1\n2\t3", enter: false });
  });

  it("to_keystrokes_chord_passes_key_names_through", () => {
    expect(toKeystrokes(item({ kind: "chord", text: undefined, keys: ["Esc", "Esc"] }))).toEqual({
      kind: "keys",
      keys: ["Esc", "Esc"],
      unverified: false,
    });
  });

  it("to_keystrokes_bad_key_or_shape_is_invalid", () => {
    expect(toKeystrokes(item({ kind: "chord", text: undefined, keys: ["Esc", "F13"] }))).toEqual({
      kind: "invalid",
      reason: "bad-key",
    });
    expect(toKeystrokes(item({ kind: "chord", text: undefined, keys: [] }))).toEqual({
      kind: "invalid",
      reason: "bad-key",
    });
    expect(toKeystrokes(item({ kind: "chord", text: undefined }))).toEqual({ kind: "invalid", reason: "bad-key" });
    expect(toKeystrokes(item({ text: undefined }))).toEqual({ kind: "invalid", reason: "bad-item" });
    expect(toKeystrokes(item({ kind: "weird" }))).toEqual({ kind: "invalid", reason: "bad-item" });
  });

  it("every_default_chord_key_is_a_key_name_and_converts", () => {
    let chords = 0;
    for (const cat of Object.values(DEFAULT_CATALOGUE.harnesses)) {
      for (const g of cat.groups) {
        for (const i of g.items) {
          if (i.kind !== "chord") continue;
          chords++;
          for (const k of i.keys ?? []) expect((KEY_NAMES as readonly string[]).includes(k), k).toBe(true);
          expect(toKeystrokes({ ...i, origin: "default" } as EffItem).kind).toBe("keys");
        }
      }
    }
    expect(chords).toBeGreaterThan(0);
  });

  it("every_default_command_converts_when_its_args_are_defaulted", () => {
    for (const e of [...forHarness(effectiveCatalogue(null), "claude"), ...forHarness(effectiveCatalogue(null), "omp")]) {
      if (e.item.kind === "chord") continue;
      const k = toKeystrokes(e);
      expect(["text", "incomplete"], `${e.harness}/${e.item.id}`).toContain(k.kind);
    }
  });

  it("unverified_surfaces", () => {
    const eff = effectiveCatalogue(null);
    const omp = forHarness(eff, "omp");
    const compact = omp.find((e) => e.item.id === "compact");
    const slate = omp.find((e) => e.item.id === "kb-slate");
    expect(compact, "omp compact").toBeDefined();
    expect(slate, "omp kb-slate").toBeDefined();
    expect(toKeystrokes(compact as Entry)).toMatchObject({ unverified: true });
    expect(toKeystrokes(slate as Entry)).toMatchObject({ unverified: false });
    const hit = searchEntries(omp, "compact").find((e) => e.item.id === "compact");
    expect(hit?.item.unverified).toBe(true);
  });
});

describe("purity", () => {
  it("no_dom_imports", () => {
    const src = nodeFs.readFileSync(here("./catalog.ts"), "utf8");
    const code = src
      .split("\n")
      .filter((l) => !l.trimStart().startsWith("//"))
      .join("\n");
    const specs = [...code.matchAll(/from\s+"([^"]+)"/g)].map((m) => m[1]);
    for (const s of specs) {
      expect(["./defaults", "../phone/keys"].includes(s) || s.startsWith("../../api/generated/"), s).toBe(true);
    }
    for (const bad of [
      /\breact\b/,
      /@tanstack/,
      /\bfetch\s*\(/,
      /\bwindow\b/,
      /\bdocument\b/,
      /\blocalStorage\b/,
      /\bsessionStorage\b/,
      /\bnavigator\b/,
    ]) {
      expect(bad.test(code), String(bad)).toBe(false);
    }
  });
});
