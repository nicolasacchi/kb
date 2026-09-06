import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createStoryUrlSync } from "./storyUrlSync";

describe("createStoryUrlSync", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("debounces: only the LAST call within the window reaches replace", () => {
    const replace = vi.fn();
    const sync = createStoryUrlSync({ replace, getSearch: () => "" });

    sync.onStep("sha1");
    vi.advanceTimersByTime(100);
    sync.onStep("sha2");
    vi.advanceTimersByTime(100);
    sync.onStep("sha3");
    vi.advanceTimersByTime(249);
    expect(replace).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);

    expect(replace).toHaveBeenCalledTimes(1);
    expect(replace).toHaveBeenCalledWith("?at=sha3");
  });

  it("respects a custom debounceMs", () => {
    const replace = vi.fn();
    const sync = createStoryUrlSync({ replace, getSearch: () => "", debounceMs: 50 });

    sync.onStep("sha1");
    vi.advanceTimersByTime(49);
    expect(replace).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(replace).toHaveBeenCalledWith("?at=sha1");
  });

  it("is a no-op when the sha is already the current at= value", () => {
    const replace = vi.fn();
    const sync = createStoryUrlSync({ replace, getSearch: () => "?at=sha1" });

    sync.onStep("sha1");
    vi.advanceTimersByTime(250);
    expect(replace).not.toHaveBeenCalled();
  });

  it("preserves every other query param, only touching at", () => {
    const replace = vi.fn();
    const sync = createStoryUrlSync({ replace, getSearch: () => "?ref=main&other=1" });

    sync.onStep("sha1");
    vi.advanceTimersByTime(250);

    expect(replace).toHaveBeenCalledWith("?ref=main&other=1&at=sha1");
  });

  it("updates an existing at param in place (URLSearchParams keeps its position)", () => {
    const replace = vi.fn();
    const sync = createStoryUrlSync({ replace, getSearch: () => "?at=sha1&other=1" });

    sync.onStep("sha2");
    vi.advanceTimersByTime(250);

    expect(replace).toHaveBeenCalledWith("?at=sha2&other=1");
  });

  it("reads getSearch fresh at flush time, not at onStep time", () => {
    const replace = vi.fn();
    let search = "?ref=main";
    const sync = createStoryUrlSync({ replace, getSearch: () => search });

    sync.onStep("sha1");
    search = "?ref=main&other=2"; // changed by something else before the debounce fires
    vi.advanceTimersByTime(250);

    expect(replace).toHaveBeenCalledWith("?ref=main&other=2&at=sha1");
  });

  it("dispose cancels a pending debounced write", () => {
    const replace = vi.fn();
    const sync = createStoryUrlSync({ replace, getSearch: () => "" });

    sync.onStep("sha1");
    sync.dispose();
    vi.advanceTimersByTime(1000);

    expect(replace).not.toHaveBeenCalled();
  });

  it("dispose is safe to call with no pending write", () => {
    const replace = vi.fn();
    const sync = createStoryUrlSync({ replace, getSearch: () => "" });
    expect(() => sync.dispose()).not.toThrow();
  });
});
