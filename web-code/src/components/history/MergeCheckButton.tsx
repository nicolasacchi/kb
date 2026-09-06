import { useState } from "react";
import { useMergeCheck } from "../../hooks/useMergeCheck";
import { Icon } from "../icons";

export interface MergeCheckButtonProps {
  repo: string;
  from: string;
  to: string;
}

/// Phase G1 — the Branches page's per-row LAZY merge-readiness check:
/// nothing fetches until the operator clicks "Check" (a branches table with
/// N rows must never fire N eager `/api/merge-check` calls just to render —
/// see `useMergeCheck`'s own `enabled` gate), then the SAME hook the
/// Compare page's card uses (`MergeCheckCard.tsx`) answers inline: "✓" for
/// a clean merge, "N conflicts" otherwise.
export default function MergeCheckButton({ repo, from, to }: MergeCheckButtonProps) {
  const [checked, setChecked] = useState(false);
  const mergeCheck = useMergeCheck(repo, from, to, checked);

  if (!checked) {
    return (
      <button
        type="button"
        className="kbc-branches__check-btn"
        onClick={() => setChecked(true)}
        data-kbc-branches-check
      >
        Check
      </button>
    );
  }
  if (mergeCheck.isLoading) {
    return (
      <span className="kbc-branches__check-pending" data-kbc-branches-check-result="pending">
        checking…
      </span>
    );
  }
  if (mergeCheck.error) {
    return (
      <span className="kbc-branches__check-error" data-kbc-branches-check-result="error">
        check failed
      </span>
    );
  }
  const data = mergeCheck.data;
  if (!data) return null;
  return data.clean ? (
    <span className="kbc-branches__check-clean" data-kbc-branches-check-result="clean" aria-label="clean merge">
      <Icon.Check width={12} height={12} />
    </span>
  ) : (
    <span className="kbc-branches__check-conflict" data-kbc-branches-check-result="conflict">
      {data.conflicts.length} conflict{data.conflicts.length === 1 ? "" : "s"}
    </span>
  );
}
