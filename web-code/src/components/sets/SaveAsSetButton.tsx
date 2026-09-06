import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { ApiError } from "../../api/client";
import { useFromSessionSet } from "../../hooks/useSets";
import { setUrl } from "../../lib/setsUrl";
import { toast } from "../../lib/toast";

export interface SaveAsSetButtonProps {
  sessionId: string;
  /// Every repo this session touched (`SessionDiff.repos_touched`) — used to
  /// resolve which repo a reading set is scoped to when the page's own
  /// `?repo=` wasn't given. A from-session set is scoped to exactly ONE repo
  /// (`reading_sets`'s module doc) — this button never guesses one out of an
  /// ambiguous session on its own; it degrades to a small picker instead.
  reposTouched: string[];
  /// The page's own `?repo=` query param, if given — always wins over
  /// `reposTouched` when present.
  explicitRepo?: string;
}

/// Phase E4 — SessionDiff's "Save as reading set": materializes this
/// session's touched-file evidence into a NEW reading set (`POST /api/sets/
/// from-session`, LOOPBACK-ONLY — the exact same gate `useSessionDiff`
/// itself rides, see `reading_sets::from_session_route`'s doc). Success
/// navigates straight to the new set's detail page; a loopback refusal (or
/// any other failure) surfaces as an explanatory toast rather than a silent
/// no-op.
export default function SaveAsSetButton({ sessionId, reposTouched, explicitRepo }: SaveAsSetButtonProps) {
  const navigate = useNavigate();
  const [picked, setPicked] = useState<string | undefined>(undefined);
  const needsPicker = !explicitRepo && reposTouched.length > 1;
  const resolvedRepo = explicitRepo ?? (reposTouched.length === 1 ? reposTouched[0] : picked);
  // `resolvedRepo` may still be `undefined` (an unresolved ambiguous-repo
  // pick) — the hook still needs SOME string at creation time (React's
  // rules-of-hooks forbid conditionally calling it), but `save()` below
  // never invokes `.mutateAsync()` until `resolvedRepo` is truthy.
  const fromSession = useFromSessionSet(resolvedRepo ?? "");

  async function save() {
    if (!resolvedRepo) return;
    try {
      const view = await fromSession.mutateAsync({ session_id: sessionId });
      navigate(setUrl(resolvedRepo, view.id));
    } catch (e) {
      if (e instanceof ApiError && e.status === 403) {
        toast.err(
          "Saving a reading set from a session is LOOPBACK-ONLY — it only works when kb-code is reached at 127.0.0.1.",
        );
      } else {
        toast.err(`couldn't save reading set: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
  }

  return (
    <div className="kbc-sessiondiff__save-set" data-kbc-save-set>
      {needsPicker && (
        <select
          className="kbc-sessiondiff__save-set-repo"
          value={picked ?? ""}
          onChange={(e) => setPicked(e.target.value || undefined)}
          aria-label="repo to save this reading set into"
          data-kbc-save-set-repo
        >
          <option value="">Choose a repo…</option>
          {reposTouched.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </select>
      )}
      <button
        type="button"
        className="kbc-sessiondiff__save-set-btn"
        onClick={() => void save()}
        disabled={!resolvedRepo || fromSession.isPending}
        title="Materialize this session's touched files into a reading set"
        data-kbc-save-set-btn
      >
        {fromSession.isPending ? "Saving…" : "Save as reading set"}
      </button>
    </div>
  );
}
