// OL S1 - the theme family travels kb -> kb-code, one way.
//
// kb persists its palette family in its `ui` settings; the daemon relays it
// (`GET /api/kb/ui`). On boot, a browser that has NOT chosen a family in
// kb-code applies kb's; a family chosen in kb-code always wins and is never
// written back (`prefs.setThemePrefs` is the only writer, and it writes
// kb-code's own blob only). Only the FAMILY follows - the light/dark
// appearance stays kb-code's own pref.

import { fetchKbUi } from "../api/client";
import type { KbUiOut } from "../api/types";
import {
  applyTheme,
  hasLocalThemeFamily,
  loadTheme,
  setFollowedThemeFamily,
  themeFamilySource,
  type ThemeFamilySource,
} from "./prefs";

/// `[a-z0-9-]{1,40}` - the same shape kb and the relay enforce. The registry
/// check (does THIS bundle carry the family?) happens in
/// `setFollowedThemeFamily`.
const FAMILY_SLUG = /^[a-z0-9-]{1,40}$/;

/// The family a relay answer asks us to follow, or null (absent, malformed,
/// or kb unavailable). Pure.
export function familyFromKbUi(ui: KbUiOut | null | undefined): string | null {
  const f = ui?.theme_family;
  return typeof f === "string" && FAMILY_SLUG.test(f) ? f : null;
}

/// The picker's source caption; `default` (nothing chosen, nothing to
/// follow) says nothing.
export function themeSourceCaption(source: ThemeFamilySource): string | null {
  switch (source) {
    case "kb":
      return "Following kb";
    case "local":
      return "Chosen here";
    default:
      return null;
  }
}

/// Boot-time follow. Never throws and never asks kb at all when the user has
/// already chosen a family here. Repaints (and tells the theme controls) only
/// when the follow actually changed what is on screen.
export async function followKbTheme(
  fetchUi: (signal?: AbortSignal) => Promise<KbUiOut> = fetchKbUi,
): Promise<void> {
  if (hasLocalThemeFamily()) return;
  let family: string | null;
  try {
    family = familyFromKbUi(await fetchUi());
  } catch {
    return;
  }
  // The user may have picked a family while the request was in flight.
  if (hasLocalThemeFamily()) return;
  setFollowedThemeFamily(family);
  if (themeFamilySource() !== "kb") return;
  applyTheme(loadTheme());
  window.dispatchEvent(new CustomEvent("kbc:theme.changed", { detail: loadTheme() }));
}
