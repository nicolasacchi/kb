// v0.34 W — pure helpers for multi-user attribution chips.
//
// Users are attribution strings, not ACL principals. Own rows hide the
// username (looks like single-user today); other users' rows show a
// small chip so team activity is visible.

/**
 * True when `user` is a non-empty attribution that differs from `me`.
 * Empty / absent / equal-to-me all hide the chip.
 */
export function isOtherUser(
  user: string | null | undefined,
  me: string | null | undefined,
): boolean {
  const u = user?.trim() || "";
  if (!u) return false;
  if (!me) return false; // identity unresolved: byte-identical to pre-v0.34 (no chip flash)
  return u !== me;
}

/**
 * Role badge + optional teammate name. Own comments return just the
 * role (`you` / `claude`); a teammate's human comment returns their
 * name; their agent's returns `claude · <name>`.
 *
 * When `preferBeside` is true (default for panel meta rows), always
 * returns the role alone — the caller renders the username chip beside
 * it. When false, returns the combined label used in denser surfaces.
 */
export function authorAttributionLabel(
  author: string,
  user: string | null | undefined,
  me: string | null | undefined,
  opts: { combined?: boolean } = {},
): string {
  if (!isOtherUser(user, me)) return author;
  const name = (user ?? "").trim();
  if (!opts.combined) return author;
  if (author === "claude") return `claude · ${name}`;
  return name;
}
