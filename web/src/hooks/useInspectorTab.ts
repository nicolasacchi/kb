import { useCallback, useEffect, useState } from "react";

// I1 — which sub-panel the reader's right-rail inspector is showing. One
// localStorage key per surface (like useInspectorCollapsed). The default is the
// LAST value the operator chose, persisted — so re-opening any artifact restores
// the tab you were on (operator request) instead of resetting to a fixed tab.
//
// State key: `kb:inspector-tab.<surface>`. Same-tab instances sync via a
// CustomEvent; cross-tab via the `storage` event (mirrors useInspectorCollapsed).

// v0.22 — the merged overview tab is FIRST and labelled "About" (it stacks
// every section, the old single-scroll view). The standalone "about" tab is
// gone; its content (Reading/Identifier/Metrics) now lives under this merged
// tab. The internal KEY stays `"all"` so the `show()` stack-all predicate and
// persisted localStorage values keep working; a legacy persisted `"about"`
// value falls back to the default ("all") via the `INSPECTOR_TABS.includes`
// validation in `read()` — a free migration.
//
// W2.1 — "story" is the biography timeline (origin session → sessions that
// touched the artifact → comments → versions). Appended last so it stacks
// into "all" without reordering the existing five tabs; a legacy persisted
// value from before this tab existed still validates fine (`includes`).
export type InspectorTab =
  | "all"
  | "meta"
  | "links"
  | "folder"
  | "sessions"
  | "story";

export const INSPECTOR_TABS: readonly InspectorTab[] = [
  "all",
  "meta",
  "links",
  "folder",
  "sessions",
  "story",
];

// Default is "all" (the merged About overview) — a fresh visit shows
// everything stacked; the rail lets the operator focus into a sub-tab and that
// choice persists (restore-on-open).
const DEFAULT_TAB: InspectorTab = "all";

function read(key: string): InspectorTab {
  if (typeof localStorage === "undefined") return DEFAULT_TAB;
  try {
    const v = localStorage.getItem(`kb:inspector-tab.${key}`);
    return INSPECTOR_TABS.includes(v as InspectorTab)
      ? (v as InspectorTab)
      : DEFAULT_TAB;
  } catch {
    return DEFAULT_TAB;
  }
}

function write(key: string, value: InspectorTab) {
  try {
    localStorage.setItem(`kb:inspector-tab.${key}`, value);
    window.dispatchEvent(new CustomEvent(`kb:inspector-tab.${key}`));
  } catch {
    // localStorage denied — state still updates this session; a reload loses it.
  }
}

export function useInspectorTab(surface: string): {
  tab: InspectorTab;
  setTab: (next: InspectorTab) => void;
} {
  const [tab, setLocal] = useState<InspectorTab>(() => read(surface));

  useEffect(() => {
    const evt = `kb:inspector-tab.${surface}`;
    const onChange = () => setLocal(read(surface));
    window.addEventListener("storage", onChange);
    window.addEventListener(evt, onChange);
    return () => {
      window.removeEventListener("storage", onChange);
      window.removeEventListener(evt, onChange);
    };
  }, [surface]);

  const setTab = useCallback(
    (next: InspectorTab) => {
      setLocal(next);
      write(surface, next);
    },
    [surface],
  );

  return { tab, setTab };
}
