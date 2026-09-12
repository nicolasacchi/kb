// PRR-U3 (design-ui.md §5) — the short, share/CLI-printable finding
// permalink `/r/{repo}/~reviews/{id}/f/{slug}`. `codeUrl.ts`'s `findingUrl`
// pinned this grammar's builder but PRR-U3 never registered a route for
// it (every link 404'd through the Reader catch-all, `/r/:repo/*`) — this
// is that route, V70-A3S. Mirrors `LensEntry.tsx`'s `<Navigate replace>`
// redirect-ramp pattern: this URL carries no state Reader could restore
// even if it matched, so the honest fix is a redirect straight to the real
// Room URL grammar (`reviewDiffHref(repo, id, undefined, {finding: slug})`
// → `/r/{repo}/~reviews/{id}/diff?finding={slug}` — scroll+expand+flash,
// same machinery `?thread=` uses), not a second renderer for the same
// content.

import { Navigate, useParams } from "react-router";
import { reviewDiffHref } from "../lib/codeUrl";

export default function FindingEntry() {
  const { repo = "", id = "", slug = "" } = useParams<{ repo: string; id: string; slug: string }>();
  return <Navigate replace to={reviewDiffHref(repo, id, undefined, { finding: slug })} />;
}
