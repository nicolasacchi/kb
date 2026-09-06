// PRR-U2 §3 (findings vs. comments — the visual language) — the finding
// card's disposition chip-row: `agree · dispute · waive · fix-later`
// (plan-arbitrated 4-set — the mock's `fixed`/`follow-up` do NOT exist on
// the server, see `api/types.ts`'s `FindingDispositionState` doc).
import { useSyncExternalStore } from "react";
import type { FindingDispositionState, ReviewFinding } from "../../api/types";
import { useDispositionMutation } from "../../hooks/useReviews";
import { isLoopbackRefusal, LOOPBACK_HINT, msg } from "./ReviewHeader";
import { toast } from "../../lib/toast";

/// Session-wide latch — mirrors `components/diff/DiffThread.tsx`'s
/// `applyLoopbackLatched` idiom EXACTLY: one 404 from a disposition write
/// hides every disposition control for the rest of this SPA session
/// (module-scoped `useSyncExternalStore`, not component state, so a remount
/// doesn't retry a call already known to be loopback-only-refused).
let dispositionLoopbackLatched = false;
const dispositionLoopbackListeners = new Set<() => void>();

function latchDispositionLoopback() {
  if (dispositionLoopbackLatched) return;
  dispositionLoopbackLatched = true;
  for (const l of dispositionLoopbackListeners) l();
}

export function useDispositionLoopbackLatched(): boolean {
  return useSyncExternalStore(
    (cb) => {
      dispositionLoopbackListeners.add(cb);
      return () => dispositionLoopbackListeners.delete(cb);
    },
    () => dispositionLoopbackLatched,
    () => dispositionLoopbackLatched,
  );
}

/// Test-only escape hatch — the latch is otherwise permanent for the tab's
/// lifetime by design (see the doc above), so a unit test that wants to
/// observe both the "not yet latched" and "latched" states needs a way back
/// to the former between cases.
export function resetDispositionLoopbackLatchForTests(): void {
  dispositionLoopbackLatched = false;
}

export const DISPOSITIONS: { key: FindingDispositionState; label: string; hint: string }[] = [
  { key: "agree", label: "agree", hint: "fix it — becomes agent work" },
  { key: "dispute", label: "dispute", hint: "opens a reply — thread goes awaiting-agent" },
  { key: "waive", label: "waive", hint: "accepted risk — publishes as a note unless unmarked" },
  { key: "fix-later", label: "fix-later", hint: "out of this PR — exportable as a todo" },
];

/// Pure click-toggle rule: clicking the ALREADY-active disposition clears it
/// (`null`, a DELETE); clicking any other disposition sets it (a PUT). Pure
/// so the toggle semantics are unit-testable without a mutation/network.
export function nextDispositionClick(
  current: FindingDispositionState | null | undefined,
  clicked: FindingDispositionState,
): FindingDispositionState | null {
  return current === clicked ? null : clicked;
}

export interface DispositionMenuProps {
  repo: string;
  reviewId: number;
  finding: ReviewFinding;
}

export default function DispositionMenu({ repo, reviewId, finding }: DispositionMenuProps) {
  const mutate = useDispositionMutation(repo);
  const loopback = useDispositionLoopbackLatched();
  const current = finding.disposition?.state ?? null;

  async function onClick(clicked: FindingDispositionState) {
    if (loopback || mutate.isPending) return;
    const next = nextDispositionClick(current, clicked);
    try {
      await mutate.mutateAsync({
        id: reviewId,
        slug: finding.slug,
        input: next === null ? null : { disposition: next },
      });
    } catch (e) {
      if (isLoopbackRefusal(e)) {
        latchDispositionLoopback();
        return;
      }
      toast.err(`couldn't update disposition: ${msg(e)}`);
    }
  }

  if (loopback) {
    return (
      <span className="kbc-finding__foot-loopback" data-kbc-finding-disposition-loopback title={LOOPBACK_HINT}>
        {LOOPBACK_HINT}
      </span>
    );
  }

  return (
    <div className="kbc-finding__dispo-row" role="group" aria-label="disposition" data-kbc-finding-disposition={finding.slug}>
      {DISPOSITIONS.map((d) => (
        <button
          key={d.key}
          type="button"
          className={"kbc-dispo" + (current === d.key ? ` kbc-dispo--active-${d.key}` : "")}
          aria-pressed={current === d.key}
          title={d.hint}
          disabled={mutate.isPending}
          onClick={() => void onClick(d.key)}
          data-kbc-finding-disposition-btn={d.key}
        >
          {current === d.key ? "✓ " : ""}
          {d.label}
        </button>
      ))}
    </div>
  );
}
