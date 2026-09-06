// CT-F2 — pure derivation of the "when written" secondary badge for
// `components/lens/RefRow.tsx`, extracted for direct unit coverage
// (`whenWritten.test.ts`) — same "pure helpers live in `lib/`, JSX stays in
// the component" split `refTier.ts` already establishes.
//
// `CodeLensRef.when_written` is an ADDITIVE, era-resolved verdict alongside
// (never instead of) the ref's ever-present current-tree `path_state`/
// `line_state`. This module composes the two into exactly the two derived
// labels the design calls for — no new vocabulary beyond the state words
// already on the wire, and no badge at all when the two verdicts carry the
// same meaning (nothing extra worth saying).

import type { CodeLensRef } from "../api/types";

export type WhenWrittenBadge = "wrong-when-written" | "rotted-since";

/// `null` when `when_written` is absent (the caller didn't ask via
/// `?at=declared`, or the doc had no usable `kb-code-rev` — `era !==
/// "declared"`) or when the declared-rev verdict says nothing DIFFERENT
/// from the current-tree one.
export function whenWrittenBadge(r: CodeLensRef): WhenWrittenBadge | null {
  const w = r.when_written;
  if (!w) return null;

  // "wrong when written" — the citation was never true at the declared rev
  // at all: the path itself wasn't there (or an unusable hint refused to
  // normalise, which the engine also reports as `absent`/`absent`). This
  // holds regardless of what the CURRENT tree says — even a citation that
  // reads as confirmed today was still wrong when it was written.
  if (w.path_state_at_rev === "absent") return "wrong-when-written";

  // "rotted since" — it WAS true then (present + confirmed against the
  // declared rev's own content) but the CURRENT-tree verdict has since
  // drifted.
  if (w.line_state_at_rev === "confirmed" && r.line_state === "drifted") {
    return "rotted-since";
  }

  return null;
}

export const WHEN_WRITTEN_BADGE_LABEL: Record<WhenWrittenBadge, string> = {
  "wrong-when-written": "wrong when written",
  "rotted-since": "rotted since",
};
