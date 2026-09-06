import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createCursorUrlSync, createPane2CursorUrlSync } from "./cursorUrlSync";

describe("createCursorUrlSync", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("debounces: only the LAST call within the window reaches replace", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "" });

    sync.onSelection({ start: 1, end: 1 });
    vi.advanceTimersByTime(200);
    sync.onSelection({ start: 5, end: 5 });
    vi.advanceTimersByTime(200);
    sync.onSelection({ start: 10, end: 24 });
    vi.advanceTimersByTime(499);
    expect(replace).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);

    expect(replace).toHaveBeenCalledTimes(1);
    expect(replace).toHaveBeenCalledWith("?line=10-24");
  });

  it("respects a custom debounceMs", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "", debounceMs: 100 });

    sync.onSelection({ start: 3, end: 3 });
    vi.advanceTimersByTime(99);
    expect(replace).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(replace).toHaveBeenCalledWith("?line=3");
  });

  it("is a no-op when the serialized line param is unchanged", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "?line=10-24" });

    sync.onSelection({ start: 10, end: 24 });
    vi.advanceTimersByTime(500);
    expect(replace).not.toHaveBeenCalled();

    // Same range, differently-ordered bounds, still normalizes identically.
    sync.onSelection({ start: 24, end: 10 });
    vi.advanceTimersByTime(500);
    expect(replace).not.toHaveBeenCalled();
  });

  it("preserves all other query params, only touching line", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "?ref=main&other=1" });

    sync.onSelection({ start: 7, end: 7 });
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("?ref=main&other=1&line=7");
  });

  it("updates an existing line param in place when the value changes", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "?ref=main&line=1&other=1" });

    sync.onSelection({ start: 10, end: 24 });
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("?ref=main&line=10-24&other=1");
  });

  it("clears the line param on null and preserves other params", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "?ref=main&line=10-24" });

    sync.onSelection(null);
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("?ref=main");
  });

  it("clears to an empty search string when line was the only param", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "?line=10-24" });

    sync.onSelection(null);
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("");
  });

  it("null is a no-op when there was no line param to begin with", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "?ref=main" });

    sync.onSelection(null);
    vi.advanceTimersByTime(500);

    expect(replace).not.toHaveBeenCalled();
  });

  it("dispose cancels a pending debounced write", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "" });

    sync.onSelection({ start: 1, end: 1 });
    sync.dispose();
    vi.advanceTimersByTime(1000);

    expect(replace).not.toHaveBeenCalled();
  });

  it("dispose is safe to call with no pending write", () => {
    const replace = vi.fn();
    const sync = createCursorUrlSync({ replace, getSearch: () => "" });
    expect(() => sync.dispose()).not.toThrow();
  });

  it("reads getSearch fresh at flush time, not at onSelection time", () => {
    const replace = vi.fn();
    let search = "?ref=main";
    const sync = createCursorUrlSync({ replace, getSearch: () => search });

    sync.onSelection({ start: 5, end: 5 });
    search = "?ref=main&other=2"; // changed by something else before the debounce fires
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("?ref=main&other=2&line=5");
  });
});

describe("createPane2CursorUrlSync", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("writes the line into the pane2= value's trailing :line suffix", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=src%2Fother.rs%40%3A",
      getPaneBase: () => ({ path: "src/other.rs" }),
    });

    sync.onSelection({ start: 10, end: 24 });
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("?pane2=src%2Fother.rs%40%3A10-24");
  });

  it("preserves the pane's own ref in the rewritten value", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=src%2Fother.rs%40abc123%3A",
      getPaneBase: () => ({ path: "src/other.rs", ref: "abc123" }),
    });

    sync.onSelection({ start: 5, end: 5 });
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("?pane2=src%2Fother.rs%40abc123%3A5");
  });

  it("preserves every OTHER query param, only touching pane2", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?ref=main&line=1&pane2=src%2Fother.rs%40%3A",
      getPaneBase: () => ({ path: "src/other.rs" }),
    });

    sync.onSelection({ start: 7, end: 7 });
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("?ref=main&line=1&pane2=src%2Fother.rs%40%3A7");
  });

  it("is a no-op when the serialized line is unchanged", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=src%2Fother.rs%40%3A10-24",
      getPaneBase: () => ({ path: "src/other.rs" }),
    });

    sync.onSelection({ start: 10, end: 24 });
    vi.advanceTimersByTime(500);
    expect(replace).not.toHaveBeenCalled();
  });

  it("clears the line suffix on null while keeping path/ref", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=src%2Fother.rs%40abc123%3A10-24",
      getPaneBase: () => ({ path: "src/other.rs", ref: "abc123" }),
    });

    sync.onSelection(null);
    vi.advanceTimersByTime(500);

    expect(replace).toHaveBeenCalledWith("?pane2=src%2Fother.rs%40abc123%3A");
  });

  it("drops the write when pane2 was closed mid-debounce (getPaneBase returns null)", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=src%2Fother.rs%40%3A",
      getPaneBase: () => null,
    });

    sync.onSelection({ start: 3, end: 3 });
    vi.advanceTimersByTime(500);
    expect(replace).not.toHaveBeenCalled();
  });

  it("stale-write guard: drops the write when the URL's pane2 path no longer matches (raced a navigation)", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=different.rs%40%3A", // a NEWER navigation already changed pane2
      getPaneBase: () => ({ path: "src/other.rs" }),
    });

    sync.onSelection({ start: 3, end: 3 });
    vi.advanceTimersByTime(500);
    expect(replace).not.toHaveBeenCalled();
  });

  it("stale-write guard: drops the write when the URL's pane2 ref no longer matches", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=src%2Fother.rs%40dev%3A",
      getPaneBase: () => ({ path: "src/other.rs", ref: "abc123" }),
    });

    sync.onSelection({ start: 3, end: 3 });
    vi.advanceTimersByTime(500);
    expect(replace).not.toHaveBeenCalled();
  });

  it("drops the write when pane2 has been removed from the URL entirely", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?ref=main",
      getPaneBase: () => ({ path: "src/other.rs" }),
    });

    sync.onSelection({ start: 3, end: 3 });
    vi.advanceTimersByTime(500);
    expect(replace).not.toHaveBeenCalled();
  });

  it("debounces: only the last call within the window reaches replace", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=src%2Fother.rs%40%3A",
      getPaneBase: () => ({ path: "src/other.rs" }),
    });

    sync.onSelection({ start: 1, end: 1 });
    vi.advanceTimersByTime(200);
    sync.onSelection({ start: 10, end: 24 });
    vi.advanceTimersByTime(499);
    expect(replace).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(replace).toHaveBeenCalledTimes(1);
    expect(replace).toHaveBeenCalledWith("?pane2=src%2Fother.rs%40%3A10-24");
  });

  it("dispose cancels a pending debounced write", () => {
    const replace = vi.fn();
    const sync = createPane2CursorUrlSync({
      replace,
      getSearch: () => "?pane2=src%2Fother.rs%40%3A",
      getPaneBase: () => ({ path: "src/other.rs" }),
    });

    sync.onSelection({ start: 1, end: 1 });
    sync.dispose();
    vi.advanceTimersByTime(1000);
    expect(replace).not.toHaveBeenCalled();
  });
});
