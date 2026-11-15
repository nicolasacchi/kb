// T14 — the bundled viewer versions, as literals.
//
// `pins.test.ts` compares these against `web/package.json` dependency
// strings (read via the `getBuiltinModule` fs pattern) and against the
// Profile golden (`crates/kb-term/tests/golden/sframe_profile.json`).
// A Profile whose `xterm`/`width` differs from the bundle blocks the
// panel ("viewer build differs from session profile") instead of
// attaching with mismatched tables.

export const BUNDLE_PINS = {
  xterm: "6.0.0",
  width: "unicode-graphemes@0.4.0",
  image: "0.9.0",
} as const;

/** True when the Profile's viewer versions match the bundle. */
export function profileMatchesBundle(profile: { xterm: string; width: string }): boolean {
  return profile.xterm === BUNDLE_PINS.xterm && profile.width === BUNDLE_PINS.width;
}
