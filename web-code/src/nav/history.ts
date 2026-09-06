// V70-A6 — the router adapter (§P7, F2 in the linked-tabs research).
//
// ONE interface, TWO adapters, and a rule that makes them interchangeable:
// **in-app Back is the browser's Back**. `nav.back` (`u`), the pane arrows,
// mouse buttons 4/5 and the browser chrome all end up in `adapter.back()`,
// which is `navigation.back()` or `history.back()`. There is no second stack
// that could disagree with the browser's — the failure the research names as
// the one that destroys trust in both ("Two histories that disagree").
//
// WHAT THE NAVIGATION ADAPTER BUYS
//   * `entry.key` — a stable id for a history SLOT that survives URL/state
//     changes on that slot, and is DIFFERENT for two visits to the same URL.
//     That is the correct key for a scroll offset; a URL is not (Back onto
//     the same list twice must restore two different offsets).
//   * `intercept({ scroll: "manual", focusReset: "manual" })` on TRAVERSALS —
//     the browser's own restore is documented as landing "unpredictably and
//     often at the wrong times" for async-rendered SPAs (WICG
//     navigation-api#187), so we suppress it and restore ourselves once the
//     route has actually laid out.
//   * `entries()` / `traverseTo(key)` — the platform's own answer to "build a
//     custom jumplist", exposed here but not yet consumed (the pane jumplist
//     is `lib/navHistory.ts`; wiring it to `traverseTo` needs the per-entry
//     mapping a later unit adds).
//
// WHAT IT DELIBERATELY DOES NOT DO
//   * It NEVER intercepts a push or a replace. React Router (`BrowserRouter`
//     in `main.tsx`) still owns rendering and still moves through
//     `history.pushState`; intercepting a push would turn a plain
//     cross-document `<a href>` into a same-document one that React Router
//     was never told about. Traversals are the only navigation type whose
//     interception can change nothing but scroll/focus behaviour.
//   * It NEVER puts durable per-hop metadata in `navigate(..., { info })`:
//     `info` is not replayed on back/forward, which the research calls "the
//     single most important implementation fact in this report". The `via`
//     edge lives in the URL (`?via=`) and in `lib/trail.ts`.
//
// BROWSER FLOOR (also recorded in `web-code/e2e/README.md`):
//   * Navigation API — Chrome/Edge 102+, Firefox 145+ (Baseline "newly
//     available", January 2026). Where it is absent the History adapter is
//     used and scroll restoration keys on the normalised URL instead of
//     `entry.key`; nothing else changes. The e2e suite runs BOTH: chromium
//     exercises the Navigation adapter, the opt-in `KBC_E2E_FIREFOX=1`
//     project is what proves the History fallback still reads code.
//   * `history.scrollRestoration = "manual"` (set in `main.tsx`) is the floor
//     under both — it is what stops the browser fighting our own restore.

export type NavKind = "push" | "replace" | "traverse" | "reload";

export interface NavEvent {
  kind: NavKind;
  url: string;
  /// The DESTINATION entry's key when the platform can name it, else `null`.
  key: string | null;
}

export interface NavEntryInfo {
  key: string;
  url: string;
  index: number;
}

/// Developer state stored on a history entry. Structured-cloneable only.
export type NavEntryState = Record<string, unknown>;

export interface HistoryAdapter {
  /// Which implementation is live — surfaced so `e2e` and the `?` sheet can
  /// state the truth rather than assuming the good one.
  readonly kind: "navigation" | "history";
  push(url: string, state?: NavEntryState): void;
  replace(url: string, state?: NavEntryState): void;
  back(): void;
  forward(): void;
  /// The whole same-origin entry list — Navigation API only (`undefined` on
  /// the History adapter, because the History API never allowed reading it).
  entries?(): NavEntryInfo[];
  traverseTo?(key: string): void;
  /// The current entry's stable key, or `null` when the platform has none.
  /// This is the scroll layer's per-entry slot id.
  currentKey(): string | null;
  getState(): NavEntryState | null;
  updateState(patch: NavEntryState): void;
  /// Subscribe to navigations. Returns the unsubscribe.
  onNavigate(cb: (e: NavEvent) => void): () => void;
  dispose(): void;
}

// The Navigation API is not in TypeScript's DOM lib on the version this repo
// pins, so the shape is declared narrowly here — only the members used.
interface NavigationHistoryEntryLike {
  key: string;
  url: string | null;
  index: number;
  getState(): unknown;
}
interface NavigateEventLike extends Event {
  navigationType: NavKind;
  canIntercept: boolean;
  hashChange: boolean;
  downloadRequest: string | null;
  destination: { url: string; key?: string };
  intercept(opts: {
    handler?: () => Promise<void>;
    scroll?: "after-transition" | "manual";
    focusReset?: "after-transition" | "manual";
  }): void;
}
interface NavigationLike extends EventTarget {
  currentEntry: NavigationHistoryEntryLike | null;
  entries(): NavigationHistoryEntryLike[];
  back(): unknown;
  forward(): unknown;
  traverseTo(key: string): unknown;
  updateCurrentEntry(opts: { state: unknown }): void;
}

interface WindowWithNavigation extends Window {
  navigation?: NavigationLike;
}

export function hasNavigationApi(win: Window = window): boolean {
  const nav = (win as WindowWithNavigation).navigation;
  return (
    !!nav &&
    typeof nav.entries === "function" &&
    typeof nav.traverseTo === "function" &&
    typeof (nav as unknown as { addEventListener?: unknown }).addEventListener === "function"
  );
}

