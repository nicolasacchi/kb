// Xterm dependency pins: the SPA's terminal panel relies on exact xterm
// versions; this test pins the four spike-validated pins and proves the
// installed tree agrees after `npm ci`.
import { describe, expect, it } from "vitest";

// Node's fs without Node typings (same trick as styles/tokens.palette.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string =>
  decodeURIComponent(new URL(rel, import.meta.url).pathname);

const PINS: Record<string, string> = {
  "@xterm/xterm": "6.0.0",
  "@xterm/addon-image": "0.9.0",
  "@xterm/addon-webgl": "0.19.0",
  "@xterm/addon-unicode-graphemes": "0.4.0",
};

const pkg = JSON.parse(fs.readFileSync(here("../../package.json"), "utf8")) as {
  dependencies?: Record<string, string>;
};

function installedVersion(name: string): string {
  const short = name.replace("@xterm/", "");
  const p = JSON.parse(
    fs.readFileSync(here(`../../node_modules/@xterm/${short}/package.json`), "utf8"),
  ) as { version?: string };
  return String(p.version ?? "");
}

describe("xtermPins", () => {
  it("xterm packages are pinned exactly", () => {
    const deps = pkg.dependencies ?? {};
    for (const [name, pin] of Object.entries(PINS)) {
      expect(deps[name], `${name} pin`).toBe(pin);
      expect(deps[name], `${name} must not be a range`).not.toMatch(/^[\^~>]/);
    }
  });

  it("installed versions equal the pins", () => {
    for (const [name, pin] of Object.entries(PINS)) {
      expect(installedVersion(name), `${name} installed`).toBe(pin);
    }
  });

  it("no beta addon-image", () => {
    expect(installedVersion("@xterm/addon-image")).not.toContain("-");
  });
});
