// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { vi } from "vitest";
import {
  dispatchForwardedKey,
  forwardedKey,
  FORWARDED_KEYS,
  routeForwardedKey,
} from "./artifactKeys";

describe("forwardedKey", () => {
  it("accepts exactly the closed list", () => {
    expect([...FORWARDED_KEYS]).toEqual(["r", "o", "b", "[", "]", "n", "p", "u"]);
    for (const key of FORWARDED_KEYS) {
      expect(forwardedKey({ kind: "kb:key", key })).toBe(key);
    }
  });

  it("rejects everything else", () => {
    for (const bad of [
      null,
      undefined,
      "r",
      42,
      {},
      { kind: "kb:key" },
      { kind: "kb:key", key: 114 },
      { kind: "kb:key", key: "R" },
      { kind: "kb:key", key: "Escape" },
      { kind: "kb:key", key: "g" },
      { kind: "kb:key", key: "/" },
      { kind: "kb:read-state", key: "r" },
    ]) {
      expect(forwardedKey(bad)).toBeNull();
    }
  });
});

describe("dispatchForwardedKey", () => {
  it("reaches a window keydown handler as a plain bare key", () => {
    const seen: KeyboardEvent[] = [];
    const onKey = (e: KeyboardEvent) => seen.push(e);
    window.addEventListener("keydown", onKey);
    try {
      dispatchForwardedKey("u");
    } finally {
      window.removeEventListener("keydown", onKey);
    }
    expect(seen).toHaveLength(1);
    expect(seen[0].key).toBe("u");
    expect(seen[0].ctrlKey || seen[0].metaKey || seen[0].altKey || seen[0].shiftKey).toBe(false);
    expect((seen[0].target as HTMLElement).tagName).toBe("BODY");
  });
});

describe("routeForwardedKey", () => {
  it("r toggles the sending pane only and is never replayed into window handlers", () => {
    const toggleA = vi.fn();
    const toggleB = vi.fn();
    const replay = vi.fn();
    expect(routeForwardedKey({ kind: "kb:key", key: "r" }, toggleA, replay)).toBe("r");
    expect(toggleA).toHaveBeenCalledTimes(1);
    expect(toggleB).not.toHaveBeenCalled();
    expect(replay).not.toHaveBeenCalled();
  });

  it("other whitelisted keys are replayed; anything else is dropped", () => {
    const toggle = vi.fn();
    const replay = vi.fn();
    expect(routeForwardedKey({ kind: "kb:key", key: "o" }, toggle, replay)).toBe("o");
    expect(replay).toHaveBeenCalledWith("o");
    expect(routeForwardedKey({ kind: "kb:key", key: "g" }, toggle, replay)).toBeNull();
    expect(routeForwardedKey({ kind: "other", key: "r" }, toggle, replay)).toBeNull();
    expect(toggle).not.toHaveBeenCalled();
    expect(replay).toHaveBeenCalledTimes(1);
  });
});
