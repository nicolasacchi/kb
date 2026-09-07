// V75-M3 — the Lane Budget's provenance channel, wired for the first time.
//
// D16 assigns each semantic lane ONE visual channel, and provenance's is "a
// 2 px session-hue rail". `themes/derive.ts` has emitted eight repaired,
// maximally-spread hues (`--prov-hue-0` … `--prov-hue-7`) since V70-A7 —
// contrast-fixed against every surface background at the NON-TEXT floor,
// because a 2 px rail is a graphical object rather than text — but nothing
// consumed them: they were a token with no reader, exactly as
// `tokens.css`'s own `--age-band-*` note records for its neighbour.
//
// Two rules, both structural:
//
//  1. **The modulus is `PROV_HUES`, imported, never a literal `8`.** The
//     palette size is `derive.ts`'s to choose; a hardcoded 8 here would
//     silently index past the end the day it changes, and CSS would fall
//     back to `var(--line)` for the tail — a rail that quietly stops
//     distinguishing anything.
//  2. **The hue carries NO meaning beyond identity.** It says "these two
//     rows came from the same run", never "this run is better/worse/more
//     trusted" — trust is line STYLE in one hue (the adjacent channel), and
//     mixing the two is how a decorative axis starts reading as a verdict.

import { PROV_HUES } from "../themes/derive";

/// FNV-1a, 32-bit. A stable, dependency-free string hash — the same value
/// in every browser and in vitest, which is what makes the rail a property
/// of the SESSION rather than of when the page happened to render.
export function provHash(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i += 1) {
    h ^= s.charCodeAt(i);
    // `Math.imul` keeps the multiply in 32-bit space; `* 16777619` would
    // lose precision past 2^53 and make the hash platform-dependent.
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/// `identity` → a hue INDEX in `[0, PROV_HUES)`. Total: an empty identity
/// is index 0 rather than a special case, because "no session" is rendered
/// by omitting the rail entirely, not by picking a colour.
export function provHueIndex(identity: string): number {
  return provHash(identity) % PROV_HUES;
}

/// The inline custom property a row sets so the CSS rail can read it.
/// Returned as a plain record (not a `CSSProperties`) so callers spread it
/// with their own cast — React's typings have no slot for a custom prop.
export function provHueStyle(identity: string): Record<string, string> {
  return { "--kbc-prov-hue": `var(--prov-hue-${provHueIndex(identity)})` };
}
