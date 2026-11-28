// FXUI — file names and paths are DATA. `displayName` makes one safe to show
// as a plain text node: control characters (C0, DEL, C1) and the bidi
// controls/isolates are rendered as a visible `\u{…}` escape, everything else
// (accents, CJK, emoji) is kept. The real name is what goes in a request; this
// string is for the eye only.

// C0 + DEL + C1, the bidi embeddings/overrides/isolates and marks, the line and
// paragraph separators, and the zero-width / BOM family that hides text.
// eslint-disable-next-line no-control-regex
const UNSAFE = /[\u0000-\u001f\u007f-\u009f\u061c\u200b-\u200f\u2028-\u202e\u2060-\u2069\ufeff]/gu;

export function displayName(name: string): string {
  return name.replace(UNSAFE, (c) => `\\u{${(c.codePointAt(0) ?? 0).toString(16)}}`);
}
