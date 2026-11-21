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

/** `201x48 · web`; `201x48` alone while the owner is unknown (undefined: no list row yet). */
export function chipText(grid: { cols: number; rows: number }, owner: string | null | undefined): string {
  if (owner === undefined) return `${grid.cols}x${grid.rows}`;
  return `${grid.cols}x${grid.rows} · ${ownerLabel(owner)}`;
}

/**
 * The owner a list row can vouch for, given the grid the stream shows NOW.
 *
 * The `["terms"]` row is a snapshot: after a grid change it keeps the old size
 * (and the old owner) until the next refetch. A row whose size differs from the
 * live grid describes a grid that no longer exists, so it must not say who owns
 * this one: `undefined` (unknown) until the refetch lands, never a stale `free`.
 * No row at all is unknown too.
 */
export function trustedOwner(
  row: { cols: number; rows: number; grid_owner?: string | null } | undefined,
  grid: { cols: number; rows: number } | null,
): string | null | undefined {
  if (!row) return undefined;
  if (grid && (row.cols !== grid.cols || row.rows !== grid.rows)) return undefined;
  return row.grid_owner ?? null;
}
