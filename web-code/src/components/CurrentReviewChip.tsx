import { Link, useNavigate } from "react-router";
import { useExplicitRepo } from "../hooks/useActiveRepo";
import { mergeCurrentSearch, reviewUrl } from "../lib/codeUrl";
import { clearCurrentReview, useCurrentReview } from "../lib/currentReview";
import { Icon } from "./icons";

export interface CurrentReviewChipProps {
  /// `"bar"` (desktop TopBar) vs `"sheet"` (mobile NavSheet row) — same
  /// state, two chrome homes: root CLAUDE.md's rule for this specific chip
  /// ("≤860px: the chip lives in the mobile nav sheet, not the bar").
  variant?: "bar" | "sheet";
  /// NavSheet's own "route change closes the sheet" convention — fired
  /// after the Room link navigates, never after the clear × (which stays
  /// on the current page).
  onNavigate?: () => void;
}

/// V80-M3 — the ONE chip for "which review am I working"
/// (`lib/currentReview.ts` — a browser-only marker, the daemon has no
/// notion of it). Hidden ENTIRELY when no marker is set for the active
/// repo; clicking the label opens the Room, the `×` clears the marker and
/// strips `?review=` from the CURRENT url via one `replace` — a no-op
/// strip on any page that never carried the param (every page but a
/// reader file), since `mergeCurrentSearch` only ever touches the one key
/// it's asked to.
export default function CurrentReviewChip({ variant = "bar", onNavigate }: CurrentReviewChipProps) {
  const repo = useExplicitRepo();
  const navigate = useNavigate();
  const currentReview = useCurrentReview(repo ?? "");

  if (!repo || !currentReview) return null;
  // Re-bound so the nested `onClear` closure below sees the narrowed
  // `string` — TS does not carry the `if (!repo …) return` guard's
  // narrowing into a function declared later in the same body.
  const activeRepo = repo;

  const label = currentReview.title ?? `#${currentReview.id}`;

  function onClear() {
    clearCurrentReview(activeRepo);
    navigate({ search: mergeCurrentSearch((p) => p.delete("review")) }, { replace: true });
  }

  if (variant === "sheet") {
    return (
      <div className="kbc-navsheet__current-review" data-kbc-current-review-chip="sheet">
        <Link
          to={reviewUrl(repo, currentReview.id)}
          className="kbc-navsheet__row"
          onClick={onNavigate}
          data-kbc-current-review-room-link
        >
          Reviewing · {label}
        </Link>
        <button
          type="button"
          className="kbc-navsheet__current-review-x"
          onClick={onClear}
          title="Stop working this review"
          aria-label="Stop working this review"
          data-kbc-current-review-clear
        >
          <Icon.X />
        </button>
      </div>
    );
  }

  return (
    <span className="kbc-topbar__current-review" data-kbc-current-review-chip="bar">
      <Link
        to={reviewUrl(repo, currentReview.id)}
        className="kbc-topbar__chip kbc-topbar__current-review-link"
        data-kbc-current-review-room-link
      >
        Reviewing · {label}
      </Link>
      <button
        type="button"
        className="kbc-topbar__current-review-x"
        onClick={onClear}
        title="Stop working this review"
        aria-label="Stop working this review"
        data-kbc-current-review-clear
      >
        <Icon.X />
      </button>
    </span>
  );
}
