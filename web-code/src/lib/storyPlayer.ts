// Pure step-index logic for Phase C7's story player ("watch this file being
// made"): oldest→newest playback ordering over `file-history/1`'s own
// newest-first entries, plus resolving a `?at=<sha>` deep-link (or its
// absence) to a starting step index. Kept DOM/router-free, same split as
// `lib/historyStep.ts` (which this deliberately does NOT reuse — that
// module's index space is `-1..entries.length-1` with `-1` meaning "the
// working tree," a concept story mode has no use for: every step here IS a
// commit, there is no live/uncommitted step).

import type { CommitSummary } from "../api/types";

/// `file-history/1` returns newest-first; the story player runs forward
/// through time, so this is simply the reversed list — `steps[0]` is the
/// file's OLDEST touching commit, `steps[length - 1]` the newest.
export function playbackSteps(entries: CommitSummary[]): CommitSummary[] {
  return [...entries].reverse();
}

/// A sha↔ref match tolerant of a short prefix on EITHER side — mirrors
/// `historyStep.ts`'s own `shaMatches` (not exported there, so restated here
/// rather than reaching across modules for one small predicate). Not a
/// security check, just enough fuzziness that a hand-typed/truncated `?at=`
/// still resolves to the right step.
function shaMatches(entrySha: string, ref: string): boolean {
  return entrySha === ref || entrySha.startsWith(ref) || ref.startsWith(entrySha);
}

/// The step index `?at=<sha>` resolves to. `undefined` (no `?at=` at all) or
/// a sha that matches no step both default to `0` — the OLDEST commit,
/// "start the story from the beginning." Returns `0` for an empty `steps`
/// too (the caller renders a "no history" state before this ever matters).
export function initialStepIndex(steps: CommitSummary[], atSha: string | undefined): number {
  if (atSha === undefined) return 0;
  const i = steps.findIndex((s) => shaMatches(s.sha, atSha));
  return i === -1 ? 0 : i;
}

/// Clamp a candidate step index into `[0, length - 1]` (`0` when `length`
/// is `0` — nothing to clamp into, but never a negative/out-of-range index
/// for callers to index with).
export function clampStep(index: number, length: number): number {
  if (length <= 0) return 0;
  return Math.min(Math.max(index, 0), length - 1);
}
