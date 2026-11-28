// FXPHONE — breadcrumb collapse for a 360 px sheet: keep the root and the last
// two segments, fold everything in between into one "…" that jumps to the
// deepest hidden directory. Pure; 1-2 segments are returned untouched.

export interface Crumb {
  /** The segment name, or "…" for the folded run. */
  label: string;
  /** The path (relative to the root) this crumb navigates to. */
  to: string;
  collapsed: boolean;
}

export function crumbsOf(path: string, collapse: boolean): Crumb[] {
  const segs = path === "" ? [] : path.split("/");
  const all = segs.map((label, i) => ({ label, to: segs.slice(0, i + 1).join("/"), collapsed: false }));
  if (!collapse || all.length <= 2) return all;
  const hidden = all.slice(0, all.length - 2);
  // Root stays as the separate first crumb the caller renders; the fold stands for `hidden`.
  return [{ label: "…", to: hidden[hidden.length - 1].to, collapsed: true }, ...all.slice(-2)];
}
