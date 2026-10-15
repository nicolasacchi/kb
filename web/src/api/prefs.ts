// User preferences applied to the document root via CSS variables.
// Persisted to localStorage; PATCH'd to /api/settings with a small
// debounce so the daemon also has a copy. localStorage is the durable
// cache (since the daemon's `ui` is in-memory only in v0.1).

import { applyTheme, coerceFamily } from "@kb/ui/themes/apply";
import { currentDaemonBase } from "./base";

/// The APPEARANCE axis (light | dark | system). The palette family is the
/// separate `themeFamily` pref below.
export type Theme = "light" | "dark" | "system";
export type Density = "compact" | "comfy" | "spacious";

// W3.M-d — where a BARE "/" cold entry lands. "grid" is the default and
// stays the default: map-home is an EVIDENCE-GATED promotion (standing rule
// 3), and the evidence lives in the local-only atlas-usage census
// (lib/census.ts), which has barely any rows yet. Until the census says
// otherwise the map is reachable only two ways — an explicit `?shell=map`
// URL, or this preference flipped deliberately in Settings → Preferences.
//
// FLIP CRITERION (record for a later session; do NOT flip on appetite).
// Read `censusRead()` in Settings → Preferences and promote map-home to the
// default ONLY when, over ≥30 days of ordinary use:
//   · `atlas.open`               ≥ 60   (the map is visited ~2×/day, not once)
//   · `atlas.selection.filter` + `atlas.selection.addToList`
//     + `atlas.selection.copyAgent` ≥ 20 (selections BECOME work, i.e. the
//                                         map is an instrument, not a poster)
//   · `home.map.pivot` / `home.map.open` ≥ 0.30 (≥30% of deliberate map-home
//                                         entries end in a list projection —
//                                         the shell's whole claim)
//   · `atlas.dimSearch.run`      ≥ 25   (the map is SEARCHED, not just panned)
// Anything short of all four and the grid stays home. Deliberately no
// prominent map entry point in NAV_ITEMS: a prominent placement would
// manufacture the very evidence this gate is supposed to measure.
export type Home = "grid" | "map";

export type Prefs = {
  theme: Theme;
  // OL F2 — the kbc-theme/1 family (`kb-classic` by default). localStorage is
  // the source of truth; OL S1 also PATCHes it to the daemon's `ui` blob
  // (`settingsPatchBody`) so kb-code can follow it. The pre-F2 `accent` pref
  // is gone: a stored `accent` is ignored on load and never written back.
  themeFamily: string;
  density: Density;
  // K1 — the last corpus the user was in, written on every active-kb change
  // and read ONCE on a cold bare-"/" entry to restore their space across
  // sessions. Browser-local only — deliberately OUT of the patchSettings
  // allow-list (it's a per-browser nav preference, not daemon UI state).
  lastKb?: string | null;
  // W3.M-d — the cold-entry home (see `Home` above). Same class as `lastKb`:
  // a browser-local UI preference, deliberately OUT of the patchSettings
  // allow-list (the daemon's `ui` blob carries theme/accent/density only) and
  // with NO `kb` CLI verb. RECORDED EXEMPTION to the CLI-parity rule: this is
  // not a server noun — nothing about it exists daemon-side, exactly like
  // theme/accent/density/lastKb, none of which have a CLI verb either.
  home?: Home;
  // W3.C/D3 — the remembered `/sessions` view (projects home vs the default
  // list). Same class as `lastKb`/`home`: a per-browser nav preference,
  // deliberately OUT of the patchSettings allow-list. D3 (synthesis-memo
  // R12): the DEFAULT landing stays "list" — the remembered preference only
  // ever promotes to "projects" once the operator has chosen it once; a bare
  // `/sessions` (no `?view=`) reads this ONCE on cold entry, exactly like
  // `lastKb` seeds the kb pill.
  lastSessionsView?: "projects" | "list";
  // F2 — the /sessions "hide trivial" toggle. Same class as `lastSessionsView`:
  // a per-browser nav preference, deliberately OUT of the patchSettings
  // allow-list. false (default) shows all sessions; true hides
  // substance=trivial rows. User choice is remembered via the URL query param
  // (`?substance=routine,substantive`), but the preference also tracks the
  // toggle state to restore it on return to /sessions.
  hideSessionsTrivial?: boolean;
};

const PREFS_KEY = "kb:prefs";

/// kb's default palette family: reproduces the hand-written dark/light palettes
/// kb shipped before kbc-theme/1 became the fleet theme.
export const KB_DEFAULT_FAMILY = "kb-classic";

export const DENSITY_SCALE: Record<Density, number> = {
  compact: 0.85,
  comfy: 1,
  spacious: 1.2,
};

