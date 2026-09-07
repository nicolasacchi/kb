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
import { KBC_THEMES } from "../themes/registry.gen";
import { fromOklch, parseHex, toHex, toOklch } from "../themes/derive";
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
export const CONTRAST_MIN = -2;
export const CONTRAST_MAX = 2;
export const CONTRAST_STEP_L = 0.018;

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
  /// V3.1-H3a — param-name inlay hints at call sites (default ON).
  paramHints?: boolean;
  /// V3.1-H3b — Code Vision lens chips above declarations (default ON).
  codeLenses?: boolean;
  /// V3.2-B3 — FileTree hotspot attention overlay (default OFF).
  attentionOverlay?: boolean;
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
  /// `[READER_FONT_SIZE_MIN, READER_FONT_SIZE_MAX]` (default `13`, the
  /// pre-SH.C3 hardcoded value).
  readerFontSize?: number;
  /// V70-A7 — the kbc-theme/1 family id (default `"kbc"`, the built-in).
  themeFamily?: string;
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
}

const PREFS_KEY = "kbc:prefs";

/// SH.C3 — reader font-size stepper bounds. `DEFAULT` matches the historical
/// hardcoded `CodeView` theme size so an unset pref renders byte-identical
/// to before this unit.
export const READER_FONT_SIZE_MIN = 11;
export const READER_FONT_SIZE_MAX = 18;
export const READER_FONT_SIZE_DEFAULT = 13;

const DEFAULT_PREFS: Prefs = {
  lastRepo: null,
  theme: "dark",
  diffMode: "unified",
  diffSyntaxHighlight: true,
  stickyContext: true,
  paramHints: true,
  codeLenses: true,
  attentionOverlay: false,
  provisionalPanes: false,
  wrap: false,
  readerFontSize: READER_FONT_SIZE_DEFAULT,
  themeFamily: BUILTIN_FAMILY,
  contrast: 0,
  density: "comfortable",
  keyPreset: KBC_DEFAULT_PRESET,
  coachGd: 0,
  learnToasts: {},
  commentGutterMode: "all",
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
  const f = loadPrefs().themeFamily;
  if (!f || f === BUILTIN_FAMILY) return BUILTIN_FAMILY;
  return KBC_THEMES.some((t) => t.family === f) ? f : BUILTIN_FAMILY;
}

export function loadContrast(): number {
  const v = loadPrefs().contrast;
  if (typeof v !== "number" || !Number.isFinite(v)) return 0;
  return Math.max(CONTRAST_MIN, Math.min(CONTRAST_MAX, Math.round(v)));
}

export function loadDensity(): Density {
  return loadPrefs().density === "compact" ? "compact" : "comfortable";
}

/// Does the OS currently ask for light? Only consulted when `appearance` is
/// `system` AND a registry family is active — the built-in family lets
/// tokens.css's own `prefers-color-scheme` block answer, unchanged.
function prefersLight(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-color-scheme: light)").matches
    : false;
}

/// Resolve (family, appearance) to a `data-kbc-theme` value, or `null` for
/// "leave the attribute off and let tokens.css paint". PURE except for the
/// injected `light` flag, so it is unit-testable without a DOM.
export function resolveThemeId(
  family: string,
  appearance: Appearance,
  light: boolean,
): string | null {
  if (family === BUILTIN_FAMILY) return null;
  const members = KBC_THEMES.filter((t) => t.family === family);
  if (members.length === 0) return null;
  const want = appearance === "system" ? (light ? "light" : "dark") : appearance;
  return (members.find((t) => t.appearance === want) ?? members[0]).id;
}

const CONTRAST_BG_ROLES = ["bg", "bg-panel", "bg-panel-2", "bg-card", "bg-card-hi"];

