import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createTourUrlSync } from "./tourUrlSync";

describe("createTourUrlSync", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("debounces: only the LAST call within the window reaches replace", () => {
    const replace = vi.fn();
    const sync = createTourUrlSync({ replace, getSearch: () => "" });

    sync.onStep(0);
    vi.advanceTimersByTime(100);
    sync.onStep(1);
    vi.advanceTimersByTime(100);
    sync.onStep(2);
    vi.advanceTimersByTime(249);
    expect(replace).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);

    expect(replace).toHaveBeenCalledTimes(1);
    expect(replace).toHaveBeenCalledWith("?step=3");
  });

  it("serializes the step 1-based", () => {
    const replace = vi.fn();
    const sync = createTourUrlSync({ replace, getSearch: () => "" });

    sync.onStep(0);
    vi.advanceTimersByTime(250);
    expect(replace).toHaveBeenCalledWith("?step=1");
  });

  it("respects a custom debounceMs", () => {
    const replace = vi.fn();
    const sync = createTourUrlSync({ replace, getSearch: () => "", debounceMs: 50 });

    sync.onStep(4);
    vi.advanceTimersByTime(49);
    expect(replace).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(replace).toHaveBeenCalledWith("?step=5");
  });

  it("is a no-op when the step is already the current value", () => {
    const replace = vi.fn();
    const sync = createTourUrlSync({ replace, getSearch: () => "?step=2" });

    sync.onStep(1);
    vi.advanceTimersByTime(250);
    expect(replace).not.toHaveBeenCalled();
  });

  it("preserves every other query param, only touching step", () => {
    const replace = vi.fn();
    const sync = createTourUrlSync({ replace, getSearch: () => "?ref=main&other=1" });

    sync.onStep(0);
    vi.advanceTimersByTime(250);

    expect(replace).toHaveBeenCalledWith("?ref=main&other=1&step=1");
  });

  it("reads getSearch fresh at flush time, not at onStep time", () => {
    const replace = vi.fn();
    let search = "?ref=main";
    const sync = createTourUrlSync({ replace, getSearch: () => search });

    sync.onStep(0);
    search = "?ref=main&other=2";
    vi.advanceTimersByTime(250);

    expect(replace).toHaveBeenCalledWith("?ref=main&other=2&step=1");
  });

  it("dispose cancels a pending debounced write", () => {
    const replace = vi.fn();
    const sync = createTourUrlSync({ replace, getSearch: () => "" });

    sync.onStep(0);
    sync.dispose();
    vi.advanceTimersByTime(1000);

    expect(replace).not.toHaveBeenCalled();
  });

  it("dispose is safe to call with no pending write", () => {
    const replace = vi.fn();
    const sync = createTourUrlSync({ replace, getSearch: () => "" });
    expect(() => sync.dispose()).not.toThrow();
  });
});
