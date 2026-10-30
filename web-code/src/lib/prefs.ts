// F1 — kb-code's own small prefs module: persists the last repo the operator
// browsed so a cold `/` entry lands them back where they left off, mirroring
// kb's own `web/src/api/prefs.ts` K1 pattern (root CLAUDE.md invariant #33).
// Theme (`dark` | `light` | `system`) is also browser-local here — applied
// via `data-theme` + `color-scheme` before first paint. Unlike kb's SPA we
// do not PATCH the daemon. Other fields stay scoped to the chrome they serve
// (repo pill, sticky context, lenses).
//
// V70-A7 — kbc-theme/1 splits what used to be one `theme` field into two
// orthogonal axes, because a catalogue of families × appearances is not a
// three-item cycle:
//
//   appearance  light | dark | system   (the stored key is still `theme`,
//                                        so an existing blob keeps working)
//   themeFamily "kbc" | "catppuccin" | … (the registry family id)
//
// `data-theme` still carries the appearance for the structural dark/light
// bit CM6 and the existing CSS rely on. `data-kbc-theme` carries the
// resolved theme id, and is ABSENT for the built-in `kbc` family — so with
// no theme chosen the DOM and every painted colour are byte-identical to
// pre-V70-A7. `system` resolves in JS for a registry family (a matchMedia
// listener), because emitting a second media-scoped block per theme would
// double the generated CSS for one derivable bit.
//
// Preferences stay BROWSER-LOCAL by the D16 ruling: no daemon write, no
// server noun, no CLI verb — exactly like `lastRepo` above it.
import { KBC_DEFAULT_PRESET, KBC_PRESETS, type KbcPreset } from "../commands/registry.gen";
import {
  CONTRAST_MAX,
  CONTRAST_MIN,
  CONTRAST_STEP_L,
  applyTheme as applySharedTheme,
  coerceFamily,
  resolveThemeId as resolveSharedThemeId,
  setThemeAttributes,
  applyContrast,
  applyThemeColorMeta,
} from "@kb/ui/themes/apply";
import { COMMENT_GUTTER_MODES, type CommentGutterMode } from "./comments";

export type Theme = "dark" | "light" | "system";

/// The kbc-theme/1 axis name for the same three values. `Theme` is kept as
/// the exported alias so no existing caller has to change.
export type Appearance = Theme;

/// The built-in family: `applyTheme` never sets `data-kbc-theme` for it, so
/// tokens.css alone paints — the zero-diff default.
export const BUILTIN_FAMILY = "kbc";

/// Reading-contract density (D16). `comfortable` reproduces today's spacing
/// exactly; `compact` tightens the two density tokens and nothing else.
export type Density = "comfortable" | "compact";

/// The contrast slider's range. 0 is the theme as authored; each step
/// applies a fixed OKLCH LIGHTNESS delta to the five background roles and
/// leaves every foreground alone, so the direction of the change is always
/// "surfaces move, ink stays".
export { CONTRAST_MAX, CONTRAST_MIN, CONTRAST_STEP_L };

/// V4.D1 — unified (default) or side-by-side hunk layout.
export type DiffMode = "unified" | "split";

