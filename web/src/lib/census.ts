/*
 * Local-only feature census (W1 — standing rule 3: evidence-gated promotion).
 *
 * Counters live in localStorage and NEVER leave the machine — they exist so
 * roadmap promotions (map-home vs session-replay, ambient yes/no) are decided
 * from observed behavior instead of appetite. This is not telemetry: nothing
 * is sent anywhere, and the Settings panel that renders it says so.
 */

const KEY = "kb:census";

export type Census = Record<string, number>;

/** Known counters with human labels (Settings renders these; unknown keys
 *  still display raw). Keep names stable — they are the evidence record. */
export const CENSUS_LABELS: Record<string, string> = {
  "atlas.open": "Atlas opens",
  "atlas.camera.save": "Atlas cameras saved",
  "atlas.camera.recall": "Atlas cameras recalled",
  // W2.3b — atlas working-set instrument.
  "atlas.lasso": "Atlas lasso selections drawn",
  "atlas.selection.filter": "Atlas selections opened as a gallery filter",
  "atlas.selection.addToList": "Atlas selections added to a list",
  "atlas.selection.copyAgent": "Atlas selections copied for an agent",
  "resurface.click": "Resurface items opened",
  "resurface.review": "Resurface review sessions",
  "search.zero_hit.retry": "Zero-hit query retries",
  "lobby.explore": "Lobby explore clicks",
  // W2.10 — activity-calendar day-cell clicks (deep-link into the gallery).
  "calendar.day": "Activity-calendar day clicks",
  // W3.M-c — the atlas's dim-in-place search overlay (MapSearchOverlay):
  // evidence for the map-home promotion gate.
  "atlas.dimSearch.run": "Atlas search-dim overlay runs",
  "atlas.dimSearch.clear": "Atlas search-dim overlay cleared",
  // W3.C-b — the reflection canvas (?view=canvas): does anyone actually
  // brush the four time tracks, and does a brush ever become a result set?
  // Evidence for keeping (or retiring) the view — density counters only, in
  // the calm-computing posture the canvas itself keeps.
  "canvas.brush": "Reflection-canvas brushes",
  "canvas.pivot": "Reflection-canvas brushes opened as a gallery filter",
  // W3.F-c — the dual-field atlas: is the operator's own map ever drawn,
  // and does anyone actually place a dot by hand? Density counters only —
  // evidence for keeping or retiring the overlay, never a score or a goal.
  "atlas.field.show": "Operator-field overlays shown",
  "atlas.field.heat": "Disagreement heat modes entered",
  "atlas.field.place": "Artifacts hand-placed on the operator field",
  // W3.F-c — loci tours. A tour IS a reading list (no new store), so these
  // count NAVIGATION only: walks started and stops handed off to the
  // reader. There is deliberately no "tour completed" counter — completion
  // is not a thing kb records (README → Non-goals).
  "atlas.tour.start": "Loci tours started",
  "atlas.tour.open": "Loci-tour stops opened in the reader",
  // W3.M-d — the map-home shell (`?shell=map`). THE gate counters: map-home
  // stays opt-in (Prefs.home defaults to "grid") until these say the map is
  // load-bearing navigation rather than a poster. The flip criterion — what
  // numbers would justify promoting map-home to the default — is recorded in
  // `api/prefs.ts` beside the `Home` type, deliberately NOT here, so the
  // decision lives next to the default it would change.
  "home.map.open": "Map-home shell opened",
  "home.map.select": "Map-home selections made",
  "home.map.pivot": "Map-home selections opened as a gallery list",
  // U3 — highlight → save as memory. Does the reader's selection chooser
  // actually feed memory, or is cite/add-to-list the whole story? Two
  // counters: the chooser action opened, and a memory actually written
  // (the gap between them IS the confirm step earning its place). Density
  // only — never a target, never a streak; kb records no goals.
  "selection.remember.open": "Highlight → memory prompts opened",
  "selection.remember.save": "Highlights saved as a memory",
};

export function censusRead(): Census {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {};
    const out: Census = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v === "number" && Number.isFinite(v)) out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

export function censusBump(counter: string, by = 1): void {
  try {
    const c = censusRead();
    c[counter] = (c[counter] ?? 0) + by;
    localStorage.setItem(KEY, JSON.stringify(c));
  } catch {
    /* storage unavailable (private mode) — the census is best-effort */
  }
}

export function censusReset(): void {
  try {
    localStorage.removeItem(KEY);
  } catch {
    /* ignore */
  }
}
