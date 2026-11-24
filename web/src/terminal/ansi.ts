// BANSI — the 16 ANSI colours, derived from the active SPA theme's tokens so a
// programme's `\e[31m` follows the theme (and stays legible on light ones)
// instead of falling back to xterm's built-in dark table.
//
// Pure and uncached on purpose: the contrast slider rewrites `--bg-panel-2`
// inline at runtime, so a per-theme-id cache would go stale.

import type { ITheme } from "@xterm/xterm";
import {
  contrastRatio,
  fromOklch,
  mixOklch,
  parseHex,
  toHex,
  toOklch,
  type Oklch,
  type Rgb,
} from "@kb/ui/themes/derive";

import type { TokenSource } from "./theme";

export type AnsiSlot =
  | "black" | "red" | "green" | "yellow" | "blue" | "magenta" | "cyan" | "white"
  | "brightBlack" | "brightRed" | "brightGreen" | "brightYellow"
  | "brightBlue" | "brightMagenta" | "brightCyan" | "brightWhite";
export type AnsiColors = Required<Pick<ITheme, AnsiSlot>>;

/** Minimum contrast every slot (but `black`) is repaired to. */
export const ANSI_MIN_CONTRAST = 4.5;
const REPAIR_STEP = 0.01;
const REPAIR_MAX_STEPS = 40;

const FALLBACK_HUE = { red: 25, green: 145, blue: 250 } as const;
const DERIVED_HUE = { yellow: 85, cyan: 200, magenta: 320 } as const;
const FALLBACK_C = 0.13;

function tryParse(value: string): Rgb | null {
  try {
    return parseHex(value);
  } catch {
    return null;
  }
}

/** Round-trip through the hex form so contrast is judged on what xterm paints. */
function quantize(o: Oklch): Rgb {
  return parseHex(toHex(fromOklch(o)));
}

/**
 * Walk the OKLCH lightness away from the background (hue and chroma kept)
 * until the colour reaches `ANSI_MIN_CONTRAST`; bounded, deterministic. When
 * the in-hue walk cannot get there, the best colour seen (or the greyscale
 * extreme, if better) is returned.
 */
function repair(c: Oklch, bg: Rgb, light: boolean): Rgb {
  let best = quantize(c);
  let bestRatio = contrastRatio(best, bg);
  if (bestRatio >= ANSI_MIN_CONTRAST) return best;
  const dir = light ? -1 : 1;
  for (let i = 1; i <= REPAIR_MAX_STEPS; i++) {
    const l = Math.max(0, Math.min(1, c.l + dir * REPAIR_STEP * i));
    const cand = quantize({ ...c, l });
    const ratio = contrastRatio(cand, bg);
    if (ratio >= ANSI_MIN_CONTRAST) return cand;
    if (ratio > bestRatio) {
      best = cand;
      bestRatio = ratio;
    }
  }
  const extreme = quantize({ l: light ? 0 : 1, c: 0, h: c.h });
  return contrastRatio(extreme, bg) > bestRatio ? extreme : best;
}

/**
 * The 16 ANSI colours for a terminal painted `bg` on `fg`. red/green/blue come
 * from the theme's `--red/--green/--blue`; yellow/cyan/magenta are derived in
 * OKLCH at fixed hues with the mean lightness and chroma of those three. A
 * missing or non-hex token falls back to a fixed hue. Returns null when `bg`
 * or `fg` is not a `#rrggbb` colour (the caller then leaves xterm's table).
 */
export function deriveAnsi(css: TokenSource, bg: string, fg: string): AnsiColors | null {
  const bgRgb = tryParse(bg.trim());
  const fgRgb = tryParse(fg.trim());
  if (!bgRgb || !fgRgb) return null;
  const light = toOklch(bgRgb).l > 0.6;
  const fbL = light ? 0.5 : 0.72;
  const base = (name: keyof typeof FALLBACK_HUE): Oklch => {
    const rgb = tryParse(css.getPropertyValue(`--${name}`).trim());
    return rgb ? toOklch(rgb) : { l: fbL, c: FALLBACK_C, h: FALLBACK_HUE[name] };
  };
  const red = base("red");
  const green = base("green");
  const blue = base("blue");
  const meanL = (red.l + green.l + blue.l) / 3;
  const meanC = (red.c + green.c + blue.c) / 3;
  const chroma: Record<"red" | "green" | "blue" | "yellow" | "magenta" | "cyan", Oklch> = {
    red,
    green,
    blue,
    yellow: { l: meanL, c: meanC, h: DERIVED_HUE.yellow },
    magenta: { l: meanL, c: meanC, h: DERIVED_HUE.magenta },
    cyan: { l: meanL, c: meanC, h: DERIVED_HUE.cyan },
  };
  const shift = light ? -0.08 : 0.08;
  const hex = (rgb: Rgb): string => toHex(rgb);
  const out: Partial<AnsiColors> = {};
  for (const name of ["red", "green", "yellow", "blue", "magenta", "cyan"] as const) {
    const c = chroma[name];
    out[name] = hex(repair(c, bgRgb, light));
    const cap = (name[0].toUpperCase() + name.slice(1)) as Capitalize<typeof name>;
    out[`bright${cap}`] = hex(repair({ ...c, l: Math.max(0, Math.min(1, c.l + shift)) }, bgRgb, light));
  }
  const grey = (rgb: Rgb): string => hex(repair(toOklch(rgb), bgRgb, light));
  out.black = hex(mixOklch(bgRgb, fgRgb, 0.12));
  out.white = grey(mixOklch(fgRgb, bgRgb, 0.12));
  out.brightBlack = grey(mixOklch(bgRgb, fgRgb, 0.3));
  out.brightWhite = grey(fgRgb);
  return out as AnsiColors;
}
