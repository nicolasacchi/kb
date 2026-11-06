// Placeholder tests for the kb-term tap (T2). Later units (T3-T7) add the
// real pipeline tests and raise the term-tap floor; these three pin the
// lane's shape: the bun pin, generator determinism, and the stream shapes
// the tap must handle.
import { test, expect } from "bun:test";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const TAP_DIR = join(import.meta.dir, ".");
const HARNESS = join(import.meta.dir, "../fixtures/fake_harness.py");

// sha256 of `python3 -I crates/kb-term/fixtures/fake_harness.py --seed 3
// --length 60` — the spike's recorded stream for exactly these args.
const HARNESS_SHA256 =
  "f1b05d401f42c65e04932dc324df45e13c8a14356dc05f83d0b39084dbe51f4a";

function harnessBytes(...args: string[]): Buffer {
  const res = spawnSync("python3", ["-I", HARNESS, ...args]);
  if (res.status !== 0) {
    throw new Error(`fake_harness failed: ${res.stderr.toString()}`);
  }
  return Buffer.from(res.stdout);
}

test("bun version matches the pinned version", () => {
  const pinned = readFileSync(join(TAP_DIR, ".bun-version"), "utf-8").trim();
  if (process.env.KB_REQUIRE_BUN_PIN === "1") {
    expect(Bun.version).toBe(pinned);
  } else {
    console.log(`bun ${Bun.version}, pinned ${pinned} (not enforcing pin)`);
  }
});

test("fake_harness is deterministic", () => {
  const a = harnessBytes("--seed", "3", "--length", "60");
  const b = harnessBytes("--seed", "3", "--length", "60");
  expect(a.equals(b)).toBe(true);
  expect(createHash("sha256").update(a).digest("hex")).toBe(HARNESS_SHA256);
  const other = harnessBytes("--seed", "4", "--length", "60");
  expect(other.equals(a)).toBe(false);
});

test("fake_harness exercises the shapes the tap must handle", () => {
  const bytes = harnessBytes("--seed", "3", "--length", "60");
  const has = (seq: string | Buffer) =>
    bytes.indexOf(Buffer.from(seq)) >= 0;
  expect(has("\x1b[?2026h")).toBe(true);
  expect(has("\x1b[2J")).toBe(true);
  expect(has("\x1b[3J")).toBe(true);
  expect(has("\x1bPtmux;")).toBe(true);
  expect(has("\x1b_G")).toBe(true);
  expect(bytes.indexOf(Buffer.from("\u{10EEEE}")) >= 0).toBe(true);
});
