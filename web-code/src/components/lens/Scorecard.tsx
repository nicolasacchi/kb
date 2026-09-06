// DCB W2.B — the repo picker + doc-prose link-out + manual-refresh
// affordance atop the lens page. Renders `GET /api/doc-lens/repos`'s
// `codelens-scorecard/1` body; `docPublicHref`/`resolvedUnix` are threaded
// through from `Lens.tsx`'s OWN lens query (the scorecard doesn't carry
// `doc_href`) — see this component's props doc.
//
// DCB W3.C adds the header's "Save as set" button
// (`POST /api/sets/from-doc` via `useFromDocSet`) — self-contained here
// (own mutation + nav + toast), mirroring `components/sets/
// SaveAsSetButton.tsx`'s from-session precedent rather than lifting the
// mutation up into `Lens.tsx` and prop-drilling a callback down.

import { useNavigate } from "react-router-dom";
import type { ScorecardOut } from "../../api/types";
import { Icon } from "../icons";
import { formatUnixSeconds, shortSha } from "../../lib/format";
import { ApiError } from "../../api/client";
import { useFromDocSet } from "../../hooks/useSets";
import { setUrl } from "../../lib/setsUrl";
import { toast } from "../../lib/toast";

export interface ScorecardProps {
  kb: string;
  docId: string;
  selectedRepo: string;
  data: ScorecardOut | undefined;
  loading: boolean;
  error: unknown;
  onPick: (repo: string) => void;
  docTitle: string | undefined;
  /// Server-built (`codelens/1`'s `doc_href`), passed straight through from
  /// `Lens.tsx` — `null` when kb-code has no usable kb public base,
  /// `undefined` while the lens query is still in flight. Both absent cases
  /// render the title unlinked; this component never emits `href="null"`.
  docPublicHref: string | null | undefined;
  resolvedUnix: number | undefined;
  onRefresh: () => void;
  /// DCB W3.C — whether `Lens.tsx`'s OWN lens query has data yet
  /// (`lens.data !== undefined`). Gates the "Save as set" button: there is
  /// NO top-level `dirty` on `CodeLensOut` (dirtiness is `repo.dirty`,
  /// already rendered above via `is-dirty`/the dirty banner) and the button
  /// is DELIBERATELY NOT disabled on a dirty tree (R26b) — a dirty-tree
  /// materialization is exactly as legitimate as any other snapshot.
  hasLens: boolean;
}