export interface Prefs {
  // The last repo name the operator was viewing. Browser-local only,
  // written on every active-repo change and read ONCE on a cold bare-"/"
  // entry (see `coldSeedRepo` below).
  lastRepo?: string | null;
  /// V4.U2 — color scheme (default `"dark"`). `"system"` follows the OS
  /// via tokens.css `@media (prefers-color-scheme: light)`.
  theme?: Theme;
  /// V4.D1 — last-chosen diff layout (default `"unified"`).
  diffMode?: DiffMode;
  /// V4.D2 — syntax-highlight painted diff lines (default ON).
  diffSyntaxHighlight?: boolean;
  /// V3.N2 — sticky context lines above the editor (default ON).
  stickyContext?: boolean;
  /// V72-I2 — fold the annotaterb `# == Schema Information` banner in the
  /// buffer. Default ON (opt-OUT), because the banner is generated noise
  /// above the class a reader opened the file for; the rail's Schema card
  /// carries the same facts in a form that fits.
  schemaFold?: boolean;
  /// V3.1-H3a — param-name inlay hints at call sites (default ON).
  paramHints?: boolean;
  /// V3.1-H3b — Code Vision lens chips above declarations (default ON).
  codeLenses?: boolean;
  /// V3.2-B3 — FileTree hotspot attention overlay (default OFF).
  attentionOverlay?: boolean;
  /// OL PRES3 - FileTree presence marks: rows light when the live mirror
  /// names them and carry kb's attribution. Default ON (it renders nothing
  /// until a file changes, and polls only while the tab is visible).
  presenceOverlay?: boolean;
  /// V70-A6 — the provisional-pane gradient (§P7). **Default OFF**, which is
  /// the design's own ruling, not a shipping compromise: VS Code's preview
  /// editors are the measured failure the research cites (right mechanism,
  /// too quiet a signal), so kb-code ships the mechanism behind a preference
  /// and only ever applies it to ONE gesture — promoting an inline peek into
  /// a pane. With this off, that promotion produces an ordinary pinned pane.
  provisionalPanes?: boolean;
  /// SH.C3 — CM6 reader line-wrap (default OFF; unwrapped is the long-
  /// standing desktop-reading default, wrap is the "code on a phone" fix).
  wrap?: boolean;
  /// SH.C3 — CM6 reader buffer font-size in px, clamped to
  /// `[CODE_FONT_SIZE_MIN, CODE_FONT_SIZE_MAX]` (default `13`, the
  /// pre-SH.C3 hardcoded value). V80-R2 narrowed the range to exactly
  /// `{13, 14, 15}` — off the same reading-scale ramp `--fs-*` sits on,
  /// never back below the `--fs-body` floor — and renamed the field from
  /// `readerFontSize` (the stepper is code-only; "reader" also names the
  /// whole SPA). An existing `readerFontSize` blob is simply unread under
  /// the new key, same as any other stale browser-local pref (D16).
  codeFontSize?: number;
  /// V70-A7 — the kbc-theme/1 family id (default `"kbc"`, the built-in).
  themeFamily?: string;
  /// OL S1 - set once the user picks a family in kb-code's own picker. Until
  /// then kb-code follows kb's family (`lib/kbTheme.ts`); a local choice
  /// always wins and is never written back to kb.
  themeFamilyChosen?: boolean;
  /// V70-A7 — contrast slider, clamped to [-2, 2] (default 0 = untouched).
  contrast?: number;
  /// V70-A7 — reading-contract density (default `"comfortable"`).
  density?: Density;
  /// V70-A5 — the kbc-cmd/1 key preset (default `"vim"`). Browser-local by
  /// the same D16 ruling the theme follows: a keymap is a body memory, not
  /// a property of the corpus, so it never becomes a daemon write.
  keyPreset?: KbcPreset;
  /// V70-A5 — learn-mode counters. `coachGd` is the one-time `gd` coach-mark
  /// for the commitment Ramp; `learnToasts` caps the "that button has a key"
  /// toast so it teaches once per action rather than nagging. Both are
  /// COUNTERS, browser-local, and are never read by anything that ranks —
  /// design D24's "local-first, never feeds ranking".
  coachGd?: number;
  learnToasts?: Record<string, number>;
  /// V72-J2 (D8) — the comments/1 gutter's display filter (default `"all"`).
  /// Browser-local by the same D16 ruling every other reading-mode toggle
  /// here follows: comments/1 itself has no display-mode concept, this is
  /// purely how the margin reads for THIS operator.
  commentGutterMode?: CommentGutterMode;
  /// V76-R3a — coverage band on the blame gutter (default off).
  coverageBand?: boolean;
  /// V80-R1 — the `/search` page's collapsible preview pane. `undefined`
  /// (no stored choice yet) lets the page pick a WIDTH-based default
  /// (`loadSearchPreviewOpen`'s own viewport-width param: open ≥1280px,
  /// closed narrower) rather than a fixed boolean — the first explicit
  /// toggle (`Alt-p` / the toolbar's "preview" button) pins it for every
  /// width thereafter, same "an unset pref degrades to a sane default,
  /// never a silent choice" posture as every other reading-mode toggle
  /// above.
  searchPreviewOpen?: boolean;
}

const PREFS_KEY = "kbc:prefs";

/// SH.C3 — code font-size stepper bounds. `DEFAULT` matches the historical
/// hardcoded `CodeView` theme size so an unset pref renders byte-identical
/// to before this unit. V80-R2 narrowed `MIN`/`MAX` from `[11, 18]` to
/// `[13, 15]` — three steps, never below the reading-contract's own body
/// floor (`--fs-body`/`--fs-sm`, root CLAUDE.md's ramp) — so A− now floors
/// at exactly the same 13px every OTHER "row" text in the reader already
/// floors at, rather than shrinking the one thing the reader exists to
/// show below that line. `DEFAULT` staying `13` keeps
/// `e2e/desk-viewport.spec.ts`'s column/row floor (computed from this same
/// font) unchanged.
export const CODE_FONT_SIZE_MIN = 13;
export const CODE_FONT_SIZE_MAX = 15;
export const CODE_FONT_SIZE_DEFAULT = 13;

