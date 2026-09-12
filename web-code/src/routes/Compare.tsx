import { useEffect, useMemo, useState, type FormEvent } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import FileChangeRow from "../components/history/FileChangeRow";
import MergeCheckCard from "../components/history/MergeCheckCard";
import PrCommentsStrip from "../components/history/PrCommentsStrip";
import { Icon } from "../components/icons";
import RefTypeahead from "../components/RefTypeahead";
import { useCompare } from "../hooks/useCompare";
import { useRefs } from "../hooks/useRefs";
import { commitUrl, compareUrl, rangeDiffUrl } from "../lib/codeUrl";
import { formatUnixSeconds, shortSha } from "../lib/format";
import { prNumberFromRef } from "../lib/prRef";
import { defaultGroupedView, groupCommitsBySession } from "../lib/reviewGroups";
import { sessionUrl } from "../lib/searchLanes";
import "../styles/history.css";

type ViewMode = "flat" | "grouped";

/// `/r/:repo/~compare?from=&to=[&dots=3]` (Phase C2's SPA half) — repo-level
/// compare: `GET /api/compare`'s commit series + merged file stats, either
/// two-dot (`from..to`) or three-dot (`from...to`, ranged from the
/// merge-base — see `history::compare`'s own module doc). Deep-linkable —
/// every input's current value round-trips through the URL via
/// `compareUrl`, so submitting the form or following a `compareUrl(...)`
/// link (Branches' ahead/behind columns) land on the exact same state.
///
/// Phase G-server extends this into the review workflow's home: a
/// merge-readiness card (`MergeCheckCard`, G1), a "compare rebases →" link
/// into the range-diff page (C6), and — the headline (G3) — SESSION-GROUPED
/// review: every commit is fetched with `?attribution=true` and can be
/// viewed either flat (the pre-G-server list) or grouped by its attributed
/// session (`lib/reviewGroups.ts`), reviewing an agent branch as the
/// narrative of the conversations that made it rather than an anonymous
/// commit list. Grouped is the DEFAULT once there's an actual narrative to
/// tell (>= 2 distinct attributed sessions — `defaultGroupedView`); the
/// operator can always flip the toggle either way, and a fresh compare
/// (different repo/from/to/dots) resets back to that computed default.
/// Whenever `to` names a fetched PR head (`refs/kbc/pr/<n>`,
/// `lib/prRef.ts`), a read-only side strip renders that PR's GitHub
/// comments (G4, `PrCommentsStrip`).
export default function Compare() {
  const { repo = "" } = useParams<{ repo: string }>();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();

  const fromParam = searchParams.get("from") ?? "";
  const toParam = searchParams.get("to") ?? "";
  const threeDot = searchParams.get("dots") === "3";

  // V76-R4d.2 — react-router 7 wraps every navigation state update in
  // React.startTransition (no opt-out), so a controlled checkbox bound
  // DIRECTLY to a search param stays at the last committed render's value
  // until the transition lands: React's restoreControlledState snaps the
  // DOM node back right after the change event. The house pattern (the
  // Stacks `all` toggle, V76-R4d round 2) is LOCAL OPTIMISTIC state: the
  // box flips on the same tick as the change and reconciles with the URL
  // when the navigation commits (the effect below). The compare QUERY
  // keeps reading the URL — the URL stays the source of truth for data;
  // only the checkbox's own checked rendering is optimistic.
  const [threeDotOptimistic, setThreeDotOptimistic] = useState(threeDot);
  useEffect(() => {
    setThreeDotOptimistic(threeDot);
  }, [threeDot]);

  const [fromInput, setFromInput] = useState(fromParam);
  const [toInput, setToInput] = useState(toParam);
  const { data: refsData } = useRefs(repo);
  const refItems = useMemo(() => refsData?.refs.map((r) => r.name) ?? [], [refsData]);

  // The URL is the source of truth (a fresh navigation — e.g. from
  // Branches' ahead/behind links — should always reset the form's local
  // input state, not leave the PREVIOUS compare's typed values showing).
  useEffect(() => {
    setFromInput(fromParam);
    setToInput(toParam);
  }, [fromParam, toParam]);

  const compare = useCompare(repo, fromParam || undefined, toParam || undefined, threeDot);

  // G3 — the flat/grouped toggle. `null` means "no explicit operator
  // choice yet — use the computed default"; an explicit click pins the
  // current view until the NEXT distinct compare (a different repo/from/
  // to/dots resets it back to `null`, so the default is recomputed fresh
  // rather than a stale manual choice leaking across compares).
  const [manualView, setManualView] = useState<ViewMode | null>(null);
  useEffect(() => {
    setManualView(null);
  }, [repo, fromParam, toParam, threeDot]);

  const groups = useMemo(() => (compare.data ? groupCommitsBySession(compare.data.commits) : []), [compare.data]);
  const autoGrouped = useMemo(
    () => (compare.data ? defaultGroupedView(compare.data.commits) : false),
    [compare.data],
  );
  const viewMode: ViewMode = manualView ?? (autoGrouped ? "grouped" : "flat");

  // G4 — a compare whose `to` names a fetched PR head gets a read-only
  // comments side strip (`PrCommentsStrip`); every other compare renders
  // `null` here, same layout as before G4 ever existed.
  const prNumber = useMemo(() => prNumberFromRef(toParam), [toParam]);

  function go(next: { from?: string; to?: string; threeDot?: boolean }) {
    navigate(
      compareUrl(repo, {
        from: next.from ?? fromInput,
        to: next.to ?? toInput,
        threeDot: next.threeDot ?? threeDot,
      }),
    );
  }

  function onSubmit(e: FormEvent) {
    e.preventDefault();
    go({});
  }

  function onSwap() {
    setFromInput(toInput);
    setToInput(fromInput);
    go({ from: toInput, to: fromInput });
  }

  const data = compare.data;
  const empty = !!data && data.commits.length === 0 && data.files.length === 0;
  // Three-dot's per-file diff base is the merge-base (falling back to
  // `from_sha` when there's no common ancestor — the same degrade
  // `history::compare`'s own module doc documents for its commit list).
  const fileDiffFrom = data
    ? threeDot
      ? (data.resolved.merge_base ?? data.resolved.from_sha)
      : data.resolved.from_sha
    : undefined;

  return (
    <div className="kbc-compare">
      <form className="kbc-compare__head" onSubmit={onSubmit}>
        <RefTypeahead
          value={fromInput}
          onChange={setFromInput}
          items={refItems}
          placeholder="base ref"
          aria-label="base ref"
          inputClassName="kbc-compare__ref-input"
          inputProps={{ "data-kbc-compare-from": "" }}
        />
        <button
          type="button"
          className="kbc-compare__swap"
          onClick={onSwap}
          title="Swap base/compare"
          aria-label="Swap base/compare"
          data-kbc-compare-swap
        >
          <Icon.Swap />
        </button>
        <RefTypeahead
          value={toInput}
          onChange={setToInput}
          items={refItems}
          placeholder="compare ref"
          aria-label="compare ref"
          inputClassName="kbc-compare__ref-input"
          inputProps={{ "data-kbc-compare-to": "" }}
        />
        <button type="submit" className="kbc-compare__go">
          Compare
        </button>
        <label className="kbc-compare__dots-toggle">
          <input
            type="checkbox"
            checked={threeDotOptimistic}
            onChange={(e) => {
              setThreeDotOptimistic(e.target.checked);
              go({ threeDot: e.target.checked });
            }}
            data-kbc-compare-threedot
          />
          three-dot
        </label>
      </form>

      <Link to={rangeDiffUrl(repo)} className="kbc-compare__rangediff-link" data-kbc-compare-rangediff-link>
        compare rebases →
      </Link>

      {!fromParam || !toParam ? (
        <div className="kbc-reader__hint">Enter both a base and a compare ref.</div>
      ) : compare.isLoading ? (
        <div className="kbc-reader__hint">Loading compare…</div>
      ) : compare.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(compare.error as Error).message}</div>
      ) : data ? (
        <div className={prNumber !== null ? "kbc-compare__layout" : undefined}>
          <div className="kbc-compare__main">
            <div className="kbc-compare__resolved" data-kbc-compare-resolved>
              {shortSha(data.resolved.from_sha)}
              {threeDot && data.resolved.merge_base && (
                <>
                  {" "}
                  (merge-base <span className="kbc-compare__mergebase">{shortSha(data.resolved.merge_base)}</span>)
                </>
              )}
              {" → "}
              {shortSha(data.resolved.to_sha)}
            </div>

            <MergeCheckCard repo={repo} from={fromParam} to={toParam} />

            {empty ? (
              <div className="kbc-reader__hint" data-kbc-compare-empty>
                No difference between {fromParam} and {toParam}.
              </div>
            ) : (
              <>
                <section className="kbc-compare__commits">
                  <div className="kbc-compare__commits-head">
                    <h2 className="kbc-compare__section-title">
                      Commits{data.commits_truncated ? " (truncated)" : ""}
                    </h2>
                    <div className="kbc-compare__view-toggle" role="group" aria-label="Commit grouping">
                      <button
                        type="button"
                        className={"kbc-compare__view-opt" + (viewMode === "flat" ? " is-active" : "")}
                        aria-pressed={viewMode === "flat"}
                        onClick={() => setManualView("flat")}
                        data-kbc-compare-view="flat"
                      >
                        Flat
                      </button>
                      <button
                        type="button"
                        className={"kbc-compare__view-opt" + (viewMode === "grouped" ? " is-active" : "")}
                        aria-pressed={viewMode === "grouped"}
                        onClick={() => setManualView("grouped")}
                        data-kbc-compare-view="grouped"
                      >
                        By session
                      </button>
                    </div>
                  </div>

                  {viewMode === "flat" ? (
                    <ul className="kbc-compare__commit-list">
                      {data.commits.map((c) => (
                        <li key={c.sha} className="kbc-compare__commit">
                          <Link to={commitUrl(repo, c.sha)} className="kbc-compare__commit-sha">
                            {shortSha(c.sha)}
                          </Link>
                          <span className="kbc-compare__commit-subject">{c.subject}</span>
                          <span className="kbc-compare__commit-meta">
                            {c.author} · {formatUnixSeconds(c.author_time)}
                          </span>
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <ul className="kbc-compare__group-list" data-kbc-compare-groups>
                      {groups.map((g) => (
                        <li
                          key={g.sessionId ?? "__none__"}
                          className={"kbc-compare__group" + (g.sessionId === null ? " kbc-compare__group--none" : "")}
                          data-kbc-compare-group={g.sessionId ?? "none"}
                        >
                          <div className="kbc-compare__group-head">
                            {g.sessionId ? (
                              <a
                                className="kbc-compare__group-name"
                                href={sessionUrl(g.sessionId)}
                                target="_blank"
                                rel="noreferrer"
                              >
                                {g.displayName ?? g.sessionId}
                              </a>
                            ) : (
                              <span
                                className="kbc-compare__group-name kbc-compare__group-name--none"
                                data-kbc-compare-group-none
                              >
                                no recorded session
                              </span>
                            )}
                            {g.confidence && (
                              <span
                                className={`kbc-attrib__confidence kbc-attrib__confidence--${g.confidence}`}
                                data-kbc-attrib-confidence={g.confidence}
                              >
                                {g.confidence}
                              </span>
                            )}
                            <span className="kbc-compare__group-count" data-kbc-compare-group-count>
                              {g.commits.length} commit{g.commits.length === 1 ? "" : "s"}
                            </span>
                          </div>
                          <ul className="kbc-compare__commit-list">
                            {g.commits.map((c) => (
                              <li key={c.sha} className="kbc-compare__commit">
                                <Link to={commitUrl(repo, c.sha)} className="kbc-compare__commit-sha">
                                  {shortSha(c.sha)}
                                </Link>
                                <span className="kbc-compare__commit-subject">{c.subject}</span>
                                <span className="kbc-compare__commit-meta">
                                  {c.author} · {formatUnixSeconds(c.author_time)}
                                </span>
                              </li>
                            ))}
                          </ul>
                        </li>
                      ))}
                    </ul>
                  )}
                </section>

                <section className="kbc-compare__files">
                  <div className="kbc-compare__files-head">
                    {data.totals.files} file{data.totals.files === 1 ? "" : "s"} changed · +
                    {data.totals.insertions} -{data.totals.deletions}
                  </div>
                  {data.files.map((f) => (
                    <FileChangeRow
                      key={f.path}
                      repo={repo}
                      file={f}
                      from={fileDiffFrom}
                      to={data.resolved.to_sha}
                      browseRef={data.resolved.to_sha}
                    />
                  ))}
                </section>
              </>
            )}
          </div>
          {prNumber !== null && <PrCommentsStrip repo={repo} number={prNumber} atRef={toParam} />}
        </div>
      ) : null}
    </div>
  );
}