function createNavigationAdapter(win: Window): HistoryAdapter {
  const nav = (win as WindowWithNavigation).navigation as NavigationLike;
  const subs = new Set<(e: NavEvent) => void>();

  const onNavigate = (ev: Event) => {
    const e = ev as NavigateEventLike;
    // TRAVERSALS ONLY (see the module doc). A push/replace is React Router's
    // and is left completely alone.
    if (e.navigationType === "traverse" && e.canIntercept && !e.hashChange && e.downloadRequest === null) {
      try {
        e.intercept({
          scroll: "manual",
          focusReset: "manual",
          // The handler resolves immediately: React Router re-renders off
          // `popstate`, which still fires for an intercepted same-document
          // traversal. All this interception buys is the two "manual"s.
          handler: () => Promise.resolve(),
        });
      } catch {
        // A racing navigation can make `intercept` throw — the traversal
        // still happens, we just lose the manual-scroll suppression for it.
      }
    }
    const emitted: NavEvent = {
      kind: e.navigationType,
      url: e.destination?.url ?? win.location.href,
      key: e.destination?.key ?? null,
    };
    for (const cb of subs) cb(emitted);
  };
  nav.addEventListener("navigate", onNavigate);

  return {
    kind: "navigation",
    push(url) {
      win.history.pushState(win.history.state, "", url);
    },
    replace(url) {
      win.history.replaceState(win.history.state, "", url);
    },
    back() {
      nav.back();
    },
    forward() {
      nav.forward();
    },
    entries() {
      return nav.entries().map((e) => ({ key: e.key, url: e.url ?? "", index: e.index }));
    },
    traverseTo(key) {
      try {
        nav.traverseTo(key);
      } catch {
        // An entry that has been evicted/disposed — an honest no-op rather
        // than a thrown error out of a click handler.
      }
    },
    currentKey() {
      return nav.currentEntry?.key ?? null;
    },
    getState() {
      const s = nav.currentEntry?.getState();
      return s && typeof s === "object" ? (s as NavEntryState) : null;
    },
    updateState(patch) {
      try {
        nav.updateCurrentEntry({ state: { ...(this.getState() ?? {}), ...patch } });
      } catch {
        // Spec-refused while a navigation is in flight. The scroll layer
        // never depends on this (it keys sessionStorage on `entry.key`);
        // this method exists for callers who can tolerate a refusal.
      }
    },
    onNavigate(cb) {
      subs.add(cb);
      return () => subs.delete(cb);
    },
    dispose() {
      nav.removeEventListener("navigate", onNavigate);
      subs.clear();
    },
  };
}

function createHistoryFallbackAdapter(win: Window): HistoryAdapter {
  const subs = new Set<(e: NavEvent) => void>();
  const onPop = () => {
    const e: NavEvent = { kind: "traverse", url: win.location.href, key: null };
    for (const cb of subs) cb(e);
  };
  win.addEventListener("popstate", onPop);
  return {
    kind: "history",
    push(url) {
      win.history.pushState(win.history.state, "", url);
      for (const cb of subs) cb({ kind: "push", url, key: null });
    },
    replace(url) {
      win.history.replaceState(win.history.state, "", url);
      for (const cb of subs) cb({ kind: "replace", url, key: null });
    },
    back() {
      win.history.back();
    },
    forward() {
      win.history.forward();
    },
    // `entries`/`traverseTo` are ABSENT, not stubbed: the History API cannot
    // enumerate or address entries, and a stub returning `[]` would let a
    // caller believe the list is empty rather than unavailable.
    currentKey() {
      return null;
    },
    getState() {
      const s = win.history.state;
      return s && typeof s === "object" ? (s as NavEntryState) : null;
    },
    updateState(patch) {
      const s = this.getState() ?? {};
      win.history.replaceState({ ...s, ...patch }, "", win.location.href);
    },
    onNavigate(cb) {
      subs.add(cb);
      return () => subs.delete(cb);
    },
    dispose() {
      win.removeEventListener("popstate", onPop);
      subs.clear();
    },
  };
}

/// Pick the adapter for `win`. Pure dispatch on capability — never a user
/// agent sniff, and never a preference: where the Navigation API exists it
/// is strictly better here (per-entry keys and manual scroll), and where it
/// does not the fallback loses exactly two things, both named in the module
/// doc.
export function createHistoryAdapter(win: Window = window): HistoryAdapter {
  return hasNavigationApi(win) ? createNavigationAdapter(win) : createHistoryFallbackAdapter(win);
}

// One adapter per document — the scroll layer, the pane arrows and
// `nav/navigate.ts` all have to agree about which entry is current, and two
// adapters would each hold their own subscriber set over the same events.
let shared: HistoryAdapter | null = null;

export function historyAdapter(): HistoryAdapter {
  if (typeof window === "undefined") {
    // SSR / a node-environment unit test: a null object, so importing this
    // module never touches a global that is not there.
    return {
      kind: "history",
      push() {},
      replace() {},
      back() {},
      forward() {},
      currentKey: () => null,
      getState: () => null,
      updateState() {},
      onNavigate: () => () => {},
      dispose() {},
    };
  }
  if (!shared) shared = createHistoryAdapter(window);
  return shared;
}

/// Test-only: drop the shared adapter so the next `historyAdapter()` rebuilds
/// it against a fresh (possibly stubbed) `window`.
export function _resetHistoryAdapterForTests(): void {
  shared?.dispose();
  shared = null;
}
