// CMDSCHEMA — the TS validator against the ONE golden the Rust validator
// also reads (crates/kb-server/src/terminal/commands.golden.json).

import { describe, expect, it } from "vitest";
import { KEY_NAMES } from "../phone/keys";
import { CMD_SCHEMA, HARNESS_NAMES, validateCatalogue } from "./schema";

// Node's fs without Node typings (same trick as phone/keys.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const nodeFs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string => decodeURIComponent(new URL(rel, import.meta.url).pathname);
const read = (rel: string): string => nodeFs.readFileSync(here(rel), "utf8");

interface GoldenCase {
  name: string;
  doc: unknown;
  errors: { code: string; path: string }[];
}
const GOLDEN_PATH = "../../../../crates/kb-server/src/terminal/commands.golden.json";
const golden = (JSON.parse(read(GOLDEN_PATH)) as { cases: GoldenCase[] }).cases;

describe("kb-term-commands/1 validator", () => {
  // Twin: `golden_cases_agree` (kb-server `terminal/commands.rs`). If a case's
  // expected code changed, exactly the side whose behaviour differs fails.
  it("agrees with the shared golden on every case, code and path", () => {
    expect(golden.length).toBeGreaterThanOrEqual(40);
    for (const c of golden) {
      expect(validateCatalogue(c.doc), c.name).toEqual(c.errors);
    }
  });

  it("covers every error code the golden names with at least one accept case per kind", () => {
    const names = new Set(golden.map((c) => c.name));
    for (const n of ["accept-command", "accept-chord", "accept-reply"]) expect(names.has(n), n).toBe(true);
    const codes = new Set(golden.flatMap((c) => c.errors.map((e) => e.code)));
    for (const code of [
      "bad-type",
      "missing-field",
      "bad-schema",
      "unknown-harness",
      "unknown-field",
      "bad-id",
      "duplicate-id",
      "too-long",
      "empty",
      "too-many",
      "kind-shape",
      "bad-mode",
      "run-multiline",
      "control-char",
      "undeclared-arg",
      "unused-arg",
      "bad-arg",
      "bad-placeholder",
      "bad-key",
    ]) {
      expect(codes.has(code), code).toBe(true);
    }
  });

  it("codepoint_counting: a label of 40 astral characters fits, 41 does not", () => {
    const mk = (label: string) => ({
      schema: CMD_SCHEMA,
      harnesses: { claude: { groups: [{ id: "g", label, items: [] }] } },
    });
    expect([..."\u{1F600}".repeat(40)].length).toBe(40);
    expect("\u{1F600}".repeat(40).length).toBe(80); // UTF-16 units: the naive count would reject
    expect(validateCatalogue(mk("\u{1F600}".repeat(40)))).toEqual([]);
    expect(validateCatalogue(mk("\u{1F600}".repeat(41)))).toEqual([
      { code: "too-long", path: "/harnesses/claude/groups/0/label" },
    ]);
  });

  it("never throws on arbitrary JSON", () => {
    let deep: unknown = 1;
    for (let i = 0; i < 100; i++) deep = { harnesses: deep };
    const items = Array.from({ length: 10_000 }, (_, i) => ({ id: `i${i}`, kind: "reply", label: "x", text: "y" }));
    const inputs: unknown[] = [
      null,
      undefined,
      1,
      "s",
      [],
      [1, [2, [3]]],
      {},
      { schema: 1, harnesses: [] },
      { schema: CMD_SCHEMA, harnesses: { claude: null } },
      { schema: CMD_SCHEMA, harnesses: { claude: { groups: [null, 1, "x", []] } } },
      { schema: CMD_SCHEMA, harnesses: { claude: { groups: [{ items: [null, { args: [1, { name: 3 }], keys: [1, null] }] }] } } },
      JSON.parse('{"__proto__":1,"constructor":2}'),
      deep,
      { schema: CMD_SCHEMA, harnesses: { claude: { groups: [{ id: "g", label: "g", items }] } } },
    ];
    for (const v of inputs) expect(() => validateCatalogue(v)).not.toThrow();
    expect(validateCatalogue(inputs[inputs.length - 1])).toContainEqual({
      code: "too-many",
      path: "/harnesses/claude/groups",
    });
  });
});

describe("lock-step with the Rust vocabulary", () => {
  /** The names inside `NAMED_KEYS` of the relay's `keys_allow.rs`. */
  function relayNames(): string[] {
    const src = read("../../../../crates/kb-server/src/terminal/keys_allow.rs");
    const m = /pub\(crate\) const NAMED_KEYS: \[&str; \d+\] = \[([\s\S]*?)\];/.exec(src);
    if (!m) throw new Error("NAMED_KEYS not found in keys_allow.rs");
    return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
  }

  it("the key names the validator accepts are exactly the relay's NAMED_KEYS", () => {
    expect([...KEY_NAMES]).toEqual(relayNames());
    const bad = golden.find((c) => c.name === "bad-key");
    expect(bad?.errors).toEqual([{ code: "bad-key", path: "/harnesses/claude/groups/0/items/0/keys/0" }]);
    expect(KEY_NAMES as readonly string[]).not.toContain("Escape");
  });

  it("every relay key is accepted as a chord key", () => {
    const names = relayNames();
    for (let i = 0; i < names.length; i += 8) {
      const doc = {
        schema: CMD_SCHEMA,
        harnesses: {
          claude: { groups: [{ id: "g", label: "g", items: [{ id: "k", kind: "chord", label: "k", keys: names.slice(i, i + 8) }] }] },
        },
      };
      expect(validateCatalogue(doc)).toEqual([]);
    }
  });

  it("the closed harness set equals kb-core's HARNESSES", () => {
    const src = read("../../../../crates/kb-core/src/sessions/constants.rs");
    const m = /pub const HARNESSES: \[&str; \d+\] = \[([\s\S]*?)\];/.exec(src);
    if (!m) throw new Error("HARNESSES not found in constants.rs");
    expect([...HARNESS_NAMES]).toEqual([...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]));
  });
});
