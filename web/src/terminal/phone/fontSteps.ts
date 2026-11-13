// T17 — the persisted phone font size. One of FONT_STEPS or nothing.

import { DEFAULT_FONT, FONT_STEPS } from "./scrollerCore";

export const FONT_KEY = "kb-term-phone-font";

function isStep(n: number): boolean {
  return (FONT_STEPS as readonly number[]).includes(n);
}

/** The saved font, or DEFAULT_FONT for an absent, foreign or unreadable value. */
export function readFont(): number {
  try {
    const raw = window.localStorage.getItem(FONT_KEY);
    if (raw === null) return DEFAULT_FONT;
    const n = Number(raw);
    return isStep(n) ? n : DEFAULT_FONT;
  } catch {
    return DEFAULT_FONT;
  }
}

/** Persist a step; a non-step or a blocked storage (private mode) is ignored. */
export function writeFont(px: number): void {
  if (!isStep(px)) return;
  try {
    window.localStorage.setItem(FONT_KEY, String(px));
  } catch {
    // private mode: the choice just does not persist
  }
}
