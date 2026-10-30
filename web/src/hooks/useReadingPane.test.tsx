// @vitest-environment jsdom
// OL READ2 - the pane's reading-mode transport: exact-origin posts, acks only
// from the pane's own frame, the annotator repaint, theme re-send, independent
// panes, and the bare `r` key's guards.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { THEME_APPLIED_EVENT } from "@kb/ui/themes/apply";
import { loadPrefs, savePrefs } from "../api/prefs";
import { dispatchForwardedKey } from "../lib/artifactKeys";
import { artifactOrigin } from "../lib/artifactHost";
import { toast } from "../lib/toast";
import {
  REPAINT_SETTLE_MS,
  isReadingKey,
  useReadingKey,
  useReadingPane,
  type UseReadingPaneOptions,
} from "./useReadingPane";

const SUFFIX = ".artifacts.localhost";
const KB = "docs";

function stubLocalStorage() {
  const store: Record<string, string> = {};
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => (k in store ? store[k] : null),
    setItem: (k: string, v: string) => void (store[k] = v),
    removeItem: (k: string) => void delete store[k],
  });
}

interface Frame {
  iframe: HTMLIFrameElement;
  win: Window;
  post: ReturnType<typeof vi.fn>;
}
function makeFrame(): Frame {
  const iframe = document.createElement("iframe");
  document.body.appendChild(iframe);
  const win = iframe.contentWindow as Window;
  const post = vi.fn();
  (win as unknown as { postMessage: unknown }).postMessage = post;
  return { iframe, win, post };
}

function deliver(data: unknown, origin: string, source: Window | null) {
  // `source` is set by hand: jsdom's MessageEvent would re-wrap the window,
  // and the hook's identity check compares against `contentWindow` itself.
  const ev = new MessageEvent("message", { data, origin });
  Object.defineProperty(ev, "source", { value: source });
  act(() => void window.dispatchEvent(ev));
}

const ack = (state: string, why?: string) => ({ kind: "kb:read-state", state, ...(why ? { why } : {}) });

