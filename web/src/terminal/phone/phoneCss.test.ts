// BPHONE - the phone chrome's stylesheet contract, pinned as source (jsdom has no layout; the
// real boxes are measured by the phone e2e specs, the look by the BSNAP baselines).

import { describe, expect, it } from "vitest";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
  readdirSync(path: string, o: { withFileTypes: true }): Array<{ name: string; isDirectory(): boolean }>;
};
const abs = (rel: string): string => decodeURIComponent(new URL(rel, import.meta.url).pathname);
const read = (rel: string): string => fs.readFileSync(abs(rel), "utf8");

/** Every stylesheet under a directory (the same search set as `lint:css-vars`). */
function allCss(dir: string): string {
  let out = "";
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (e.name === "node_modules") continue;
    const p = `${dir}/${e.name}`;
    if (e.isDirectory()) out += allCss(p);
    else if (e.name.endsWith(".css")) out += `${fs.readFileSync(p, "utf8")}\n`;
  }
  return out;
}

/** Names set from TypeScript at runtime (a per-element custom property), not declared in a sheet. */
const SET_FROM_TS = new Set(["--kb-term-vvh"]);

const FILES = {
  drive: read("./drive.css"),
  phone: read("./phone.css"),
  palette: read("../commands/palette/palette.css"),
};
const SESSIONS = read("../../styles/sessions.css");
const DEFINING = `${allCss(abs("../../"))}\n${allCss(abs("../../../../ui/src"))}`;

/** The body of the first rule whose selector list is exactly `sel` (comments stripped). */
function rule(css: string, sel: string): string {
  const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const i = stripped.indexOf(`${sel} {`);
  if (i < 0) throw new Error(`no rule for ${sel}`);
  return stripped.slice(i, stripped.indexOf("}", i));
}

describe("BPHONE stylesheets: no literals, every var() resolves", () => {
  for (const [name, css] of Object.entries(FILES)) {
    it(`${name}: no hex colour and no px/rem font-size literal`, () => {
      const plain = css.replace(/\/\*[\s\S]*?\*\//g, "");
      expect(plain.match(/#[0-9a-fA-F]{3,8}\b/g) ?? []).toEqual([]);
      expect(plain.match(/font-size:\s*[\d.]+(px|rem)\b/g) ?? []).toEqual([]);
    });

    it(`${name}: every custom-property reference names a declared one`, () => {
      const used = new Set([...css.matchAll(/var\((--[a-z0-9-]+)/gi)].map((m) => m[1]));
      expect(used.size).toBeGreaterThan(0);
      for (const v of used) if (!SET_FROM_TS.has(v)) expect(DEFINING, v).toMatch(new RegExp(`${v}\\s*:`));
    });
  }
});

describe("BPHONE drive.css", () => {
  it("keys and Send are at least the touch minimum", () => {
    const keys = rule(FILES.drive, ".kb-drive__key,\n.kb-drive__send");
    expect(keys).toContain("min-height: var(--touch-min)");
    expect(keys).toContain("min-width: var(--touch-min)");
    expect(rule(FILES.phone, ".kb-term-phone__btn")).toContain("min-height: var(--touch-min)");
  });

  it("keys have a pressed state and a focus-visible ring; the Ctrl states still win over :active", () => {
    expect(FILES.drive).toMatch(/\.kb-drive__key:active:not\(:disabled\)/);
    expect(FILES.drive).toMatch(/\.kb-drive__key:focus-visible/);
    expect(FILES.phone).toMatch(/\.kb-term-phone__btn:focus-visible/);
    expect(FILES.drive.indexOf(".kb-drive__key:active")).toBeLessThan(FILES.drive.indexOf('.kb-drive__key--mod[data-mod="armed"]'));
    // Armed and locked are accent states (PHIN2): still declared.
    expect(rule(FILES.drive, '.kb-drive__key--mod[data-mod="armed"]')).toContain("var(--accent-bg)");
    expect(rule(FILES.drive, '.kb-drive__key--mod[data-mod="locked"]')).toContain("var(--accent)");
  });

  it("one radius, gap, border and type-step token set serves keys, chips, the composer and the title bar", () => {
    const tokens = rule(FILES.drive, ".kb-drive,\n.kb-term-phone");
    for (const t of ["--drv-radius", "--drv-gap", "--drv-bw", "--drv-fs", "--drv-fs-sub"]) expect(tokens).toContain(`${t}:`);
    for (const sel of [".kb-drive__key,\n.kb-drive__send", ".kb-drive__input", ".kb-drive__chip"]) {
      expect(rule(FILES.drive, sel), sel).toContain("border-radius: var(--drv-radius)");
    }
    expect(rule(FILES.drive, ".kb-drive__chip")).toContain("font-size: var(--drv-fs-sub)");
    expect(rule(FILES.phone, ".kb-term-phone__btn")).toContain("border-radius: var(--drv-radius)");
    expect(rule(FILES.phone, ".kb-term-phone__sub")).toContain("font-size: var(--drv-fs-sub)");
    expect(rule(FILES.phone, ".kb-term-phone__bar,\n.kb-term-read__bar")).toContain("font-size: var(--drv-fs)");
  });

  it("the dock's bottom padding clears the home indicator, and the sheet releases its own inset while the dock shows", () => {
    expect(rule(FILES.drive, ".kb-drive")).toMatch(/padding:[^;]*max\(var\(--pad-2\), env\(safe-area-inset-bottom, 0px\)\)/);
    expect(FILES.phone).toMatch(/\.kb-sheet:has\(\[data-kb-term-root\] > div:not\(\[hidden\]\) > \.kb-drive\) \{\s*padding-bottom: 0;/);
  });

  it("the key bar wrap is the positioning box of the floating sheet", () => {
    expect(rule(FILES.drive, ".kb-drive__keybar-wrap")).toContain("position: relative");
    expect(rule(FILES.drive, ".kb-drive__cmdsheet")).toContain("position: absolute");
  });
});

describe("BPHONE landscape command sheet", () => {
  const LAND = "@media (pointer: coarse) and (orientation: landscape) and (max-height: 599px) {";
  const block = (css: string): string => css.slice(css.lastIndexOf(LAND));

  it("palette.css folds the sheet variant: one-line rows, no group heads or descriptions, the toggle rides the head row", () => {
    const b = block(FILES.palette);
    expect(b).toContain(".kb-cmdp--sheet .kb-cmdp__ghead");
    expect(b).toMatch(/\.kb-cmdp--sheet \.kb-cmdp__row \{\s*flex-wrap: nowrap;/);
    expect(b).toMatch(/\.kb-cmdp--sheet \.kb-cmdp__searchtoggle \{\s*position: absolute;/);
    // The row keeps its 44 px touch height (the portrait sheet rule is untouched).
    expect(rule(FILES.palette, ".kb-cmdp--rail .kb-cmdp__row,\n.kb-cmdp--sheet .kb-cmdp__row")).toContain("min-height: var(--touch-min)");
  });

  it("drive.css drops the sheet title in landscape and keeps the head to one row", () => {
    const b = block(FILES.drive);
    expect(b).toMatch(/\.kb-drive__cmdsheet-title \{\s*display: none;/);
    expect(b).toMatch(/\.kb-drive__cmdsheet-head \{\s*padding-block: 0;/);
  });
});

describe("BPHONE terminals list padding (TermList)", () => {
  it("drops the list's own inline gutter on a phone", () => {
    expect(SESSIONS).toMatch(/@media \(max-width: 860px\) \{\s*\.kb-ses__terms \{\s*padding-inline: 0;/);
  });
});
