// kbc-theme/1 — the DOM half of the theme engine, shared by both SPAs.
//
// `derive.ts` is pure math and `registry.gen.ts` / `themes.gen.css` are its
// checked-in output; this module is the one place that turns a (family,
// appearance) choice into the attributes that CSS selects on:
//
//   <html data-theme="dark|light|system">   the APPEARANCE axis (structural
//                                           dark/light bit CM6 and legacy
//                                           CSS key on)
//   <html data-kbc-theme="<theme id>">      the resolved registry theme the
//                                           generated CSS selects on
//   <html style="color-scheme: …">          native widgets follow the choice
//                                           (the CSS roles follow the
//                                           data-theme selectors instead)
//
// Storage is NOT here: each app owns where its preference lives (kb-code's
// `lib/prefs.ts`, kb's `api/prefs.ts`, different keys, different blobs) and
// calls in with plain values. Everything below is idempotent, so re-running
// it from a prefs change, a preview or an OS scheme flip is always safe.
//
// `builtinFamily` is the one host-specific knob. kb-code ships its built-in
// `kbc` palette in its own tokens.css and wants NO `data-kbc-theme` for it
// (a fresh browser's DOM stays byte-identical to before themes existed); kb
// has no hand-written palette any more, so it passes `null` and every family,
// `kb-classic` included, resolves to an attribute.
import { KBC_THEMES } from "./registry.gen";
import { fromOklch, parseHex, toHex, toOklch } from "./derive";

export type ThemeAppearance = "light" | "dark" | "system";

export const CONTRAST_MIN = -2;
export const CONTRAST_MAX = 2;
/// One contrast step is this much OKLCH lightness on the background roles.
export const CONTRAST_STEP_L = 0.018;

export interface ThemeHostOptions {
  /// A family the host paints from its own CSS: resolving it yields no
  /// `data-kbc-theme`. `null` / omitted = every family is attribute-driven.
  readonly builtinFamily?: string | null;
}

/// Does the OS currently ask for light? Only consulted when the appearance
/// is `system` and the family is attribute-driven.
export function prefersLight(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-color-scheme: light)").matches
    : false;
}

/// Coerce a stored / missing family to one the registry carries. An unknown
/// id (a downgrade, a hand-edited blob) degrades to `fallback` rather than
/// leaving the app on an attribute nothing styles.
export function coerceFamily(
  family: string | null | undefined,
  fallback: string,
  host: ThemeHostOptions = {},
): string {
  if (!family) return fallback;
  if (host.builtinFamily != null && family === host.builtinFamily) return family;
  return KBC_THEMES.some((t) => t.family === family) ? family : fallback;
}

/// Resolve (family, appearance) to a `data-kbc-theme` value, or `null` for
/// "leave the attribute off and let the host's own CSS paint". PURE except
/// for the injected `light` flag, so it is unit-testable without a DOM.
export function resolveThemeId(
  family: string,
  appearance: ThemeAppearance,
  light: boolean,
  host: ThemeHostOptions = {},
): string | null {
  if (host.builtinFamily != null && family === host.builtinFamily) return null;
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
/// paints, and so the value is the same one `theme lint` would measure.
export function applyContrast(delta: number): void {
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

/// Keep the `theme-color` meta on the theme the user actually chose.
/// `index.html` may key two of these to `prefers-color-scheme`, so picking
/// light on a dark OS would leave the mobile browser chrome dark. We resolve
/// the live `--bg` and collapse them to one unconditional meta.
export function applyThemeColorMeta(): void {
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

/// Set the three attributes WITHOUT touching contrast, the theme-color meta
/// or the OS listener: the cheap half, used directly by picker previews.
export function setThemeAttributes(
  appearance: ThemeAppearance,
  family: string,
  host: ThemeHostOptions = {},
): void {
  const root = document.documentElement;
  root.setAttribute("data-theme", appearance);
  root.style.colorScheme = appearance === "system" ? "light dark" : appearance;
  const id = resolveThemeId(family, appearance, prefersLight(), host);
  if (id) root.setAttribute("data-kbc-theme", id);
  else root.removeAttribute("data-kbc-theme");
}

export interface ApplyThemeOptions extends ThemeHostOptions {
  /// Contrast delta in [CONTRAST_MIN, CONTRAST_MAX]; 0 / omitted = none.
  readonly contrast?: number;
}

let osSchemeListener: (() => void) | null = null;

/// Full apply: attributes, contrast, the `theme-color` meta, and (for
/// `system` with an attribute-driven family) an OS-flip listener, because
/// such a family is resolved in JS rather than by a media query. One listener
/// at a time; re-registered on every apply.
export function applyTheme(
  appearance: ThemeAppearance,
  family: string,
  opts: ApplyThemeOptions = {},
): void {
  setThemeAttributes(appearance, family, opts);

  const mq =
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia("(prefers-color-scheme: light)")
      : null;
  if (mq && osSchemeListener) mq.removeEventListener("change", osSchemeListener);
  osSchemeListener = null;
  const attributeDriven = !(opts.builtinFamily != null && family === opts.builtinFamily);
  if (mq && appearance === "system" && attributeDriven) {
    osSchemeListener = () => applyTheme(appearance, family, opts);
    mq.addEventListener("change", osSchemeListener);
  }

  applyContrast(opts.contrast ?? 0);
  applyThemeColorMeta();
}
