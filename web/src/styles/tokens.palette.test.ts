// OL F2 — tokens.css no longer hand-writes a palette. It imports the generated
// kbc-theme/1 CSS, and the roles the generator does not emit are expressed
// over generated roles, never raw hex. These are source-text checks: a stray
// palette block or a hex literal creeping back in fails here.
import { describe, expect, it } from "vitest";

// Node's fs via `process.getBuiltinModule` typed as `any`: web/'s tsconfig pulls
// ui/src in but carries no Node typings (same trick as ui/src/tokens.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(url: URL, enc: "utf8"): string;
};

const css = fs.readFileSync(new URL("./tokens.css", import.meta.url), "utf8");
// Comments may quote old hex values; code may not.
const code = css.replace(/\/\*[\s\S]*?\*\//g, "");

describe("web tokens.css palette", () => {
  it("imports the generated themes", () => {
    expect(css).toContain('@import "@kb/ui/themes/themes.gen.css";');
  });

  it("declares no hand-written palette blocks", () => {
    expect(code).not.toMatch(/:root\[data-theme="dark"\]\s*[,{]/);
    expect(code).not.toMatch(/:root\[data-theme="system"\]\s*\{[^}]*--bg:/);
    for (const role of ["bg", "bg-panel", "bg-card", "ink", "ink-mute", "rule", "accent", "warn", "green", "red", "blue"]) {
      expect(code, `--${role} must come from the generated theme`).not.toMatch(
        new RegExp(`^\\s*--${role}:`, "m"),
      );
    }
  });

  it("re-expresses the kb-only roles without raw hex", () => {
    for (const role of [
      "paper", "paper-ink", "paper-rule", "card-bg", "card-bg-end", "card-ring",
      "collage-bg", "term-bg", "accent-fill", "accent-fill-fg", "ink-strong",
    ]) {
      const m = code.match(new RegExp(`--${role}:([^;]+);`));
      expect(m, `--${role} is declared`).toBeTruthy();
      expect(m?.[1], `--${role} must not be a raw hex colour`).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    }
  });

  it("keeps the semantic aliases over the generated roles", () => {
    expect(code).toMatch(/--err:\s*var\(--red\)/);
    expect(code).toMatch(/--success:\s*var\(--green\)/);
  });
});