const DEFAULT_PREFS: Prefs = {
  lastRepo: null,
  theme: "dark",
  diffMode: "unified",
  diffSyntaxHighlight: true,
  stickyContext: true,
  schemaFold: true,
  paramHints: true,
  codeLenses: true,
  attentionOverlay: false,
  presenceOverlay: true,
  provisionalPanes: false,
  wrap: false,
  codeFontSize: CODE_FONT_SIZE_DEFAULT,
  themeFamily: BUILTIN_FAMILY,
  contrast: 0,
  density: "comfortable",
  keyPreset: KBC_DEFAULT_PRESET,
  coachGd: 0,
  learnToasts: {},
  commentGutterMode: "all",
  coverageBand: false,
};

export function loadPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      return { ...DEFAULT_PREFS, ...parsed };
    }
  } catch {
    // fallthrough — corrupt/denied localStorage degrades to defaults.
  }
  return DEFAULT_PREFS;
}

export function savePrefs(prefs: Prefs): void {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    // best-effort — localStorage may be unavailable (private browsing quota,
    // disabled storage); the pref just doesn't survive a reload.
  }
}

// Last-repo persistence. `saveLastRepo` is a no-op when unchanged so the
// per-active-repo-change writer doesn't thrash localStorage.
export function loadLastRepo(): string | null {
  return loadPrefs().lastRepo ?? null;
}

export function saveLastRepo(repo: string): void {
  const cur = loadPrefs();
  if (cur.lastRepo === repo) return;
  savePrefs({ ...cur, lastRepo: repo });
}

/// V4.U2 — coerce a stored / missing theme to the closed Theme set.
export function loadTheme(): Theme {
  const t = loadPrefs().theme;
  return t === "light" || t === "system" ? t : "dark";
}

/// V4.D1 — coerce a stored / missing layout to the closed DiffMode set.
export function loadDiffMode(): DiffMode {
  return loadPrefs().diffMode === "split" ? "split" : "unified";
}

export function saveDiffMode(mode: DiffMode): void {
  const cur = loadPrefs();
  if (cur.diffMode === mode) return;
  savePrefs({ ...cur, diffMode: mode });
}

/// V4.D2 — coerce a stored / missing flag; default ON when unset.
export function loadDiffSyntaxHighlight(): boolean {
  const v = loadPrefs().diffSyntaxHighlight;
  return v !== false;
}

export function saveDiffSyntaxHighlight(on: boolean): void {
  const cur = loadPrefs();
  if (cur.diffSyntaxHighlight === on) return;
  savePrefs({ ...cur, diffSyntaxHighlight: on });
}

/// V70-A7 — coerce a stored / missing family to one the registry actually
/// carries. An unknown id (a downgrade, a hand-edited blob) degrades to the
/// built-in rather than leaving the app on an attribute nothing styles.
export function loadThemeFamily(): string {
  if (!hasLocalThemeFamily() && followedFamily) return followedFamily;
  return coerceFamily(loadPrefs().themeFamily, BUILTIN_FAMILY, HOST);
}

/// OL S1 - did the user choose a family in kb-code itself? True once the
/// picker committed one, or when an older blob already holds a non-built-in
/// family (a choice made before this flag existed).
export function hasLocalThemeFamily(): boolean {
  const p = loadPrefs();
  return (
    p.themeFamilyChosen === true ||
    (typeof p.themeFamily === "string" && p.themeFamily !== BUILTIN_FAMILY)
  );
}

/// kb's family, as relayed by `GET /api/kb/ui` (`lib/kbTheme.ts`). In-memory
/// ONLY: it is never written into the prefs blob, so kb-code stores nothing
/// about kb's choice and a stale value cannot outlive the page.
let followedFamily: string | null = null;

/// Record the family to follow, or clear it (`null`). An id the registry does
/// not carry is ignored (kb may be newer than this bundle).
export function setFollowedThemeFamily(family: string | null): void {
  followedFamily =
    family && coerceFamily(family, "", HOST) === family ? family : null;
}