export const DEFAULT_PREFS: Prefs = {
  theme: "dark",
  themeFamily: KB_DEFAULT_FAMILY,
  density: "comfy",
  lastKb: null,
  // The gate: grid is home until the census says otherwise.
  home: "grid",
  // D3: list stays the /sessions default until the operator visits projects
  // home at least once.
  lastSessionsView: "list",
  // F2: hide-trivial defaults to false (show all sessions)
  hideSessionsTrivial: false,
};

export function loadPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (raw) {
      // `accent` is a retired pref (OL F2): drop it so it is never written
      // back. `themeFamily` is coerced to a family the registry carries.
      const { accent: _retired, ...parsed } = JSON.parse(raw);
      void _retired;
      const merged = { ...DEFAULT_PREFS, ...parsed };
      return { ...merged, themeFamily: loadFamilyFrom(merged.themeFamily) };
    }
  } catch {
    // fallthrough
  }
  return DEFAULT_PREFS;
}

function loadFamilyFrom(family: unknown): string {
  return coerceFamily(typeof family === "string" ? family : null, KB_DEFAULT_FAMILY);
}

export function savePrefs(prefs: Prefs) {
  localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
}

// K1 — last-corpus persistence. saveLastKb is a no-op when unchanged so the
// per-active-kb-change writer doesn't thrash localStorage.
export function loadLastKb(): string | null {
  return loadPrefs().lastKb ?? null;
}

export function saveLastKb(kb: string) {
  const cur = loadPrefs();
  if (cur.lastKb === kb) return;
  savePrefs({ ...cur, lastKb: kb });
}

// K1 — decide whether a cold app entry should restore the last-used corpus,
// and to which kb. Returns the kb to seed (`navigate('/?kb=<it>')`), or null to
// leave the URL untouched. Pure so the gating — the bug-prone part — is
// unit-tested without a router.
export function coldSeedKb(opts: {
  kbs: string[]; // live kb names, in configured order
  explicitKb: string | null; // path/?kb= selection, if any
  pathname: string;
  lastKb: string | null;
}): string | null {
  const { kbs, explicitKb, pathname, lastKb } = opts;
  if (kbs.length === 0) return null; // kb list not loaded yet
  if (explicitKb) return null; // the user already has an explicit selection
  if (pathname !== "/") return null; // only a bare "/" cold entry seeds
  if (!lastKb) return null; // nothing remembered
  if (lastKb === kbs[0]) return null; // already the kbs[0] default → no-op
  if (!kbs.includes(lastKb)) return null; // ghost-kb guard (#13)
  return lastKb;
}

// W3.M-d — cold-entry home. Reads/writes ride the same prefs blob as
// `lastKb` above; an unknown/absent value degrades to "grid" so a hand-edited
// or older localStorage entry can never strand the operator on the map.
export function loadHome(): Home {
  const h = loadPrefs().home;
  return h === "map" ? "map" : "grid";
}

export function saveHome(home: Home) {
  const cur = loadPrefs();
  if (cur.home === home) return;
  savePrefs({ ...cur, home });
}

// W3.M-d — decide whether a cold app entry should open the MAP shell, and
// nothing else. Pure (same shape + same one-shot contract as `coldSeedKb`)
// so the gating — which is the correctness-critical part of the promotion
// gate — is unit-tested without a router.
//
// Deliberately conservative: only a TRULY bare "/" (no query string at all)
// can be redirected. An operator who followed any link carrying params
// (`/?kb=x`, `/?view=list`, `/?tags=…`) asked for that view explicitly and
// the shell must not hijack it, and a URL that already names a `shell` param
// (including `shell=grid`) is already explicit.
export function coldSeedShell(opts: {
  home: Home;
  pathname: string;
  search: string; // location.search, leading "?" included (or "")
}): "map" | null {
  const { home, pathname, search } = opts;
  if (home !== "map") return null;
  if (pathname !== "/") return null;
  if (search !== "" && search !== "?") return null;
  return "map";
}

// W3.C/D3 — remembered `/sessions` view. Reads/writes ride the same prefs
// blob as `lastKb`/`home`; an unknown/absent value degrades to "list" (the
// D3-ratified default) so a hand-edited or older localStorage entry never
// strands the operator on a view they didn't choose.
export function loadLastSessionsView(): "projects" | "list" {
  return loadPrefs().lastSessionsView === "projects" ? "projects" : "list";
}

export function saveLastSessionsView(view: "projects" | "list") {
  const cur = loadPrefs();
  if (cur.lastSessionsView === view) return;
  savePrefs({ ...cur, lastSessionsView: view });
}

