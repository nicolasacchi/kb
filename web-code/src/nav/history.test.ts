// V70-A6 — the router adapter's contract, both branches.
//
// The vitest environment is `node` (see `vitest.config.ts`), so this drives
// the adapters against a minimal fake `window`. That is enough for what the
// unit test can honestly claim: which adapter is chosen, that in-app Back is
// the BROWSER's back on both, and that the Navigation adapter intercepts
// TRAVERSALS ONLY (the property that keeps React Router's push path
// untouched). The real browser behaviour — per-entry scroll restore across a
// traversal — is `e2e/scroll-restore.spec.ts`, in chromium and in the opt-in
// Firefox project.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createHistoryAdapter, hasNavigationApi, type NavEvent } from "./history";

interface Listener {
  (e: Event): void;
}

function fakeWindow(withNavigation: boolean) {
  const winListeners = new Map<string, Listener[]>();
  const navListeners = new Map<string, Listener[]>();
  const calls: string[] = [];
  const entries = [
    { key: "k0", url: "/a", index: 0, getState: () => ({ n: 0 }) },
    { key: "k1", url: "/b", index: 1, getState: () => ({ n: 1 }) },
  ];
  const navigation = {
    currentEntry: entries[1],
    entries: () => entries,
    back: () => calls.push("nav.back"),
    forward: () => calls.push("nav.forward"),
    traverseTo: (k: string) => calls.push(`nav.traverseTo(${k})`),
    updateCurrentEntry: (o: { state: unknown }) => calls.push(`nav.updateState(${JSON.stringify(o.state)})`),
    addEventListener: (t: string, l: Listener) => {
      navListeners.set(t, [...(navListeners.get(t) ?? []), l]);
    },
    removeEventListener: (t: string, l: Listener) => {
      navListeners.set(t, (navListeners.get(t) ?? []).filter((x) => x !== l));
    },
  };
  const win = {
    location: { href: "/b" },
    history: {
      state: { rr: 1 },
      pushState: (_s: unknown, _t: string, u: string) => calls.push(`history.pushState(${u})`),
      replaceState: (_s: unknown, _t: string, u: string) => calls.push(`history.replaceState(${u})`),
      back: () => calls.push("history.back"),
      forward: () => calls.push("history.forward"),
    },
    addEventListener: (t: string, l: Listener) => {
      winListeners.set(t, [...(winListeners.get(t) ?? []), l]);
    },
    removeEventListener: (t: string, l: Listener) => {
      winListeners.set(t, (winListeners.get(t) ?? []).filter((x) => x !== l));
    },
    ...(withNavigation ? { navigation } : {}),
  } as unknown as Window;
  function fireNav(ev: Record<string, unknown>) {
    for (const l of navListeners.get("navigate") ?? []) l(ev as unknown as Event);
  }
  function firePop() {
    for (const l of winListeners.get("popstate") ?? []) l({} as Event);
  }
  return { win, calls, fireNav, firePop };
}

function navigateEvent(type: string, intercepted: string[]) {
  return {
    navigationType: type,
    canIntercept: true,
    hashChange: false,
    downloadRequest: null,
    destination: { url: "/dest", key: "kX" },
    intercept: (o: Record<string, unknown>) => intercepted.push(JSON.stringify({ scroll: o.scroll, focusReset: o.focusReset })),
  };
}

describe("adapter selection is pure capability detection", () => {
  it("picks the Navigation adapter where the API exists", () => {
    const { win } = fakeWindow(true);
    expect(hasNavigationApi(win)).toBe(true);
    expect(createHistoryAdapter(win).kind).toBe("navigation");
  });
  it("falls back where it does not", () => {
    const { win } = fakeWindow(false);
    expect(hasNavigationApi(win)).toBe(false);
    expect(createHistoryAdapter(win).kind).toBe("history");
  });
});

describe("in-app Back IS the browser's back (there is no second stack)", () => {
  it("navigation adapter", () => {
    const { win, calls } = fakeWindow(true);
    const a = createHistoryAdapter(win);
    a.back();
    a.forward();
    expect(calls).toEqual(["nav.back", "nav.forward"]);
  });
  it("history adapter", () => {
    const { win, calls } = fakeWindow(false);
    const a = createHistoryAdapter(win);
    a.back();
    a.forward();
    expect(calls).toEqual(["history.back", "history.forward"]);
  });
});

describe("the navigation adapter intercepts TRAVERSALS ONLY", () => {
  let intercepted: string[];
  beforeEach(() => {
    intercepted = [];
  });

  it("a traversal gets manual scroll + manual focus reset", () => {
    const { win, fireNav } = fakeWindow(true);
    createHistoryAdapter(win);
    fireNav(navigateEvent("traverse", intercepted));
    expect(intercepted).toEqual([JSON.stringify({ scroll: "manual", focusReset: "manual" })]);
  });

  it("a push is left ENTIRELY alone — React Router owns it", () => {
    // Intercepting a push would turn a plain cross-document `<a href>` into a
    // same-document navigation React Router was never told about.
    const { win, fireNav } = fakeWindow(true);
    createHistoryAdapter(win);
    fireNav(navigateEvent("push", intercepted));
    fireNav(navigateEvent("replace", intercepted));
    expect(intercepted).toEqual([]);
  });

  it("still reports every navigation to subscribers, intercepted or not", () => {
    const { win, fireNav } = fakeWindow(true);
    const a = createHistoryAdapter(win);
    const seen: NavEvent[] = [];
    a.onNavigate((e) => seen.push(e));
    fireNav(navigateEvent("push", intercepted));
    fireNav(navigateEvent("traverse", intercepted));
    expect(seen.map((e) => e.kind)).toEqual(["push", "traverse"]);
    expect(seen[1].key).toBe("kX");
  });
});

describe("entry addressing", () => {
  it("the navigation adapter names the current slot and can enumerate", () => {
    const { win, calls } = fakeWindow(true);
    const a = createHistoryAdapter(win);
    expect(a.currentKey()).toBe("k1");
    expect(a.entries?.()).toEqual([
      { key: "k0", url: "/a", index: 0 },
      { key: "k1", url: "/b", index: 1 },
    ]);
    a.traverseTo?.("k0");
    expect(calls).toContain("nav.traverseTo(k0)");
  });

  it("the history adapter says it CANNOT, rather than lying with an empty list", () => {
    const { win } = fakeWindow(false);
    const a = createHistoryAdapter(win);
    expect(a.currentKey()).toBeNull();
    expect(a.entries).toBeUndefined();
    expect(a.traverseTo).toBeUndefined();
  });

  it("a refused state write is swallowed, never thrown out of a handler", () => {
    const { win } = fakeWindow(true);
    const nav = (win as unknown as { navigation: { updateCurrentEntry: () => void } }).navigation;
    nav.updateCurrentEntry = () => {
      throw new Error("InvalidStateError");
    };
    const a = createHistoryAdapter(win);
    expect(() => a.updateState({ x: 1 })).not.toThrow();
  });
});

describe("popstate is the history adapter's traversal signal", () => {
  it("reports it", () => {
    const { win, firePop } = fakeWindow(false);
    const a = createHistoryAdapter(win);
    const seen: NavEvent[] = [];
    a.onNavigate((e) => seen.push(e));
    firePop();
    expect(seen.map((e) => e.kind)).toEqual(["traverse"]);
  });

  it("unsubscribes cleanly", () => {
    const { win, firePop } = fakeWindow(false);
    const a = createHistoryAdapter(win);
    const cb = vi.fn();
    a.onNavigate(cb)();
    firePop();
    expect(cb).not.toHaveBeenCalled();
  });
});