/// Where the family in force comes from: the user's own pick, kb's, or the
/// built-in default (no pick and nothing to follow).
export type ThemeFamilySource = "local" | "kb" | "default";

export function themeFamilySource(): ThemeFamilySource {
  if (hasLocalThemeFamily()) return "local";
  return followedFamily ? "kb" : "default";
}

export function loadContrast(): number {
  const v = loadPrefs().contrast;
  if (typeof v !== "number" || !Number.isFinite(v)) return 0;
  return Math.max(CONTRAST_MIN, Math.min(CONTRAST_MAX, Math.round(v)));
}

export function loadDensity(): Density {
  return loadPrefs().density === "compact" ? "compact" : "comfortable";
}

/// The shared engine's host knob: kb-code paints its built-in `kbc` family
/// from its own tokens.css, so that family resolves to NO `data-kbc-theme`.
const HOST = { builtinFamily: BUILTIN_FAMILY } as const;

/// Resolve (family, appearance) to a `data-kbc-theme` value, or `null` for
/// "leave the attribute off and let tokens.css paint". Pure except for the
/// injected `light` flag; the resolution itself is the shared engine's.
export function resolveThemeId(
  family: string,
  appearance: Appearance,
  light: boolean,
): string | null {
  return resolveSharedThemeId(family, appearance, light, HOST);
}

/// Sets `<html data-theme>` (appearance), `<html data-kbc-theme>` (the
/// resolved registry theme, absent for the built-in family),
/// `<html data-density>`, `color-scheme`, the contrast deltas and the
/// `theme-color` meta. Re-running is cheap and idempotent. The attribute and
/// contrast mechanics live in `@kb/ui/themes/apply`; density is kb-code's own.
export function applyTheme(theme: Theme, family: string = loadThemeFamily()): void {
  const root = document.documentElement;
  // `comfortable` is tokens.css's `:root` default, so the ATTRIBUTE is only
  // set for `compact`. That keeps a fresh browser's DOM byte-identical to
  // pre-V70-A7 — the reading contract adds a lever, not a marker.
  const density = loadDensity();
  if (density === "compact") root.setAttribute("data-density", density);
  else root.removeAttribute("data-density");

  applySharedTheme(theme, family, { ...HOST, contrast: loadContrast() });
}

/// V70-A7 — persist + apply an appearance/family/contrast/density change and
/// notify every control. ONE writer, so the two theme controls (TopBar and
/// the mobile NavSheet) and the picker can never disagree.
export function setThemePrefs(next: {
  theme?: Theme;
  themeFamily?: string;
  contrast?: number;
  density?: Density;
}): void {
  const cur = loadPrefs();
  // OL S1 - picking a family here is the moment kb-code stops following kb.
  savePrefs({
    ...cur,
    ...next,
    ...(next.themeFamily !== undefined ? { themeFamilyChosen: true } : {}),
  });
  applyTheme(next.theme ?? loadTheme(), next.themeFamily ?? loadThemeFamily());
  window.dispatchEvent(
    new CustomEvent<Theme>("kbc:theme.changed", { detail: loadTheme() }),
  );
}

/// PREVIEW — flip the attributes WITHOUT persisting, so the picker's arrow
/// keys can show a real theme instantly and Esc can revert. This is the whole
/// reason the theme is one attribute and not a stylesheet swap: previewing is
/// free. `applyTheme()` (from the live prefs) is the revert.
export function previewTheme(family: string, appearance: Appearance): void {
  setThemeAttributes(appearance, family, HOST);
  applyContrast(loadContrast());
  applyThemeColorMeta();
}

// v0.10 D2 / V4.U2 — the mobile NavSheet row still cycles the APPEARANCE
// axis (dark → light → system); the family is chosen in the picker.
const THEME_CYCLE: Theme[] = ["dark", "light", "system"];

export function cycleTheme(): Theme {
  const next = THEME_CYCLE[(THEME_CYCLE.indexOf(loadTheme()) + 1) % THEME_CYCLE.length];
  setThemePrefs({ theme: next });
  return next;
}

/// V3.N2 — sticky context preference (default true when unset).
export function loadStickyContext(): boolean {
  const v = loadPrefs().stickyContext;
  return v !== false;
}

export function saveStickyContext(on: boolean): void {
  const cur = loadPrefs();
  if (cur.stickyContext === on) return;
  savePrefs({ ...cur, stickyContext: on });
}

/// V72-I2 — the annotaterb schema fold (default true when unset — an
/// opt-OUT, the same shape `stickyContext`/`paramHints`/`codeLenses` use).
export function loadSchemaFold(): boolean {
  const v = loadPrefs().schemaFold;
  return v !== false;
}

