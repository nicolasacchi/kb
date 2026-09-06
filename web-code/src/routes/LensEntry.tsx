// DCB W2.B — the repo-less, ID-addressed lens entry ramp (R2/D14). The
// COMMON case: a caller that already holds the artifact id (kb's own
// reader always does — `PreviewInspector` renders a concrete `Doc`, so
// 13-w1d's deep link is `{code_url}/~lens/{kb}/{docId}`, R22 — no
// resolution needed). Resolves `:repo` from `useActiveRepo`'s existing
// fallback-to-first-configured-repo behavior (`hooks/useActiveRepo.ts`),
// then hands off to the repo-scoped route — `Lens.tsx`'s own
// `pinned_repo`-correction effect does the rest (Decision 1's "last pick
// per doc pre-selects the switcher next time").

import { Navigate, useLocation, useParams } from "react-router-dom";
import { useActiveRepo } from "../hooks/useActiveRepo";
import { useRepos } from "../hooks/useRepos";
import { lensUrl } from "../lib/docLensUrl";

export default function LensEntry() {
  const { kb = "", docId = "" } = useParams<{ kb: string; docId: string }>();
  const location = useLocation();
  const repos = useRepos();
  // No explicit `:repo` in THIS route (it's the repo-less ramp) — falls
  // back to the first configured repo once `repos` has loaded
  // (`useActiveRepo.ts`'s own documented behavior).
  const activeRepo = useActiveRepo();

  if (repos.error) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error">{(repos.error as Error).message}</div>
    );
  }
  if (repos.isLoading || !activeRepo) {
    return <div className="kbc-reader__hint">Loading…</div>;
  }
  return <Navigate replace to={lensUrl(activeRepo, kb, docId) + location.search} />;
}
