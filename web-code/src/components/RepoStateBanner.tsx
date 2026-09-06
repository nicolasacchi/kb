import { useState } from "react";
import { Link } from "react-router-dom";
import { Icon } from "./icons";
import { useRepoState } from "../hooks/useRepoState";
import { readerUrl } from "../lib/breadcrumbs";
import { commitUrl } from "../lib/codeUrl";
import { shortSha } from "../lib/format";
import type { OpDetail, RepoOp } from "../api/types";

export interface RepoStateBannerProps {
  repo: string;
}

function opLabel(op: RepoOp): string {
  return op === "cherry-pick" ? "cherry-pick" : op;
}

/// A stable fingerprint of the repo-state fields this banner actually
/// shows — the dismiss/reappear contract keys on THIS, not on the query's
/// own fetch identity: a refetch that resolves to the SAME state must stay
/// dismissed, but any actual CHANGE (a rebase advances a step, a new file
/// conflicts, the operation resolves and a fresh one starts) must
/// re-surface the banner even if it was dismissed a moment ago.
function signatureOf(op: RepoOp, detail: OpDetail, conflicted: string[]): string {
  return JSON.stringify([op, detail.step, detail.total, detail.head_sha, conflicted]);
}

/// Phase G2 — the reader chrome's repo-state banner: `GET /api/repo-state`
/// fetched on mount (`useRepoState`, refetched via the SSE bridge's generic
/// per-repo invalidation — see that hook's own doc) and rendered whenever
/// `op !== "none"` — "Repository is mid-<op>", the rebase step X/Y when
/// present, and every conflicted path as a chip linking into the reader
/// (`readerUrl`, working tree — the reader already renders the raw conflict
/// markers, and Phase G2's CM6 overlay additionally tints them, see
/// `editor/conflictOverlay.ts`). Dismissible; reappears the moment the
/// state's own fingerprint changes (`signatureOf`), even while still
/// mid-op — mirrors `HeadMovedBanner`'s own conventions (`components/
/// LiveMirrorBanners.tsx`) under a new `kbc-repostate-banner` class rather
/// than folding into that module (repo-state has no live-mirror event of
/// its own to piggyback on — it's a plain poll-on-mount + SSE-invalidated
/// query, not an SSE payload this banner reads directly).
export default function RepoStateBanner({ repo }: RepoStateBannerProps) {
  const repoState = useRepoState(repo);
  const [dismissedSig, setDismissedSig] = useState<string | null>(null);
  const data = repoState.data;

  if (!data || data.op === "none") return null;
  const sig = signatureOf(data.op, data.detail, data.conflicted);
  if (sig === dismissedSig) return null;

  return (
    <div className="kbc-repostate-banner" role="status" data-kbc-repostate-banner data-kbc-repostate-op={data.op}>
      <span className="kbc-repostate-banner__msg">
        Repository is mid-{opLabel(data.op)}
        {data.detail.step !== undefined && data.detail.total !== undefined && (
          <span className="kbc-repostate-banner__step" data-kbc-repostate-step>
            {" "}
            (step {data.detail.step}/{data.detail.total})
          </span>
        )}
        {data.detail.head_sha && (
          <>
            {" — "}
            <Link to={commitUrl(repo, data.detail.head_sha)} data-kbc-repostate-head>
              {shortSha(data.detail.head_sha)}
            </Link>
          </>
        )}
      </span>
      {data.conflicted.length > 0 && (
        <span className="kbc-repostate-banner__conflicts" data-kbc-repostate-conflicts>
          {data.conflicted.map((path) => (
            <Link
              key={path}
              to={readerUrl(repo, path)}
              className="kbc-repostate-banner__conflict-chip"
              data-kbc-repostate-conflict={path}
            >
              {path}
            </Link>
          ))}
        </span>
      )}
      <button
        type="button"
        className="kbc-repostate-banner__dismiss"
        onClick={() => setDismissedSig(sig)}
        aria-label="dismiss"
        data-kbc-repostate-dismiss
      >
        <Icon.X />
      </button>
    </div>
  );
}