export function saveSchemaFold(on: boolean): void {
  const cur = loadPrefs();
  if (cur.schemaFold === on) return;
  savePrefs({ ...cur, schemaFold: on });
}

/// V3.1-H3a — param-name inlay hints (default true when unset).
export function loadParamHints(): boolean {
  const v = loadPrefs().paramHints;
  return v !== false;
}

export function saveParamHints(on: boolean): void {
  const cur = loadPrefs();
  if (cur.paramHints === on) return;
  savePrefs({ ...cur, paramHints: on });
}

/// V3.1-H3b — Code Vision lenses (default true when unset).
export function loadCodeLenses(): boolean {
  const v = loadPrefs().codeLenses;
  return v !== false;
}

export function saveCodeLenses(on: boolean): void {
  const cur = loadPrefs();
  if (cur.codeLenses === on) return;
  savePrefs({ ...cur, codeLenses: on });
}

/// V3.2-B3 — FileTree attention overlay (default false when unset).
export function loadAttentionOverlay(): boolean {
  return loadPrefs().attentionOverlay === true;
}

export function saveAttentionOverlay(on: boolean): void {
  const cur = loadPrefs();
  if (cur.attentionOverlay === on) return;
  savePrefs({ ...cur, attentionOverlay: on });
}

/// OL PRES3 - FileTree presence marks (default TRUE when unset: only an
/// explicit `false` turns them off). Broadcasts `PRESENCE_PREF_EVENT` so the
/// tree toggle, Home and the Spine chip stay in step within one tab.
export const PRESENCE_PREF_EVENT = "kbc:presence-pref";

export function loadPresenceOverlay(): boolean {
  return loadPrefs().presenceOverlay !== false;
}

export function savePresenceOverlay(on: boolean): void {
  const cur = loadPrefs();
  if (cur.presenceOverlay !== on) savePrefs({ ...cur, presenceOverlay: on });
  if (typeof window !== "undefined") window.dispatchEvent(new Event(PRESENCE_PREF_EVENT));
}

/// V70-A6 — provisional panes (default false when unset; see the field doc).
export function loadProvisionalPanes(): boolean {
  return loadPrefs().provisionalPanes === true;
}

export function saveProvisionalPanes(on: boolean): void {
  const cur = loadPrefs();
  if (cur.provisionalPanes === on) return;
  savePrefs({ ...cur, provisionalPanes: on });
}

/// SH.C3 — reader line-wrap (default false when unset).
export function loadWrap(): boolean {
  return loadPrefs().wrap === true;
}

export function saveWrap(on: boolean): void {
  const cur = loadPrefs();
  if (cur.wrap === on) return;
  savePrefs({ ...cur, wrap: on });
}

/// V72-J2 — the comments/1 gutter's display mode (default `"all"` when
/// unset, or when a corrupt/unrecognized value is stored — same "degrade to
/// the honest default rather than throw" posture `normalizeInspectorTab`
/// uses).
export function loadCommentGutterMode(): CommentGutterMode {
  const v = loadPrefs().commentGutterMode;
  return v && (COMMENT_GUTTER_MODES as readonly string[]).includes(v) ? v : "all";
}

export function saveCommentGutterMode(mode: CommentGutterMode): void {
  const cur = loadPrefs();
  if (cur.commentGutterMode === mode) return;
  savePrefs({ ...cur, commentGutterMode: mode });
}

/// V76-R3a — coverage band on the blame gutter. Default OFF.
export function loadCoverageBand(): boolean {
  return loadPrefs().coverageBand === true;
}

export function saveCoverageBand(on: boolean): void {
  const cur = loadPrefs();
  if (cur.coverageBand === on) return;
  savePrefs({ ...cur, coverageBand: on });
}

/// V80-R1 — the search results page's preview pane. No stored choice yet ⇒
/// derive the default from the CALLER's own viewport width (an injected
/// param, not a `window` read here, so this stays testable without a DOM) —
/// open at ≥1280px, closed narrower. Once the operator has toggled it
/// explicitly, that choice wins at every width.
export const SEARCH_PREVIEW_DEFAULT_MIN_WIDTH = 1280;

export function loadSearchPreviewOpen(viewportWidth: number): boolean {
  const v = loadPrefs().searchPreviewOpen;
  if (typeof v === "boolean") return v;
  return viewportWidth >= SEARCH_PREVIEW_DEFAULT_MIN_WIDTH;
}

