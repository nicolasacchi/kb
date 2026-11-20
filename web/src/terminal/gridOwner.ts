// FUTERMFIT — who owns the session grid, in words.
//
// The runner records one owner per session: `kb-term` is the relay (every web
// client shares that one name, so it cannot tell windows apart: never say
// "this window"), `local:<pid>` is a `kb-term attach`, null/empty is nobody.

export type OwnerClass = "free" | "web" | "local" | "other";

/** The relay's grid-owner name for a grid a web client took (`routes/terms.rs`). */
export const WEB_OWNER = "kb-term";
const LOCAL_PREFIX = "local:";

export function ownerClass(owner: string | null | undefined): OwnerClass {
  if (owner === null || owner === undefined || owner === "") return "free";
  if (owner === WEB_OWNER) return "web";
  if (owner.startsWith(LOCAL_PREFIX)) return "local";
  return "other";
}

export function ownerLabel(owner: string | null | undefined): string {
  switch (ownerClass(owner)) {
    case "free":
      return "free";
    case "web":
      return "web";
    case "local":
      return "local attach";
    default:
      return owner as string;
  }
}

/** `201x48 · web` (the owner is dropped to `free` until the list row is known). */
export function chipText(grid: { cols: number; rows: number }, owner: string | null | undefined): string {
  return `${grid.cols}x${grid.rows} · ${ownerLabel(owner)}`;
}
