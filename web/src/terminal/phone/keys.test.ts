// T18 — the key vocabulary, against the Rust copies it must equal.

import { describe, expect, it } from "vitest";
import { KEYBAR_KEYS, KEY_NAMES, SLASH_MENU, isKeyName, slashMenuFor } from "./keys";

// Node's fs without Node typings (same trick as terminal/wire.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const nodeFs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string => decodeURIComponent(new URL(rel, import.meta.url).pathname);

/** The names inside `NAMED_KEYS` of the relay's `keys_allow.rs`. */
function relayNames(): string[] {
  const src = nodeFs.readFileSync(here("../../../../crates/kb-server/src/terminal/keys_allow.rs"), "utf8");
  const m = /pub\(crate\) const NAMED_KEYS: \[&str; \d+\] = \[([\s\S]*?)\];/.exec(src);
  if (!m) throw new Error("NAMED_KEYS not found in keys_allow.rs");
  return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
}

/** The names the runner maps, from the arms of `named_key_bytes` in `keys.rs`. */
function runnerNames(): string[] {
  const src = nodeFs.readFileSync(here("../../../../crates/kb-term/src/runner/keys.rs"), "utf8");
  const body = src.split("pub fn named_key_bytes")[1].split("#[cfg(test)]")[0];
  return [...body.matchAll(/^\s*"([^"]+)"\s*=>\s*b"/gm)].map((x) => x[1]);
}

describe("key vocabulary", () => {
  // Twins: `relay_key_names_equal_the_runner_table` (kb-server `keys_allow.rs`) and
  // `named_key_table_is_pinned` (kb-term `runner/keys.rs`).
  it("equals the relay's list and the runner's table, in order", () => {
    expect([...KEY_NAMES]).toEqual(relayNames());
    expect([...KEY_NAMES]).toEqual(runnerNames());
  });

  it("every key-bar and choice key is a key the relay forwards", () => {
    for (const k of KEYBAR_KEYS) expect(isKeyName(k.name), k.name).toBe(true);
    expect(isKeyName("Escape")).toBe(false);
    expect(isKeyName("BTab")).toBe(false);
    expect(isKeyName("/")).toBe(false);
  });

  it("FUKEYBAR: the bar carries Backspace and Ctrl-D, both names the relay already forwards", () => {
    const names = KEYBAR_KEYS.map((k) => k.name);
    expect(names).toContain("BSpace");
    expect(names).toContain("C-d");
    expect(relayNames()).toContain("BSpace");
    expect(relayNames()).toContain("C-d");
    expect(KEYBAR_KEYS.find((k) => k.name === "C-d")?.aria).toMatch(/hold/i);
  });

  it("the slash menu is per harness, pasted entries only, empty for the rest", () => {
    expect(slashMenuFor("claude")).toEqual(["/help", "/compact", "/clear", "/model", "/resume"]);
    expect(slashMenuFor("omp")).toEqual(["/compact", "/shake", "/tree"]);
    expect(slashMenuFor("codex")).toEqual([]);
    for (const entries of Object.values(SLASH_MENU)) {
      for (const e of entries) expect(e.startsWith("/")).toBe(true);
    }
  });
});
