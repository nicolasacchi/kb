// LSC-4 — the global "N waiting" chip: the piece design §7 calls "the piece
// that makes the feature worth building — the operator should not have to
// go look." Visible on every route (mounted in Header.tsx, chrome that
// never unmounts on navigation); same geometry/hidden-at-zero convention as
// the existing AnchorPill/InboxPill chips beside it (Header.tsx) — extracted
// to its own file (unlike those two, which are inline) so it gets direct
// component coverage without mounting the whole Header (workspace
// selector, identity chip, saved queries, …).
//
// Reads the SAME cached `useLiveSessions()` query the /sessions Now band
// reads (`["sessions","live"]`, invariant #23's fifth documented exception)
// — mounting this chip does not add a second poll/connection (#24); it's
// one more consumer of one shared cache entry.

import { Link } from "react-router-dom";
import { Icon } from "@kb/ui/icons";
import { useLiveSessions } from "../../hooks/useLiveSessions";
import { blockedCount, waitingCount } from "../../lib/liveSessionLanes";

export default function WaitingChip() {
  const { rows } = useLiveSessions();
  const count = waitingCount(rows);
  const blocked = blockedCount(rows);
  // Zero cost when quiet: no element at all, so the chip never reserves
  // layout space it isn't using (design §7's "must not shift layout when it
  // appears" — same hidden-at-zero rule AnchorPill/InboxPill already follow).
  if (count === 0 && blocked === 0) return null;
  // OL IDEA1: blocked rows (a permission prompt nobody answered) lead the
  // chip and link to the "Needs you" lane; with none, the chip is exactly
  // the pre-IDEA1 waiting chip.
  if (blocked > 0) {
    const plural = (n: number) => `${n} session${n === 1 ? "" : "s"}`;
    const text = `${blocked} blocked${count > 0 ? ` · ${count} waiting` : ""}`;
    const title =
      `${plural(blocked)} blocked on you` + (count > 0 ? ` · ${count} waiting on you` : "");
    // The accessible name starts with the visible text (WCAG 2.5.3).
    const label = `${text}: ${title}`;
    return (
      <Link
        to="/sessions#kb-now-blocked"
        className="kb-waiting kb-waiting--blocked"
        data-testid="header-waiting"
        data-kb-blocked=""
        title={title}
        aria-label={label}
      >
        <Icon.Terminal aria-hidden />
        <span>{text}</span>
      </Link>
    );
  }
  return (
    <Link
      to="/sessions#kb-now-waiting"
      className="kb-waiting"
      data-testid="header-waiting"
      title={`${count} session${count === 1 ? "" : "s"} waiting on you`}
      aria-label={`${count} session${count === 1 ? "" : "s"} waiting on you`}
    >
      <Icon.Terminal aria-hidden />
      <span>{count}</span>
    </Link>
  );
}
