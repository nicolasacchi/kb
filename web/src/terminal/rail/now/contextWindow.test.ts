import { describe, expect, it } from "vitest";
import { contextPercent, contextWindow } from "./contextWindow";

describe("contextWindow", () => {
  it("knows the Claude families at 200k", () => {
    expect(contextWindow("claude-opus-4-8")).toBe(200_000);
    expect(contextWindow("claude-sonnet-5-5")).toBe(200_000);
    expect(contextWindow("Opus 4.8")).toBe(200_000);
    expect(contextWindow("haiku")).toBe(200_000);
  });
  it("reads the 1M variant from the model-string suffix", () => {
    expect(contextWindow("claude-opus-4-8[1m]")).toBe(1_000_000);
    expect(contextWindow("claude-opus-4-8-1m")).toBe(1_000_000);
    expect(contextWindow("Opus 4.8 (1M context)")).toBe(1_000_000);
    // A model id that merely contains "1m" inside another token stays 200k.
    expect(contextWindow("claude-haiku-4-1mini")).toBe(200_000);
  });
  it("is null for an unknown, empty or absent model", () => {
    expect(contextWindow("some-other-model")).toBeNull();
    expect(contextWindow("")).toBeNull();
    expect(contextWindow(undefined)).toBeNull();
    expect(contextWindow(null)).toBeNull();
  });
});

describe("contextPercent", () => {
  it("computes a percentage only with a known window", () => {
    expect(contextPercent(100_000, "claude-opus-4-8")).toBe(50);
    expect(contextPercent(100_000, "claude-opus-4-8[1m]")).toBe(10);
    expect(contextPercent(100_000, "unknown")).toBeNull();
    expect(contextPercent(null, "claude-opus-4-8")).toBeNull();
  });
  it("caps at 100", () => {
    expect(contextPercent(900_000, "claude-opus-4-8")).toBe(100);
  });
});