export default function Scorecard({
  kb,
  docId,
  selectedRepo,
  data,
  loading,
  error,
  onPick,
  docTitle,
  docPublicHref,
  resolvedUnix,
  onRefresh,
  hasLens,
}: ScorecardProps) {
  const navigate = useNavigate();
  const fromDoc = useFromDocSet(selectedRepo);

  async function saveAsSet() {
    try {
      const view = await fromDoc.mutateAsync({ kb, doc: docId });
      navigate(setUrl(selectedRepo, view.id));
    } catch (e) {
      if (e instanceof ApiError && e.status === 403) {
        toast.err(
          "Saving a reading set from a document is LOOPBACK-ONLY — it only works when kb-code is reached at 127.0.0.1.",
        );
      } else {
        toast.err(`couldn't save reading set: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
  }

  const title = docTitle ?? docId;
  return (
    <header className="kbc-lens__scorecard" data-kbc-lens-scorecard>
      <div className="kbc-lens__scorecard-head">
        {docPublicHref ? (
          <a
            className="kbc-lens__doc-title"
            href={docPublicHref}
            target="_blank"
            rel="noreferrer"
            data-kbc-lens-doc-link
          >
            {title} <Icon.External width={12} height={12} aria-hidden />
          </a>
        ) : (
          <span className="kbc-lens__doc-title" data-kbc-lens-doc-title>
            {title}
          </span>
        )}
        <button
          type="button"
          className="kbc-lens__save-set-btn"
          onClick={() => void saveAsSet()}
          disabled={!hasLens || fromDoc.isPending}
          title="Materialize this document's resolved code references into a reading set"
          data-kbc-lens-save-set
        >
          {fromDoc.isPending ? "Saving…" : "Save as set"}
        </button>
        {resolvedUnix !== undefined && (
          <span className="kbc-lens__resolved" data-kbc-lens-resolved>
            resolved {formatUnixSeconds(resolvedUnix)}
            <button
              type="button"
              className="kbc-lens__refresh"
              onClick={onRefresh}
              title="refresh this checkout's lens"
              aria-label="refresh this checkout's lens"
              data-kbc-lens-refresh
            >
              <Icon.Refresh width={12} height={12} />
            </button>
          </span>
        )}
      </div>

      {loading && <div className="kbc-reader__hint">Loading checkouts…</div>}
      {!loading && !!error && (
        <div className="kbc-reader__hint kbc-reader__hint--error">
          {error instanceof ApiError || error instanceof Error ? error.message : "Failed to load checkouts."}
        </div>
      )}
      {!loading && !error && data && data.repos.length === 0 && (
        <div className="kbc-reader__hint">kb-code has no checkouts configured.</div>
      )}
      {!loading && !error && data && data.repos.length > 0 && (
        <div className="kbc-lens__repos" role="tablist" aria-label="choose a checkout" data-kbc-lens-repos>
          {data.repos.map((r) => {
            // DCB-W2.B.R fix 10 — guard on the counts THEMSELVES being
            // non-null, not on `state === "ready"` as a proxy for it: the
            // server invariant ("ready" ⇒ all four counts `Some`) isn't
            // something the TYPE SYSTEM enforces here (`present`/`ambiguous`/
            // `absent` are all `number | null`), and trusting it blindly
            // produced two visible bugs when it slipped — `{r.present}/
            // {total}` rendered as bare "/0" (JSX drops a `null` child but
            // keeps the literal "/" and the `?? 0`-defaulted `total`), and
            // the `title` string interpolated `null` as the literal text
            // "null present · null ambiguous · null absent".
            const hasCounts = r.present != null && r.ambiguous != null && r.absent != null;
            const total = hasCounts ? (r.present ?? 0) + (r.ambiguous ?? 0) + (r.absent ?? 0) + (r.external ?? 0) : null;
            const notReady = r.state !== "ready";
            const reasonLabel = r.reason ?? r.state;
            return (
              <button
                key={r.name}
                type="button"
                role="tab"
                aria-selected={selectedRepo === r.name}
                disabled={r.state === "indexing"}
                className={`kbc-lens__repo${selectedRepo === r.name ? " is-selected" : ""}${r.dirty ? " is-dirty" : ""}`}
                onClick={() => onPick(r.name)}
                title={
                  notReady
                    ? reasonLabel
                    : hasCounts
                      ? `${r.present} present · ${r.ambiguous} ambiguous · ${r.absent} absent`
                      : reasonLabel
                }
                data-kbc-lens-repo={r.name}
              >
                <span className="kbc-lens__repo-name">{r.name}</span>
                {r.head_sha && <span className="kbc-lens__repo-sha">{shortSha(r.head_sha)}</span>}
                {r.dirty && (
                  <span className="kbc-lens__repo-dirty" aria-hidden title="uncommitted changes">
                    ●
                  </span>
                )}
                {/* DCB-W2.B.R fix 3 — `ScorecardRepoRow.partial` was never
                    rendered anywhere; a repo whose own resolution hit the
                    deadline before finishing now says so. */}
                {r.partial && (
                  <span
                    className="kbc-lens__repo-partial"
                    title="resolution incomplete for this checkout"
                    data-kbc-lens-repo-partial
                  >
                    partial
                  </span>
                )}
                {notReady ? (
                  <span className="kbc-lens__repo-state">
                    {r.state === "indexing" ? "indexing…" : "error"}
                  </span>
                ) : hasCounts ? (
                  <span className="kbc-lens__repo-state">
                    {r.present}/{total}
                  </span>
                ) : null /* fix 10 — a "ready" repo with null counts (a
                    contract violation the wire type doesn't rule out) renders
                    NOTHING here rather than a fabricated "0". */}
                {/* `state === "error"` already renders `reasonLabel` in the
                    row's own `title` (above) — Decision reconciliation
                    R10: `notReady` covers BOTH "indexing" and "error", so
                    there is no separate reachable case where `!notReady &&
                    state === "error"` could ever be true. */}
              </button>
            );
          })}
        </div>
      )}
      {!loading && !error && data && data.repos.find((r) => r.name === selectedRepo)?.dirty && (
        <div className="kbc-lens__dirty-banner" data-kbc-lens-dirty-banner>
          resolved against uncommitted working tree @{" "}
          {shortSha(data.repos.find((r) => r.name === selectedRepo)?.head_sha ?? "unknown")}
        </div>
      )}
    </header>
  );
}
