/** Phone history cap (client-side trim only; the join still ships the server seed). */
export const PHONE_SCROLLBACK_CAP = 20_000;

/** The scrollback a terminal created for this form factor carries. */
export function scrollbackFor(profile: { scrollback: number }, phone: boolean): number {
  return phone ? Math.min(profile.scrollback, PHONE_SCROLLBACK_CAP) : profile.scrollback;
}
