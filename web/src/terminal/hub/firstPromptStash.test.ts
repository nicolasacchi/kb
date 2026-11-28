import { beforeEach, describe, expect, it } from "vitest";
import { resetFirstPromptStash, stashFirstPrompt, takeFirstPrompt } from "./firstPromptStash";

beforeEach(resetFirstPromptStash);

describe("firstPromptStash", () => {
  it("returns the text once, then undefined", () => {
    stashFirstPrompt("t1", "echo hi");
    expect(takeFirstPrompt("t1")).toBe("echo hi");
    expect(takeFirstPrompt("t1")).toBeUndefined();
  });
  it("is per terminal id", () => {
    stashFirstPrompt("t1", "one");
    stashFirstPrompt("t2", "two");
    expect(takeFirstPrompt("t2")).toBe("two");
    expect(takeFirstPrompt("t1")).toBe("one");
    expect(takeFirstPrompt("t3")).toBeUndefined();
  });
});
