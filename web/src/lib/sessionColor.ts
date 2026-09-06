// v0.14 S5/S7 — deterministic per-session color for the /sessions
// view chips + the atlas overlay polylines. Same session_id always
// yields the same hue across reloads + views, so the user can
// recognise a session by its color across the SPA without us tracking
// a shared cache.
//
// FNV-1a 32-bit hash → HSL hue, fixed saturation + lightness chosen
// to stay readable against the dark theme's panel + still be visible
// against the atlas dot palette.

const HSL_SAT = 65;
const HSL_LIT = 60;

// v0.14 S8 — module-local memoisation. The atlas overlay re-renders
// the polyline color string on every draw frame; the /sessions row
// chips do the same on every list update. Hashing is cheap but the
// HSL string allocation adds up; a single `Map<sid, string>` shared
// across views drops the cost to a single lookup after first paint.
const colorCache = new Map<string, string>();

function fnv1a32(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i += 1) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  // Treat as unsigned 32-bit.
  return h >>> 0;
}

export function sessionColorFor(sessionId: string): string {
  const cached = colorCache.get(sessionId);
  if (cached) return cached;
  const hue = fnv1a32(sessionId) % 360;
  const out = `hsl(${hue}deg ${HSL_SAT}% ${HSL_LIT}%)`;
  colorCache.set(sessionId, out);
  return out;
}
