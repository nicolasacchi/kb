import { describe, expect, it, vi } from "vitest";
import {
  osc52ClipboardText,
  writeClipboardIfRecent,
  writeClipboardText,
} from "./clipboard";

describe("osc52ClipboardText", () => {
  it("recent typing → decodes and returns the text", () => {
    expect(osc52ClipboardText("c;SGVsbG8=", 1000, 5000)).toBe("Hello");
  });

  it("stale typing (>10s) → null, no write", () => {
    expect(osc52ClipboardText("c;SGVsbG8=", 0, 11_000)).toBeNull();
    expect(osc52ClipboardText("c;SGVsbG8=", 1000, 11_001)).toBeNull();
  });

  it("exactly at the window edge still writes; never-typed does not", () => {
    expect(osc52ClipboardText("c;SGVsbG8=", 0, 10_000)).toBe("Hello");
    expect(osc52ClipboardText("c;SGVsbG8=", 0, 10_001)).toBeNull();
  });

  it("read query (?) → null", () => {
    expect(osc52ClipboardText("c;?", 1000, 2000)).toBeNull();
    expect(osc52ClipboardText("?", 1000, 2000)).toBeNull();
  });

  it("malformed base64 / missing field → null, never throws", () => {
    expect(osc52ClipboardText("c;!!!", 1000, 2000)).toBeNull();
    expect(osc52ClipboardText("c", 1000, 2000)).toBeNull();
    expect(osc52ClipboardText("", 1000, 2000)).toBeNull();
  });
});

describe("writeClipboardText", () => {
  it("writes via navigator.clipboard and swallows rejection", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    await writeClipboardText("Hello");
    expect(writeText).toHaveBeenCalledWith("Hello");
    vi.unstubAllGlobals();
  });

  it("a rejected writeText never throws", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    await expect(writeClipboardText("Hello")).resolves.toBeUndefined();
    vi.unstubAllGlobals();
  });

  it("missing clipboard API never throws", async () => {
    vi.stubGlobal("navigator", {});
    await expect(writeClipboardText("Hello")).resolves.toBeUndefined();
    vi.unstubAllGlobals();
  });
});

describe("writeClipboardIfRecent", () => {
  it("stale → clipboard is not touched (mutant always-writes fails)", async () => {
    const writeText = vi.fn();
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    await writeClipboardIfRecent("c;SGVsbG8=", 0, 60_000);
    expect(writeText).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it("recent → clipboard written exactly once", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    await writeClipboardIfRecent("c;SGVsbG8=", 1000, 2000);
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith("Hello");
    vi.unstubAllGlobals();
  });
});