/// Apply the contrast slider as inline OKLCH lightness deltas on the five
/// background roles. The inline properties are REMOVED first so the read
/// back sees the theme's own value, never a previous delta compounding on
/// itself. The arithmetic is done in JS (not `oklch(from …)` in CSS) so the
/// result is a plain hex a browser without relative-colour support still
/// paints — and so the value is the same one `theme lint` would measure.
function applyContrast(delta: number): void {
  const root = document.documentElement;
  for (const role of CONTRAST_BG_ROLES) root.style.removeProperty(`--${role}`);
  if (!delta) return;
  const cs = getComputedStyle(root);
  for (const role of CONTRAST_BG_ROLES) {
    const raw = cs.getPropertyValue(`--${role}`).trim();
    if (!/^#[0-9a-fA-F]{6}$/.test(raw)) continue;
    const lch = toOklch(parseHex(raw));
    // Positive contrast pushes surfaces AWAY from the ink: darker on a dark
    // theme, lighter on a light one. `lch.l < 0.5` is the cheap, stable test
    // for which side of the ramp this palette sits on.
    const dir = lch.l < 0.5 ? -1 : 1;
    const l = Math.max(0, Math.min(1, lch.l + dir * delta * CONTRAST_STEP_L));
    root.style.setProperty(`--${role}`, toHex(fromOklch({ l, c: lch.c, h: lch.h })));
  }
}

/// Keep the `theme-color` meta on the theme the user actually chose. R11:
/// `index.html` keys two of these to `prefers-color-scheme`, so picking
/// light on a dark OS left the mobile browser chrome dark. We resolve the
/// live `--bg` and collapse them to one unconditional meta.
function applyThemeColorMeta(): void {
  const bg = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim();
  if (!bg) return;
  const head = document.head;
  if (!head) return;
  const metas = Array.from(head.querySelectorAll('meta[name="theme-color"]'));
  for (const m of metas.slice(1)) m.remove();
  const meta = (metas[0] as HTMLMetaElement | undefined) ?? document.createElement("meta");
  meta.setAttribute("name", "theme-color");
  meta.removeAttribute("media");
  meta.setAttribute("content", bg);
  if (!meta.parentNode) head.appendChild(meta);
}

let osSchemeListener: ((e: MediaQueryListEvent) => void) | null = null;

/// Sets `<html data-theme>` (appearance), `<html data-kbc-theme>` (the
/// resolved registry theme, absent for the built-in family),
/// `<html data-density>`, `color-scheme`, the contrast deltas and the
/// `theme-color` meta. Re-running is cheap and idempotent.
export function applyTheme(theme: Theme, family: string = loadThemeFamily()): void {
  const root = document.documentElement;
  root.setAttribute("data-theme", theme);
  root.style.colorScheme = theme === "system" ? "light dark" : theme;

  const id = resolveThemeId(family, theme, prefersLight());
  if (id) root.setAttribute("data-kbc-theme", id);
  else root.removeAttribute("data-kbc-theme");

  // `comfortable` is tokens.css's `:root` default, so the ATTRIBUTE is only
  // set for `compact`. That keeps a fresh browser's DOM byte-identical to
  // pre-V70-A7 — the reading contract adds a lever, not a marker.
  const density = loadDensity();
  if (density === "compact") root.setAttribute("data-density", density);
  else root.removeAttribute("data-density");

  // A registry family resolves `system` in JS, so it needs the OS flip that
  // tokens.css's media block gives the built-in family for free. One
  // listener, re-registered on every apply (the closure captures `family`).
  const mq =
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia("(prefers-color-scheme: light)")
      : null;
  if (mq && osSchemeListener) mq.removeEventListener("change", osSchemeListener);
  osSchemeListener = null;
  if (mq && theme === "system" && family !== BUILTIN_FAMILY) {
    osSchemeListener = () => applyTheme(theme, family);
    mq.addEventListener("change", osSchemeListener);
  }

  applyContrast(loadContrast());
  applyThemeColorMeta();
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
  savePrefs({ ...cur, ...next });
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
  const root = document.documentElement;
  root.setAttribute("data-theme", appearance);
  root.style.colorScheme = appearance === "system" ? "light dark" : appearance;
  const id = resolveThemeId(family, appearance, prefersLight());
  if (id) root.setAttribute("data-kbc-theme", id);
  else root.removeAttribute("data-kbc-theme");
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

/// SH.C3 — clamp to the reader font-size stepper's bounds; a non-finite
/// input (corrupt localStorage, a stray `NaN` from an empty-string parse)
/// degrades to the default rather than propagating into CM6's theme.
export function clampReaderFontSize(px: number): number {
  if (!Number.isFinite(px)) return READER_FONT_SIZE_DEFAULT;
  return Math.min(READER_FONT_SIZE_MAX, Math.max(READER_FONT_SIZE_MIN, Math.round(px)));
}

/// SH.C3 — coerce a stored / missing font size to the clamped range.
export function loadReaderFontSize(): number {
  return clampReaderFontSize(loadPrefs().readerFontSize ?? READER_FONT_SIZE_DEFAULT);
}

/// Returns the clamped value actually persisted, so a caller driving a
/// stepper can seed its next state from the return rather than re-reading.
export function saveReaderFontSize(px: number): number {
  const clamped = clampReaderFontSize(px);
  const cur = loadPrefs();
  if (cur.readerFontSize !== clamped) savePrefs({ ...cur, readerFontSize: clamped });
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
