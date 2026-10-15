import { describe, expect, it } from "vitest";

// ui/src/tokens.css is the ONE home of the type ramp, spacing steps, radii,
// the z-ladder and motion. Each app imports it from its own tokens.css and
// must not redeclare what it provides. The files are read as raw text via
// import.meta.glob (not node:fs) so the test needs no Node typings and runs
// under both apps' vitest (React 18 and 19 alike -- it touches neither).
const raw = import.meta.glob(
  ["./tokens.css", "../../web/src/styles/tokens.css", "../../web-code/src/styles/tokens.css"],
  { query: "?raw", import: "default", eager: true },
) as Record<string, string>;

function text(path: string): string {
  const t = raw[path];
  if (typeof t !== "string") throw new Error(`tokens.test: ${path} was not loaded`);
  return t;
}

/** `selector|--prop` -> normalised value, for every custom property declared
 * at any brace depth (so @media-nested blocks count). Comments stripped. */
function declarations(css: string): Map<string, string> {
  const src = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const out = new Map<string, string>();
  const stack: string[] = [];
  let buf = "";
  for (const ch of src) {
    if (ch === "{") {
      stack.push(buf.replace(/\s+/g, " ").trim());
      buf = "";
    } else if (ch === "}") {
      stack.pop();
      buf = "";
    } else if (ch === ";") {
      const m = buf.match(/^\s*(--[\w-]+)\s*:([\s\S]*)$/);
      if (m && stack.length) out.set(`${stack.join(" > ")}|${m[1]}`, m[2].replace(/\s+/g, " ").trim());
      buf = "";
    } else buf += ch;
  }
  return out;
}

const ui = declarations(text("./tokens.css"));
const props = (m: Map<string, string>) => new Set([...m.keys()].map((k) => k.split("|")[1]));
const web = declarations(text("../../web/src/styles/tokens.css"));
const code = declarations(text("../../web-code/src/styles/tokens.css"));

describe("ui/src/tokens.css", () => {
  it("pins the Margin ramp, the card-scale xl step and the compact floors", () => {
    const v = (sel: string, p: string) => ui.get(`${sel}|${p}`);
    const base = ":root";
    expect(["xs", "sm", "md", "lg", "xl", "title", "hero", "body"].map((s) => v(base, `--fs-${s}`))).toEqual([
      "12px", "13px", "14px", "15px", "17px", "20px", "28px", "15px",
    ]);
    const compact = ':root[data-density="compact"]';
    expect(["sm", "md", "lg", "xl", "title", "hero", "body"].map((s) => v(compact, `--fs-${s}`))).toEqual([
      "12px", "13px", "14px", "16px", "18px", "24px", "14px",
    ]);
    // floors: caption never below 12, body never below 14
    expect(v(compact, "--fs-body")).toBe("14px");
    expect(v(base, "--fs-xs")).toBe("12px");
    expect(v(compact, "--fs-xs")).toBeUndefined();
  });

  it("pins spacing, radius, z-ladder and motion", () => {
    const g = (p: string) => ui.get(`:root|${p}`);
    expect([1, 2, 3, 4].map((n) => g(`--pad-${n}`))).toEqual(["4px", "8px", "16px", "24px"]);
    expect(["sm", "md", "lg", "pill"].map((s) => g(`--radius-${s}`))).toEqual(["4px", "8px", "14px", "999px"]);
    expect(["bar", "popover", "popover-hi", "float", "scrim", "drawer", "hint", "modal", "top"].map((s) => g(`--z-${s}`))).toEqual(
      ["5", "20", "30", "35", "40", "41", "50", "100", "1000"],
    );
    expect([g("--dur-fast"), g("--dur-base")]).toEqual(["100ms", "160ms"]);
  });

  it("is declared nowhere else in web-code", () => {
    const dup = [...props(ui)].filter((p) => props(code).has(p));
    expect(dup).toEqual([]);
  });

  it("is declared nowhere else in web, except the density-scaled --pad-* steps", () => {
    // kb multiplies the spacing steps by --density; that is deliberate and is
    // not unified here. Everything else must come from ui/.
    const allowed = new Set(["--pad-1", "--pad-2", "--pad-3", "--pad-4"]);
    const dup = [...props(ui)].filter((p) => props(web).has(p) && !allowed.has(p));
    expect(dup).toEqual([]);
  });

  it("never gives the same selector+property two different values across files", () => {
    for (const other of [web, code]) {
      for (const [k, val] of ui) {
        const p = k.split("|")[1];
        if (other.has(k) && !["--pad-1", "--pad-2", "--pad-3", "--pad-4"].includes(p)) expect(other.get(k)).toBe(val);
      }
    }
  });
});
