import { describe, expect, it } from "vitest";
import type { PromptList } from "./promptsApi";
import { byteLength, MAX_PROMPT_BYTES, moveTarget, splitItems, textOk, validProject } from "./promptsModel";

const item = (id: string, done: number | null = null, order = 0) => ({ id, text: id, created_at: 1, done_at: done, order });
const list = (items: ReturnType<typeof item>[]): PromptList => ({ project: "demo", rev: 1, items });

describe("validProject", () => {
  it("accepts the server's slug shape and nothing else", () => {
    expect(validProject("demo-1_x")).toBe("demo-1_x");
    for (const bad of ["", "A", "a/b", "a b", "../x", "a".repeat(65), null, undefined]) expect(validProject(bad)).toBeNull();
  });
});

describe("textOk", () => {
  it("bounds by trimmed UTF-8 bytes", () => {
    expect(textOk("  ")).toBe(false);
    expect(textOk("a".repeat(MAX_PROMPT_BYTES))).toBe(true);
    expect(textOk("a".repeat(MAX_PROMPT_BYTES + 1))).toBe(false);
    // 4096 two-byte characters = 8192 bytes: the cap is bytes, not characters.
    expect(byteLength("é".repeat(4096))).toBe(8192);
    expect(textOk("é".repeat(4097))).toBe(false);
  });
});

describe("splitItems / moveTarget", () => {
  const l = list([item("q_a"), item("q_d1", 5), item("q_b"), item("q_c")]);
  it("keeps open items first in stored order and done items apart", () => {
    const s = splitItems(l);
    expect(s.open.map((i) => i.id)).toEqual(["q_a", "q_b", "q_c"]);
    expect(s.done.map((i) => i.id)).toEqual(["q_d1"]);
    expect(splitItems(undefined)).toEqual({ open: [], done: [] });
  });
  it("moves to the open neighbour's index in the FULL list", () => {
    expect(moveTarget(l, "q_b", -1)).toBe(0);
    expect(moveTarget(l, "q_a", 1)).toBe(2);
    expect(moveTarget(l, "q_b", 1)).toBe(3);
  });
  it("is null at the ends and for a done or unknown item", () => {
    expect(moveTarget(l, "q_a", -1)).toBeNull();
    expect(moveTarget(l, "q_c", 1)).toBeNull();
    expect(moveTarget(l, "q_d1", 1)).toBeNull();
    expect(moveTarget(l, "q_zz", 1)).toBeNull();
  });
});