let frames: Frame[];
beforeEach(() => {
  vi.useFakeTimers();
  stubLocalStorage();
  vi.stubGlobal("requestAnimationFrame", (cb: () => void) => setTimeout(cb, 16));
  vi.stubGlobal("cancelAnimationFrame", (n: number) => clearTimeout(n));
  vi.spyOn(toast, "info").mockReturnValue(0);
  document.documentElement.setAttribute("data-theme", "dark");
  frames = [];
});
afterEach(() => {
  cleanup();
  for (const f of frames) f.iframe.remove();
  document.body.innerHTML = "";
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function pane(
  over: Partial<UseReadingPaneOptions> & { frame?: Frame; id?: string } = {},
) {
  const frame = over.frame ?? makeFrame();
  frames.push(frame);
  const id = over.id ?? "a1";
  const refresh = vi.fn();
  const onSelectionChange = vi.fn();
  const opts: UseReadingPaneOptions = {
    kb: KB,
    id,
    hostSuffix: SUFFIX,
    iframeRef: { current: frame.iframe },
    annotateActive: false,
    reviewFile: { comments: [] } as unknown as UseReadingPaneOptions["reviewFile"],
    bridgeRef: { current: { refresh } as unknown as NonNullable<UseReadingPaneOptions["bridgeRef"]["current"]> },
    onSelectionChange,
    ...over,
  };
  const hook = renderHook(() => useReadingPane(opts));
  return { ...hook, frame, refresh, onSelectionChange, origin: artifactOrigin(id, KB, SUFFIX) };
}

describe("useReadingPane", () => {
  it("posts to the iframe at its exact artifact origin, never '*'", () => {
    const p = pane();
    act(() => p.result.current.toggle());
    expect(p.frame.post).toHaveBeenCalledTimes(1);
    const [msg, target] = p.frame.post.mock.calls[0];
    expect(msg).toEqual({ kind: "kb:read", on: true, scheme: "dark" });
    expect(target).toBe(p.origin);
    expect(target).not.toBe("*");
  });

  it("takes acks only from the exact origin AND the pane's own frame", () => {
    const p = pane();
    const other = makeFrame();
    frames.push(other);
    deliver(ack("on"), "https://evil.example", p.frame.win);
    deliver(ack("on"), artifactOrigin("a2", KB, SUFFIX), p.frame.win); // sibling artifact's origin
    deliver(ack("on"), p.origin, other.win); // right origin, someone else's frame
    deliver(ack("on"), p.origin, null); // source-less
    expect(p.result.current.state).toBe("unknown");
    deliver(ack("on"), p.origin, p.frame.win);
    expect(p.result.current.state).toBe("on");
    expect(p.result.current.pressed).toBe(true);
  });

  it("two panes on one origin are independent (a then|now split)", () => {
    const a = pane({ id: "same" });
    const b = pane({ id: "same" });
    deliver(ack("on"), a.origin, b.frame.win);
    expect(a.result.current.state).toBe("unknown");
    expect(b.result.current.state).toBe("on");
    act(() => a.result.current.toggle());
    expect(a.frame.post).toHaveBeenCalledTimes(1);
    expect(b.frame.post).not.toHaveBeenCalled();
  });

  it("re-sends the standing intent when the runtime says kb:read-ready", () => {
    savePrefs({ ...loadPrefs(), readingDefault: { [KB]: true } });
    const p = pane();
    const before = p.frame.post.mock.calls.length; // the mount-time send
    deliver({ kind: "kb:read-ready" }, p.origin, p.frame.win);
    expect(p.frame.post.mock.calls.length).toBe(before + 1);
    expect(p.frame.post.mock.calls.at(-1)?.[0]).toMatchObject({ kind: "kb:read", on: true });
    // A foreign ready is ignored.
    deliver({ kind: "kb:read-ready" }, "https://evil.example", p.frame.win);
    expect(p.frame.post.mock.calls.length).toBe(before + 1);
  });

  it("a new document in the frame voids the old ack and restates the intent at once", () => {
    savePrefs({ ...loadPrefs(), readingDefault: { [KB]: true } });
    const p = pane();
    deliver(ack("unsuitable", "media"), p.origin, p.frame.win);
    expect(p.result.current.state).toBe("unsuitable");
    p.frame.post.mockClear();
    act(() => p.result.current.onFrameLoad());
    expect(p.result.current.state).toBe("unknown");
    expect(p.frame.post).toHaveBeenCalledTimes(1);
    expect(p.frame.post.mock.calls[0][0]).toMatchObject({ kind: "kb:read", on: true });
  });

  it("a page the daemon styled at serve time acks 'on' after its ready: the row is pressed and the first r turns it OFF", () => {
    const p = pane();
    deliver({ kind: "kb:read-ready" }, p.origin, p.frame.win);
    deliver(ack("on"), p.origin, p.frame.win);
    // The iframe's load event follows the ready; it must not erase the ack.
    act(() => p.result.current.onFrameLoad());
    expect(p.result.current.pressed).toBe(true);
    p.frame.post.mockClear();
    act(() => p.result.current.toggle());
    expect(p.frame.post.mock.calls[0][0]).toMatchObject({ kind: "kb:read", on: false });
  });

  it("sends nothing at mount when there is neither a default nor a request", () => {
    const p = pane();
    expect(p.frame.post).not.toHaveBeenCalled();
  });

  it("repaints the annotator after an applied ack: chooser dismissed, refresh at one frame and again after the settle", () => {
    const p = pane({ annotateActive: true });
    deliver(ack("on"), p.origin, p.frame.win);
    expect(p.onSelectionChange).toHaveBeenCalledWith(null);
    expect(p.refresh).not.toHaveBeenCalled();
    act(() => void vi.advanceTimersByTime(20));
    expect(p.refresh).toHaveBeenCalledTimes(1);
    act(() => void vi.advanceTimersByTime(REPAINT_SETTLE_MS));
    expect(p.refresh).toHaveBeenCalledTimes(2);
    // Off repaints too.
    deliver(ack("off"), p.origin, p.frame.win);
    act(() => void vi.advanceTimersByTime(REPAINT_SETTLE_MS + 20));
    expect(p.refresh).toHaveBeenCalledTimes(4);
  });

  it("does not touch the annotator when it is not active, or on a refusal", () => {
    const idle = pane({ annotateActive: false });
    deliver(ack("on"), idle.origin, idle.frame.win);
    act(() => void vi.advanceTimersByTime(1000));
    expect(idle.onSelectionChange).not.toHaveBeenCalled();
    expect(idle.refresh).not.toHaveBeenCalled();

    const live = pane({ annotateActive: true });
    deliver(ack("unsuitable", "media"), live.origin, live.frame.win);
    act(() => void vi.advanceTimersByTime(1000));
    expect(live.onSelectionChange).not.toHaveBeenCalled();
    expect(live.refresh).not.toHaveBeenCalled();
  });

  it("re-sends the scheme on a theme change while applied, and only then", () => {
    const p = pane();
    act(() => void window.dispatchEvent(new Event(THEME_APPLIED_EVENT)));
    expect(p.frame.post).not.toHaveBeenCalled();
    act(() => p.result.current.toggle());
    deliver(ack("on"), p.origin, p.frame.win);
    p.frame.post.mockClear();
    document.documentElement.setAttribute("data-theme", "light");
    act(() => void window.dispatchEvent(new Event(THEME_APPLIED_EVENT)));
    expect(p.frame.post.mock.calls[0][0]).toEqual({ kind: "kb:read", on: true, scheme: "light" });
  });

  it("stops listening on unmount", () => {
    const p = pane();
    p.unmount();
    deliver(ack("on"), p.origin, p.frame.win);
    expect(p.result.current.state).toBe("unknown");
  });
});

describe("bare r key", () => {
  it("a forwarded kb:key r (replayed on body) toggles the focused pane only", () => {
    const a = vi.fn();
    const b = vi.fn();
    renderHook(() => useReadingKey(true, a));
    renderHook(() => useReadingKey(false, b));
    dispatchForwardedKey("r");
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).not.toHaveBeenCalled();
  });

  const key = (init: KeyboardEventInit = {}, target: EventTarget = document.body) =>
    act(() => {
      target.dispatchEvent(new KeyboardEvent("keydown", { key: "r", bubbles: true, cancelable: true, ...init }));
    });

  it("toggles on the focused pane and prevents the default", () => {
    const toggle = vi.fn();
    renderHook(() => useReadingKey(true, toggle));
    const ev = new KeyboardEvent("keydown", { key: "r", bubbles: true, cancelable: true });
    act(() => void document.body.dispatchEvent(ev));
    expect(toggle).toHaveBeenCalledTimes(1);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("an unfocused pane never acts", () => {
    const toggle = vi.fn();
    renderHook(() => useReadingKey(false, toggle));
    key();
    expect(toggle).not.toHaveBeenCalled();
  });

  it("ignores any modifier, other keys, and composition", () => {
    const toggle = vi.fn();
    renderHook(() => useReadingKey(true, toggle));
    for (const init of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }, { shiftKey: true }, { isComposing: true }]) {
      key(init);
    }
    key({ key: "R" });
    key({ key: "o" });
    expect(toggle).not.toHaveBeenCalled();
  });

  it("ignores typing in inputs, textareas, selects and contenteditable (CM6)", () => {
    const toggle = vi.fn();
    renderHook(() => useReadingKey(true, toggle));
    for (const tag of ["input", "textarea", "select"]) {
      const el = document.createElement(tag);
      document.body.appendChild(el);
      key({}, el);
    }
    const ce = document.createElement("div");
    Object.defineProperty(ce, "isContentEditable", { value: true });
    document.body.appendChild(ce);
    key({}, ce);
    expect(toggle).not.toHaveBeenCalled();
  });

  it("ignores the key while a modal dialog or sheet is open", () => {
    const toggle = vi.fn();
    renderHook(() => useReadingKey(true, toggle));
    const d = document.createElement("div");
    d.setAttribute("role", "dialog");
    d.setAttribute("aria-modal", "true");
    document.body.appendChild(d);
    key();
    expect(toggle).not.toHaveBeenCalled();
    d.remove();
    key();
    expect(toggle).toHaveBeenCalledTimes(1);
  });

  it("a phone's always-mounted reader-tools sheet only blocks the key while it is open", () => {
    const toggle = vi.fn();
    renderHook(() => useReadingKey(true, toggle));
    const sheet = document.createElement("aside");
    sheet.id = "kb-reader-sheet";
    sheet.setAttribute("role", "dialog");
    sheet.setAttribute("aria-modal", "true");
    document.body.appendChild(sheet);
    key(); // mounted but closed
    expect(toggle).toHaveBeenCalledTimes(1);
    const route = document.createElement("div");
    route.className = "detail detail--inspector-open";
    document.body.appendChild(route);
    key(); // open
    expect(toggle).toHaveBeenCalledTimes(1);
  });

  it("isReadingKey is false for a non-r key", () => {
    expect(isReadingKey(new KeyboardEvent("keydown", { key: "t" }))).toBe(false);
  });
});
