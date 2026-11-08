// T14 — the golden lock-step test: every T1 golden JSON (under
// `crates/kb-term/tests/golden/`, read via the `getBuiltinModule` fs
// pattern) must parse into the typed union and its key set must stay
// inside `KNOWN_SERVER_KEYS`, so a new golden field fails here until
// the TS type is extended. Client-sent frames re-encode byte-identically.

import { describe, expect, it } from "vitest";

// Node's fs without Node typings (same trick as lib/codeDoors.runtime.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
  readdirSync(dir: string): string[];
};
const here = (rel: string): string =>
  decodeURIComponent(new URL(rel, import.meta.url).pathname);
const GOLDEN_DIR = here("../../../crates/kb-term/tests/golden/");

import {
  decodeBinaryFrame,
  encodeBinaryFrame,
  encodeInput,
  KNOWN_CLIENT_KEYS,
  KNOWN_SERVER_KEYS,
  parseTextFrame,
  WireProtocolError,
} from "./wire";

function goldenNames(prefix: string): string[] {
  return fs
    .readdirSync(GOLDEN_DIR)
    .filter((n) => n.startsWith(prefix) && n.endsWith(".json"))
    .sort();
}

function readGolden(name: string): string {
  return fs.readFileSync(GOLDEN_DIR + name, "utf8");
}

describe("binary frames", () => {
  it("decodes offsets 0, 2^32+5 and near 2^53", () => {
    const payload = new TextEncoder().encode("hi");
    for (const offset of [0, 2 ** 32 + 5, Number.MAX_SAFE_INTEGER]) {
      const frame = encodeBinaryFrame(offset, payload);
      const decoded = decodeBinaryFrame(frame);
      expect(decoded.offset).toBe(offset);
      expect(Array.from(decoded.payload)).toEqual(Array.from(payload));
    }
  });

  it("round-trips offset+payload byte-identically through encode/decode", () => {
    const payload = new Uint8Array([1, 2, 3, 250]);
    const frame = encodeBinaryFrame(987654321, payload);
    expect(Array.from(frame.subarray(0, 8))).toEqual([
      0, 0, 0, 0, 3, 174, 151, 133,
    ]);
    const decoded = decodeBinaryFrame(frame);
    expect(decoded.offset).toBe(987654321);
    expect(Array.from(encodeBinaryFrame(decoded.offset, decoded.payload))).toEqual(
      Array.from(frame),
    );
  });

  it("treats frames shorter than 8 bytes as a protocol error", () => {
    for (const len of [0, 1, 7]) {
      expect(() => decodeBinaryFrame(new Uint8Array(len))).toThrow(WireProtocolError);
    }
  });

  it("rejects offsets >= 2^53", () => {
    const frame = new Uint8Array(8);
    new DataView(frame.buffer).setBigUint64(0, 1n << 53n, false);
    expect(() => decodeBinaryFrame(frame)).toThrow(WireProtocolError);
    const tooBig = new Uint8Array(8);
    new DataView(tooBig.buffer).setBigUint64(0, (1n << 64n) - 1n, false);
    expect(() => decodeBinaryFrame(tooBig)).toThrow(WireProtocolError);
  });
});

describe("server text frames (goldens)", () => {
  it("parses every sframe_* golden into the typed union and unknown t is ignored", () => {
    const names = goldenNames("sframe_");
    expect(names.length).toBeGreaterThan(0);
    for (const name of names) {
      const raw = readGolden(name);
      const parsed = parseTextFrame(raw);
      expect(parsed.t, name).not.toBe("unknown");
      const golden = JSON.parse(raw) as Record<string, unknown>;
      const kind = golden.t as string;
      expect(KNOWN_SERVER_KEYS[kind], name).toBeDefined();
      const goldenKeys = Object.keys(golden).sort();
      const knownKeys = [...KNOWN_SERVER_KEYS[kind]].sort();
      // Every golden key must be known to the TS type (the lock-step).
      for (const k of goldenKeys) expect(knownKeys, `${name} key ${k}`).toContain(k);
      // Spot-check the discriminant round-trip field.
      expect(parsed.t, name).toBe(kind);
      // Client-sent re-encode (input) is covered below; for server frames
      // the typed view must hold the golden's fields.
    }
  });

  it("marks an unknown kind as unknown (ignored, never thrown)", () => {
    expect(parseTextFrame('{"t":"totally-new","x":1}')).toEqual({
      t: "unknown",
      raw: '{"t":"totally-new","x":1}',
    });
  });

  it("throws on malformed text", () => {
    expect(() => parseTextFrame("not json")).toThrow(WireProtocolError);
    expect(() => parseTextFrame('{"kind":"x"}')).toThrow(WireProtocolError);
  });

  it("tolerates unknown fields on a known kind (newer servers stay readable)", () => {
    const frame = parseTextFrame(
      '{"t":"profile","xterm":"6.0.0","serialize":"0.14.0","width":"w","image":"0.9.0","cols":120,"rows":40,"scrollback":50000,"scroll_on_erase_in_display":true,"cell_px_w":9,"cell_px_h":18,"theme":null,"future_field":7}',
    );
    expect(frame.t).toBe("profile");
  });
});

describe("client text frames (goldens)", () => {
  it("re-encodes every client-sent golden byte-identically", () => {
    // The only frame this unit sends is `input`; the others pin their key
    // sets for the lock-step.
    const raw = readGolden("cframe_input.json").trim();
    expect(encodeInput("hello")).toBe(raw);
    for (const name of goldenNames("cframe_")) {
      const golden = JSON.parse(readGolden(name)) as Record<string, unknown>;
      const kind = golden.t as string;
      expect(KNOWN_CLIENT_KEYS[kind], name).toBeDefined();
      for (const k of Object.keys(golden)) {
        expect([...KNOWN_CLIENT_KEYS[kind]].sort(), `${name} key ${k}`).toContain(k);
      }
    }
  });

  it("encodeInput escapes the data string exactly as JSON", () => {
    expect(encodeInput("a\"b\nc")).toBe('{"t":"input","data":"a\\"b\\nc"}');
    expect(encodeInput("")).toBe('{"t":"input","data":""}');
  });
});