// W3.C/D3 — decide whether a cold `/sessions` entry should promote to the
// projects home. Pure (the `coldSeedKb`/`coldSeedShell` shape) so the gating
// is unit-tested without a router. Conservative like `coldSeedShell`: only a
// TRULY bare `/sessions` (no query string at all) is eligible — a deep link
// carrying `?focus=`/`?q=`/anything else was asked for explicitly and must
// land exactly where it points, never redirected.
export function coldSeedSessionsView(opts: {
  search: string; // location.search, leading "?" included (or "")
  lastSessionsView: "projects" | "list";
}): "projects" | null {
  const { search, lastSessionsView } = opts;
  if (lastSessionsView !== "projects") return null;
  if (search !== "" && search !== "?") return null;
  return "projects";
}

// F2 — hide-trivial sessions preference. Mirrors `lastSessionsView`: a per-browser
// nav preference, deliberately OUT of the patchSettings allow-list. false (default)
// shows all sessions; true hides substance=trivial rows.
export function loadHideSessionsTrivial(): boolean {
  return loadPrefs().hideSessionsTrivial ?? false;
}

export function saveHideSessionsTrivial(hide: boolean) {
  const cur = loadPrefs();
  if (cur.hideSessionsTrivial === hide) return;
  savePrefs({ ...cur, hideSessionsTrivial: hide });
}

// Apply prefs to <html>: density as a CSS var, and the theme via the shared
// kbc-theme/1 engine, which sets data-theme (appearance), data-kbc-theme (the
// resolved family member the generated CSS selects on) and color-scheme, and
// follows OS light/dark flips for `system` on its own. Re-running is cheap.
export function applyPrefs(prefs: Prefs) {
  const root = document.documentElement;
  // The retired inline accent (pre-OL-F2 applyPrefs wrote it): clear any copy
  // still on a live document so the generated --accent is what paints.
  root.style.removeProperty("--accent");
  root.style.setProperty("--density", String(DENSITY_SCALE[prefs.density]));
  applyTheme(prefs.theme, loadFamilyFrom(prefs.themeFamily));
}

// v0.10 D2 — header Sun icon cycles dark → light → system. Returns the
// chosen theme so the caller can reflect it (e.g. update a tooltip).
const THEME_CYCLE: Theme[] = ["dark", "light", "system"];

export function cycleTheme(): Theme {
  const cur = loadPrefs();
  const next = THEME_CYCLE[(THEME_CYCLE.indexOf(cur.theme) + 1) % THEME_CYCLE.length];
  const updated = { ...cur, theme: next };
  savePrefs(updated);
  applyPrefs(updated);
  patchSettingsDebounced(updated);
  return next;
}

// Inline density cycle (kb2 redesign — the context-line ⊟ toggle). Mirrors
// cycleTheme: writes the same pref the Settings → Preferences control owns,
// so localStorage + the daemon copy stay in sync. Returns the chosen density.
const DENSITY_CYCLE: Density[] = ["comfy", "compact", "spacious"];

export function cycleDensity(): Density {
  const cur = loadPrefs();
  const next =
    DENSITY_CYCLE[(DENSITY_CYCLE.indexOf(cur.density) + 1) % DENSITY_CYCLE.length];
  const updated = { ...cur, density: next };
  savePrefs(updated);
  applyPrefs(updated);
  patchSettingsDebounced(updated);
  return next;
}

let patchTimer: ReturnType<typeof setTimeout> | null = null;
const PATCH_DEBOUNCE_MS = 500;

// The daemon-side allow-list is theme/density/themeFamily (the retired accent
// is no longer sent) — `lastKb` (K1) and `home` (W3.M-d) are browser-local nav
// preferences and are deliberately NOT sent: they are per-browser, and the
// daemon has no noun for either. OL S1: `themeFamily` rides along so the
// sibling kb-code daemon can read it back (`GET /api/kb/ui`) and follow it
// until its user picks a family there.
export function settingsPatchBody(prefs: Prefs): {
  theme: Theme;
  density: Density;
  themeFamily: string;
} {
  return {
    theme: prefs.theme,
    density: prefs.density,
    themeFamily: loadFamilyFrom(prefs.themeFamily),
  };
}

export function patchSettingsDebounced(prefs: Prefs) {
  if (patchTimer) clearTimeout(patchTimer);
  patchTimer = setTimeout(() => {
    fetch(`${currentDaemonBase()}/api/settings`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(settingsPatchBody(prefs)),
    }).catch(() => {
      // Best-effort. localStorage is the durable cache; the daemon's
      // copy is convenience-only in v0.1.
    });
  }, PATCH_DEBOUNCE_MS);
}
