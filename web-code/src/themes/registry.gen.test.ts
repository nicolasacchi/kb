// V70-A7 — the drift golden for the two CHECKED-IN generated artefacts.
//
// `registry.gen.ts` and `themes.gen.css` are generated from
// `crates/kb-code-server/themes/registry.json`, which lives in the SERVER
// crate (the same ruling as the command registry: the daemon serves it
// verbatim at `GET /api/themes` via `include_str!`, so an agent, the CLI and
// the SPA all read the same bytes). The SPA must never import across the
// crate boundary at BUILD time — the Docker SPA stage copies only
// `web-code/`, so a `../crates` import would work locally and break in the
// image.
//
// That leaves a checked-in copy, and a checked-in copy can go stale. This
// test is why it cannot: it re-runs the exact generator in memory and
// asserts byte equality. Edit the registry without running
// `npm run gen:themes` and this fails, naming the file to regenerate.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { ANCHOR_KEYS, deriveTheme, generateRegistryTs, generateThemesCss } from "./derive";
import { KBC_THEMES, KBC_THEME_FAMILIES, KBC_THEME_SCHEMA } from "./registry.gen";

const REGISTRY_PATH = fileURLToPath(
  new URL("../../../crates/kb-code-server/themes/registry.json", import.meta.url),
);
const TS_PATH = fileURLToPath(new URL("./registry.gen.ts", import.meta.url));
const CSS_PATH = fileURLToPath(new URL("./themes.gen.css", import.meta.url));

const registry = JSON.parse(readFileSync(REGISTRY_PATH, "utf8"));

describe("the checked-in generated theme artefacts", () => {
  it("registry.gen.ts is byte-identical to a fresh generation", () => {
    expect(
      generateRegistryTs(registry),
      "stale — run `npm run gen:themes` in web-code/",
    ).toBe(readFileSync(TS_PATH, "utf8"));
  });

  it("themes.gen.css is byte-identical to a fresh generation", () => {
    expect(
      generateThemesCss(registry),
      "stale — run `npm run gen:themes` in web-code/",
    ).toBe(readFileSync(CSS_PATH, "utf8"));
  });
});

describe("the registry itself", () => {
  it("declares the kbc-theme/1 schema", () => {
    expect(registry.schema).toBe("kbc-theme/1");
    expect(KBC_THEME_SCHEMA).toBe("kbc-theme/1");
  });

  it("carries twelve named families plus the built-in", () => {
    const families = [...new Set(registry.themes.map((t: { family: string }) => t.family))];
    expect(families).toContain("kbc");
    // Twelve catalogue families (D16) + the built-in `kbc` palette.
    expect(families).toHaveLength(13);
    expect(KBC_THEME_FAMILIES).toHaveLength(13);
  });

  it("ships a light AND a dark member for every family", () => {
    for (const fam of KBC_THEME_FAMILIES) {
      const appearances = new Set(fam.members.map((m) => m.appearance));
      expect(appearances, fam.family).toContain("light");
      expect(appearances, fam.family).toContain("dark");
    }
  });

  it("has unique ids and complete, well-formed anchors", () => {
    const ids = registry.themes.map((t: { id: string }) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const t of registry.themes) {
      for (const k of ANCHOR_KEYS) {
        expect(t.anchors[k], `${t.id}.${String(k)}`).toMatch(/^#[0-9a-f]{6}$/);
      }
      expect(Object.keys(t.anchors).sort()).toEqual([...ANCHOR_KEYS].sort());
      // Provenance is part of the contract, not decoration: a bundled theme
      // states its licence and where the palette came from.
      expect(t.license, t.id).toBeTruthy();
      expect(t.source_url, t.id).toMatch(/^https?:\/\//);
    }
  });

  it("derives cleanly for every bundled theme", () => {
    for (const t of registry.themes) expect(() => deriveTheme(t)).not.toThrow();
  });

  it("exposes the same theme list to the SPA as the registry holds", () => {
    expect(KBC_THEMES.map((t) => t.id)).toEqual(registry.themes.map((t: { id: string }) => t.id));
  });

  it("never sets `data-kbc-theme` semantics for the built-in family", () => {
    // The two `kbc-*` entries exist so the catalogue is honest about what the
    // default IS and so the lint measures the shipped palette — but the
    // attribute is never applied for them (see `prefs.resolveThemeId`), which
    // is what keeps a fresh browser byte-identical to pre-V70-A7.
    const builtin = KBC_THEMES.filter((t) => t.family === "kbc");
    expect(builtin.map((t) => t.id)).toEqual(["kbc-dark", "kbc-light"]);
  });
});