export function saveSearchPreviewOpen(open: boolean): void {
  const cur = loadPrefs();
  if (cur.searchPreviewOpen === open) return;
  savePrefs({ ...cur, searchPreviewOpen: open });
}

/// SH.C3/V80-R2 — clamp to the code font-size stepper's bounds; a
/// non-finite input (corrupt localStorage, a stray `NaN` from an
/// empty-string parse) degrades to the default rather than propagating
/// into CM6's theme.
export function clampCodeFontSize(px: number): number {
  if (!Number.isFinite(px)) return CODE_FONT_SIZE_DEFAULT;
  return Math.min(CODE_FONT_SIZE_MAX, Math.max(CODE_FONT_SIZE_MIN, Math.round(px)));
}

/// SH.C3/V80-R2 — coerce a stored / missing font size to the clamped range.
export function loadCodeFontSize(): number {
  return clampCodeFontSize(loadPrefs().codeFontSize ?? CODE_FONT_SIZE_DEFAULT);
}

/// Returns the clamped value actually persisted, so a caller driving a
/// stepper can seed its next state from the return rather than re-reading.
export function saveCodeFontSize(px: number): number {
  const clamped = clampCodeFontSize(px);
  const cur = loadPrefs();
  if (cur.codeFontSize !== clamped) savePrefs({ ...cur, codeFontSize: clamped });
  return clamped;
}

// ── V70-A5: the key preset + learn-mode counters ──────────────────────────

/// Coerce a stored / missing preset to one the registry actually ships. A
/// preset dropped from `registry.json` degrades to the default rather than
/// leaving the operator with a keyboard that resolves nothing.
export function loadKeyPreset(): KbcPreset {
  const raw = loadPrefs().keyPreset;
  return KBC_PRESETS.some((p) => p.id === raw) ? (raw as KbcPreset) : KBC_DEFAULT_PRESET;
}

export function saveKeyPreset(preset: KbcPreset): void {
  const cur = loadPrefs();
  if (cur.keyPreset === preset) return;
  savePrefs({ ...cur, keyPreset: preset });
}

/// D24 — the one-time `gd` coach-mark for the commitment Ramp. Returns true
/// exactly once per browser profile; the increment is written on the same
/// call, so a double-render cannot show it twice.
export function claimGdCoachMark(): boolean {
  const cur = loadPrefs();
  if ((cur.coachGd ?? 0) > 0) return false;
  savePrefs({ ...cur, coachGd: 1 });
  return true;
}

/// D24 — learn mode: the first mouse click on a control that HAS a key earns
/// one toast naming the key. Once per command id, never a running commentary
/// — a teaching aid that keeps teaching after you have learned is just noise.
export function claimLearnToast(commandId: string): boolean {
  const cur = loadPrefs();
  const seen = cur.learnToasts ?? {};
  if ((seen[commandId] ?? 0) > 0) return false;
  savePrefs({ ...cur, learnToasts: { ...seen, [commandId]: 1 } });
  return true;
}

/// Test/debug helper — clears the two learn-mode counters so a fresh-profile
/// script (`e2e/day-one.spec.ts`) can assert the first-run experience without
/// clearing the whole prefs blob (which would also reset the theme the same
/// script is checking).
export function resetLearnState(): void {
  const cur = loadPrefs();
  savePrefs({ ...cur, coachGd: 0, learnToasts: {} });
}

// Decide whether a cold app entry should restore the last-browsed repo, and
// to which one. Returns the repo to seed (`navigate(readerUrl(it, ""))`), or
// null to leave the URL untouched. Pure so the gating — the bug-prone part —
// is unit-tested without a router, same discipline as kb's own `coldSeedKb`.
export function coldSeedRepo(opts: {
  repos: string[]; // live repo names, in configured order
  explicitRepo: string | null; // path/?repo= selection, if any
  pathname: string;
  lastRepo: string | null;
}): string | null {
  const { repos, explicitRepo, pathname, lastRepo } = opts;
  if (repos.length === 0) return null; // repo list not loaded yet
  if (explicitRepo) return null; // the user already has an explicit selection
  if (pathname !== "/") return null; // only a bare "/" cold entry seeds
  if (!lastRepo) return null; // nothing remembered
  if (lastRepo === repos[0]) return null; // already the repos[0] default → no-op
  if (!repos.includes(lastRepo)) return null; // ghost-repo guard
  return lastRepo;
}
