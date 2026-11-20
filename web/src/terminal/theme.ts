// FUTERMFIT — the xterm theme, from the Profile's palette when it names one
// and from the app's tokens otherwise. Selection, inactive selection and the
// cursor accent ALWAYS come from tokens (so the selection is visible on the
// light themes too). The panel re-runs this when the SPA theme changes
// (`THEME_APPLIED_EVENT`), since the tokens it reads are per-theme.

import type { ITheme } from "@xterm/xterm";

export type TokenSource = Pick<CSSStyleDeclaration, "getPropertyValue">;

const token = (css: TokenSource, name: string): string => css.getPropertyValue(name).trim();

export function readTheme(profileTheme: unknown, css: TokenSource = getComputedStyle(document.documentElement)): ITheme {
  const out: ITheme = {};
  const set = (key: keyof ITheme, value: string): void => {
    if (value) out[key] = value;
  };
  // The terminal background IS the panel background (`--bg-panel-2`), so the
  // host and the terminal never show two different blacks.
  set("background", token(css, "--bg-panel-2"));
  set("foreground", token(css, "--ink"));
  set("cursor", token(css, "--accent"));
  set("cursorAccent", token(css, "--bg-panel-2"));
  set("selectionBackground", token(css, "--accent-bor"));
  set("selectionInactiveBackground", token(css, "--accent-bg"));
  if (profileTheme !== null && typeof profileTheme === "object") {
    // Only string color slots are honored; unknown keys are dropped.
    const slots = profileTheme as Record<string, unknown>;
    for (const key of ["background", "foreground", "cursor"] as const) {
      const v = slots[key];
      if (typeof v === "string") out[key] = v;
    }
    if (typeof slots.background === "string") out.cursorAccent = slots.background;
  }
  return out;
}
