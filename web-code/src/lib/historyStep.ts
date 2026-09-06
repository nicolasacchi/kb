// Pure history-stepping logic for the reader's `[c`/`]c` vim bindings
// (Wave C — "time travel in place") and the History inspector tab's
// "current position" highlight. Kept DOM/router-free so it's directly
// vitest-covered, same split as `lib/blameGutter.ts`/`lib/peekState.ts`.
//
// `entries` is `file-history/1`'s own newest-first `CommitSummary[]`
// (`GET /api/file-history`). Positions are: `-1` = the WORKING TREE (the
// newest possible position, ahead of every commit — no `?ref=` in the
// URL), `0..entries.length-1` = that index into `entries` (`0` is the
// newest commit that ever touched this file).

import type { CommitSummary } from "../api/types";

/// A sha↔ref match tolerant of a short prefix on EITHER side (a URL
/// `?ref=` may be a caller-typed short prefix, or vice versa) — not a
/// security check, just enough fuzziness that a hand-edited URL still
/// highlights/steps from the right row.
function shaMatches(entrySha: string, ref: string): boolean {
  return entrySha === ref || entrySha.startsWith(ref) || ref.startsWith(entrySha);
}

/// The current position's index into `entries` — `-1` for the working
/// tree (`currentRef === undefined`) OR when `currentRef` doesn't match any
/// entry (an unrecognized ref, e.g. a branch name, or a sha older than
/// `entries`' own truncation — treated as "no known position," the same
/// safe default stepping OLDER from the working tree already uses).
export function currentHistoryIndex(entries: CommitSummary[], currentRef: string | undefined): number {
  if (currentRef === undefined) return -1;
  return entries.findIndex((e) => shaMatches(e.sha, currentRef));
}

export type HistoryStepResult =
  | { kind: "navigate"; sha: string | undefined }
  | { kind: "warn"; message: string };

/// `dir: -1` (`[c`) steps OLDER (further back through `entries`, newest
/// first); `dir: 1` (`]c`) steps NEWER (toward the working tree). `curIndex`
/// should come from `currentHistoryIndex` — `-1` doubles as both "genuinely
/// at the working tree" and "unknown position," which is why stepping
/// NEWER from `-1` warns rather than navigating (there's nothing newer to
/// go to either way) while stepping OLDER from `-1` always lands on
/// `entries[0]` (a safe, unambiguous first step either way).
export function historyStepTarget(entries: CommitSummary[], curIndex: number, dir: -1 | 1): HistoryStepResult {
  if (entries.length === 0) {
    return { kind: "warn", message: "no history for this file" };
  }
  if (dir === -1) {
    const nextIndex = curIndex + 1;
    if (nextIndex >= entries.length) {
      return { kind: "warn", message: "oldest commit" };
    }
    return { kind: "navigate", sha: entries[nextIndex].sha };
  }
  // dir === 1 — newer.
  if (curIndex <= -1) {
    return { kind: "warn", message: "back to working tree" };
  }
  if (curIndex === 0) {
    return { kind: "navigate", sha: undefined };
  }
  return { kind: "navigate", sha: entries[curIndex - 1].sha };
}
