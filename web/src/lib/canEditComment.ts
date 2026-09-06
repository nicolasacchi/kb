// v0.34 W — multi-user comment edit gate (kb-users/1).
//
// Role (`author: you|claude`) is not ownership. Ownership is the
// server-stamped `user` attribution. Legacy rows (no `user`) belong to
// the operator-fallback identity only — the same default the identity
// ladder stamps for loopback / legacy shared-token callers.

/**
 * Whether the current identity may edit this comment's body (and, by
 * the same ownership rule, detach its attachments).
 *
 * - `comment.user === me` → mine, editable.
 * - `comment.user` absent/empty (legacy pre-multi-user row) → editable
 *   only when `me` is the CONFIGURED operator (`identity.operator` from
 *   /api/identity — the server resolves legacy ownership to the
 *   configured name, never a hardcoded default).
 * - otherwise → not editable (a teammate's comment).
 * - `me` or `operator` unresolved → not editable (fail closed; the
 *   server 403 is the backstop either way).
 */
export function canEditComment(
  comment: { user?: string | null },
  me: string | null | undefined,
  operator: string | null | undefined,
): boolean {
  if (!me) return false;
  const owner = comment.user?.trim() || "";
  if (!owner) return !!operator && me === operator;
  return owner === me;
}
