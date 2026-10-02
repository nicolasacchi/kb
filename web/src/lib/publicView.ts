// The ONE projection through which review data leaves the operator's own
// comments panel. `fetchReview` asks for `?visibility=all` (the SPA's only way
// to show a private note to the operator), so the cached file carries notes.
// Anything that crosses the operator boundary — the postMessage into the
// untrusted artifact iframe, the Claude prompt, the Markdown/JSON/portable
// exports, any future "send to agent" action — MUST go through this helper.
// Fail-closed: only `private !== true` survives, so a missing flag from an
// older daemon is treated as public (it cannot be a note).
import type { ReviewFile } from "../api/client";

export function publicView(file: ReviewFile): ReviewFile {
  return {
    ...file,
    comments: file.comments.filter((c) => c.private !== true),
  };
}
