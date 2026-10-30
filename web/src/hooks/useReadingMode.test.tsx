// @vitest-environment jsdom
// OL READ2 - the reading-mode hook: intent vs acknowledged state, the per-kb
// default, the ack timeout, and one sentence per refusal reason.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { savePrefs, loadPrefs } from "../api/prefs";
import { toast } from "../lib/toast";
import {
  ACK_TIMEOUT_MS,
  READING_TIMEOUT_COPY,
  parseReadAck,
  readingWhyCopy,
  resolveScheme,
  useReadingMode,
  type ReadMessage,
} from "./useReadingMode";

function stubLocalStorage() {
  const store: Record<string, string> = {};
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => (k in store ? store[k] : null),
    setItem: (k: string, v: string) => void (store[k] = v),
    removeItem: (k: string) => void delete store[k],
  });
}

let sent: ReadMessage[];
const post = (m: ReadMessage) => {
  sent.push(m);
  return true;
};

beforeEach(() => {
  vi.useFakeTimers();
  stubLocalStorage();
  sent = [];
  vi.spyOn(toast, "info").mockReturnValue(0);
  document.documentElement.setAttribute("data-theme", "dark");
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const setup = (kb = "docs", id = "a1", onApplied?: (s: "on" | "off") => void) =>
  renderHook((p: { kb: string; id: string }) =>
    useReadingMode({ kb: p.kb, id: p.id, post, onApplied }),
  { initialProps: { kb, id } });

describe("useReadingMode", () => {
  it("is off and unknown by default, and sync says nothing to a page nobody asked about", () => {
    const { result } = setup();
    expect(result.current.effective).toBe(false);
    expect(result.current.state).toBe("unknown");
    expect(result.current.pressed).toBe(false);
    act(() => result.current.sync());
    expect(sent).toEqual([]);
  });

  it("the per-kb default turns it on for that kb only, and sync sends it with the scheme", () => {
    savePrefs({ ...loadPrefs(), readingDefault: { pages: true } });
    const on = setup("pages");
    expect(on.result.current.effective).toBe(true);
    act(() => on.result.current.sync());
    expect(sent).toEqual([{ kind: "kb:read", on: true, scheme: "dark" }]);
    const off = setup("docs");
    expect(off.result.current.effective).toBe(false);
  });

  it("pressed follows the ack, not the intent", () => {
    const { result } = setup();
    act(() => result.current.toggle());
    expect(sent.at(-1)).toMatchObject({ kind: "kb:read", on: true });
    expect(result.current.effective).toBe(true);
    expect(result.current.pressed).toBe(false); // asked, not yet acknowledged
    act(() => result.current.onAck({ state: "on" }));
    expect(result.current.pressed).toBe(true);
    act(() => result.current.toggle());
    expect(sent.at(-1)).toMatchObject({ on: false });
    expect(result.current.pressed).toBe(true); // still on until the page says off
    act(() => result.current.onAck({ state: "off" }));
    expect(result.current.pressed).toBe(false);
    expect(result.current.state).toBe("off");
  });

  it("an unforced sync does not repeat what toggle just sent; a forced one always does", () => {
    const { result } = setup();
    act(() => result.current.toggle());
    expect(sent).toHaveLength(1);
    act(() => result.current.sync());
    expect(sent).toHaveLength(1);
    act(() => result.current.sync(true));
    expect(sent).toHaveLength(2);
  });

  it("an explicit off survives a reload of the frame (sync re-sends off)", () => {
    savePrefs({ ...loadPrefs(), readingDefault: { docs: true } });
    const { result } = setup("docs");
    act(() => result.current.onAck({ state: "on" }));
    act(() => result.current.toggle());
    act(() => result.current.onAck({ state: "off" }));
    sent.length = 0;
    act(() => result.current.onFrameLoad());
    expect(result.current.state).toBe("unknown");
    act(() => result.current.sync());
    expect(sent).toEqual([{ kind: "kb:read", on: false, scheme: "dark" }]);
  });

  it("resets the override and ack when the artifact changes", () => {
    const { result, rerender } = setup("docs", "a1");
    act(() => result.current.toggle());
    act(() => result.current.onAck({ state: "on" }));
    expect(result.current.effective).toBe(true);
    rerender({ kb: "docs", id: "a2" });
    expect(result.current.effective).toBe(false);
    expect(result.current.state).toBe("unknown");
    expect(result.current.pressed).toBe(false);
  });

  it("raises one toast when no ack arrives within the timeout", () => {
    const { result } = setup();
    act(() => result.current.toggle());
    act(() => void vi.advanceTimersByTime(ACK_TIMEOUT_MS - 1));
    expect(toast.info).not.toHaveBeenCalled();
    act(() => void vi.advanceTimersByTime(2));
    expect(toast.info).toHaveBeenCalledTimes(1);
    expect(toast.info).toHaveBeenCalledWith(READING_TIMEOUT_COPY);
    act(() => void vi.advanceTimersByTime(5000));
    expect(toast.info).toHaveBeenCalledTimes(1);
  });

  it("an ack in time cancels the timeout toast", () => {
    const { result } = setup();
    act(() => result.current.toggle());
    act(() => result.current.onAck({ state: "on" }));
    act(() => void vi.advanceTimersByTime(5000));
    expect(toast.info).not.toHaveBeenCalled();
  });

  it.each([
    ["author", /opts out/],
    ["media", /images or media/],
    ["short", /too little text/],
    ["load", /stylesheet/],
  ])("a refused toggle (%s) toasts its own sentence, disables the row and does not send again", (why, re) => {
    const { result } = setup();
    act(() => result.current.toggle());
    act(() => result.current.onAck({ state: "unsuitable", why }));
    expect(result.current.state).toBe("unsuitable");
    expect(result.current.why).toBe(why);
    expect(result.current.pressed).toBe(false);
    expect(toast.info).toHaveBeenCalledTimes(1);
    expect(String(vi.mocked(toast.info).mock.calls[0][0])).toMatch(re);
    sent.length = 0;
    act(() => result.current.toggle()); // r on a refused page: reason again, no post
    expect(sent).toEqual([]);
    expect(toast.info).toHaveBeenCalledTimes(2);
  });

  it("a refusal of an automatic default stays quiet", () => {
    savePrefs({ ...loadPrefs(), readingDefault: { pages: true } });
    const { result } = setup("pages");
    act(() => result.current.sync());
    act(() => result.current.onAck({ state: "unsuitable", why: "media" }));
    expect(result.current.state).toBe("unsuitable");
    expect(toast.info).not.toHaveBeenCalled();
  });

  it("reports on/off acks (and only those) to onApplied", () => {
    const applied = vi.fn();
    const { result } = setup("docs", "a1", applied);
    act(() => result.current.onAck({ state: "on" }));
    act(() => result.current.onAck({ state: "off" }));
    act(() => result.current.onAck({ state: "unsuitable", why: "media" }));
    expect(applied.mock.calls).toEqual([["on"], ["off"]]);
  });

  it("syncScheme re-sends only while the sheet is applied", () => {
    const { result } = setup();
    act(() => result.current.syncScheme());
    expect(sent).toEqual([]);
    act(() => result.current.toggle());
    act(() => result.current.onAck({ state: "on" }));
    sent.length = 0;
    document.documentElement.setAttribute("data-theme", "light");
    act(() => result.current.syncScheme());
    expect(sent).toEqual([{ kind: "kb:read", on: true, scheme: "light" }]);
  });
});

describe("helpers", () => {
  it("parseReadAck accepts the three states and drops the rest", () => {
    expect(parseReadAck({ kind: "kb:read-state", state: "on" })).toEqual({ state: "on" });
    expect(parseReadAck({ kind: "kb:read-state", state: "unsuitable", why: "short" })).toEqual({
      state: "unsuitable",
      why: "short",
    });
    for (const bad of [
      null,
      "x",
      { kind: "kb:read-state" },
      { kind: "kb:read-state", state: "maybe" },
      { kind: "kb:read", state: "on" },
    ]) {
      expect(parseReadAck(bad)).toBeNull();
    }
  });

  it("resolveScheme reads data-theme and resolves system through the OS", () => {
    const el = (t: string | null) => ({ getAttribute: () => t });
    expect(resolveScheme(el("light"), () => ({ matches: true }))).toBe("light");
    expect(resolveScheme(el("dark"), () => ({ matches: false }))).toBe("dark");
    expect(resolveScheme(el("system"), () => ({ matches: true }))).toBe("dark");
    expect(resolveScheme(el("system"), () => ({ matches: false }))).toBe("light");
    expect(resolveScheme(el(null), undefined)).toBe("light");
  });

  it("an unknown refusal reason still has a sentence", () => {
    expect(readingWhyCopy("nope")).toMatch(/unavailable/);
    expect(readingWhyCopy("house")).toMatch(/already uses/);
  });
});
