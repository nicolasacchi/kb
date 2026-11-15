// T14 — bundled-version pins vs web/package.json and the Profile golden.

import { describe, expect, it } from "vitest";

// Node's fs without Node typings (same trick as lib/codeDoors.runtime.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string =>
  decodeURIComponent(new URL(rel, import.meta.url).pathname);

import { BUNDLE_PINS, profileMatchesBundle } from "./pins";

function packageDeps(): Record<string, string> {
  const raw = fs.readFileSync(here("../../package.json"), "utf8");
  const parsed = JSON.parse(raw) as { dependencies?: Record<string, string> };
  return parsed.dependencies ?? {};
}

function stripRange(version: string): string {
  return version.replace(/^[\^~]/, "");
}

describe("pins", () => {
  it("bundle pins equal the web/package.json dependency strings", () => {
    const deps = packageDeps();
    expect(stripRange(deps["@xterm/xterm"] ?? "")).toBe(BUNDLE_PINS.xterm);
    expect(stripRange(deps["@xterm/addon-unicode-graphemes"] ?? "")).toBe("0.4.0");
    expect(BUNDLE_PINS.width.endsWith("@0.4.0")).toBe(true);
    expect(stripRange(deps["@xterm/addon-image"] ?? "")).toBe(BUNDLE_PINS.image);
  });

  it("the Profile golden matches the bundle (xterm + width)", () => {
    const raw = fs.readFileSync(
      here("../../../crates/kb-term/tests/golden/sframe_profile.json"),
      "utf8",
    );
    const profile = JSON.parse(raw) as { xterm: string; width: string };
    expect(profile.xterm).toBe(BUNDLE_PINS.xterm);
    expect(profile.width).toBe(BUNDLE_PINS.width);
    expect(profileMatchesBundle(profile)).toBe(true);
  });

  it("a differing Profile does not match (the panel blocks)", () => {
    expect(profileMatchesBundle({ xterm: "9.9.9", width: BUNDLE_PINS.width })).toBe(false);
    expect(profileMatchesBundle({ xterm: BUNDLE_PINS.xterm, width: "other@1.0" })).toBe(false);
  });
});
