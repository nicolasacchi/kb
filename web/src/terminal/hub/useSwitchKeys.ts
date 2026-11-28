// HUBSWITCH — the `terminals` doc-only key scope's dispatcher: `[` / `]` step
// to the previous / next live terminal, `n` / `N` to the next / previous one
// that needs you. ONE document keydown listener, mounted only by HubSwitcher.
// Keys belong to the pane while it has focus: any event from an editable
// field, the xterm viewport or a dialog is ignored, as is any modifier chord,
// key repeat and IME composition. preventDefault only when it navigates.

import { useEffect, useRef } from "react";
import { useNavigate } from "react-router-dom";
import { buildHubUrl, type HubRow } from "./termHub";
import { stepTerm } from "./switchModel";

const IGNORED_TARGET =
  'input, textarea, select, [contenteditable=""], [contenteditable="true"], [contenteditable="plaintext-only"], .xterm, [role="dialog"], [aria-modal="true"]';

function ignoredTarget(t: EventTarget | null): boolean {
  const el = t as (Element & { isContentEditable?: boolean }) | null;
  if (!el || typeof el.closest !== "function") return false;
  return el.isContentEditable === true || el.closest(IGNORED_TARGET) !== null;
}

type Plan = { dir: 1 | -1; mode: "live" | "needs" };

function planFor(key: string): Plan | null {
  switch (key) {
    case "]":
      return { dir: 1, mode: "live" };
    case "[":
      return { dir: -1, mode: "live" };
    case "n":
      return { dir: 1, mode: "needs" };
    case "N":
      return { dir: -1, mode: "needs" };
    default:
      return null;
  }
}

export function useSwitchKeys(rows: readonly HubRow[], currentId: string | null): void {
  const navigate = useNavigate();
  const latest = useRef({ rows, currentId });
  latest.current = { rows, currentId };
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.defaultPrevented || e.repeat || e.isComposing) return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      const plan = planFor(e.key);
      if (plan === null) return;
      if (ignoredTarget(e.target)) return;
      const { rows: r, currentId: cur } = latest.current;
      const next = stepTerm(r, cur, plan.dir, plan.mode);
      if (next === null || next === cur) return;
      e.preventDefault();
      void navigate(buildHubUrl({ id: next }));
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [navigate]);
}
