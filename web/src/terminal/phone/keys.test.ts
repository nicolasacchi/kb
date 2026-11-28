// T18 — the key vocabulary, against the Rust copies it must equal.

import { describe, expect, it } from "vitest";
import { KEYBAR_KEYS, KEY_NAMES, ctrlArrowBytes, isKeyName, keyBytes } from "./keys";

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
});

// PHIN2 - the raw bytes the direct-typing path sends equal the runner's table.
describe("keyBytes", () => {
  /** `name -> bytes` from the arms of `named_key_bytes`, Rust byte-literal escapes decoded. */
  function runnerBytes(): Map<string, string> {
    const src = nodeFs.readFileSync(here("../../../../crates/kb-term/src/runner/keys.rs"), "utf8");
    const body = src.split("pub fn named_key_bytes")[1].split("#[cfg(test)]")[0];
    const out = new Map<string, string>();
    for (const m of body.matchAll(/^\s*"([^"]+)"\s*=>\s*b"([^"]*)"/gm)) {
      out.set(
        m[1],
        m[2].replace(/\\x([0-9a-fA-F]{2})/g, (_s, h: string) => String.fromCharCode(parseInt(h, 16))),
      );
    }
    return out;
  }

  it("equals the runner table for all 24 names (normal cursor-keys mode)", () => {
    const table = runnerBytes();
    expect(table.size).toBe(24);
    for (const name of KEY_NAMES) expect(keyBytes(name, false), name).toBe(table.get(name));
  });

  it("the arrows follow application cursor-keys mode", () => {
    expect(["Up", "Down", "Right", "Left"].map((k) => keyBytes(k as "Up", false))).toEqual(["\x1b[A", "\x1b[B", "\x1b[C", "\x1b[D"]);
    expect(["Up", "Down", "Right", "Left"].map((k) => keyBytes(k as "Up", true))).toEqual(["\x1bOA", "\x1bOB", "\x1bOC", "\x1bOD"]);
    expect(keyBytes("Esc", true)).toBe("\x1b");
    expect(keyBytes("S-Tab", true)).toBe("\x1b[Z");
  });

  it("Ctrl+arrow is the modified form; any other key has none", () => {
    expect(["Up", "Down", "Right", "Left"].map((k) => ctrlArrowBytes(k as "Up"))).toEqual(["\x1b[1;5A", "\x1b[1;5B", "\x1b[1;5C", "\x1b[1;5D"]);
    expect(ctrlArrowBytes("Esc")).toBeNull();
    expect(ctrlArrowBytes("y")).toBeNull();
  });
});
