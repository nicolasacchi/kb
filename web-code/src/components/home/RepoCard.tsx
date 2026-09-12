import { Link } from "react-router";
import type { HeadInfo, RepoListEntry } from "../../api/types";
import { useBranches } from "../../hooks/useBranches";
import { useRecentFiles } from "../../hooks/useRecentFiles";
import { useWorkingSet } from "../../hooks/useWorkingSet";
import { readerUrl } from "../../lib/breadcrumbs";
import { branchesUrl, codeBasePath, commitUrl, hotspotsUrl, reviewsUrl } from "../../lib/codeUrl";
import { effectiveIntelProviders } from "../../lib/diagnostics";
import { shortSha } from "../../lib/format";
import { fullSearchUrl } from "../../lib/searchLanes";
import { splitPath } from "../../lib/workingSet";
import AttributionCard from "../history/AttributionCard";
import { BranchAhead, BranchBehind } from "../history/BranchAheadBehind";

const RECENT_FILES_LIMIT = 4;
const CONTINUE_READING_LIMIT = 3;
const ACTIVE_BRANCHES_LIMIT = 4;

export interface RepoCardProps {
  repo: RepoListEntry;
}

function SkeletonLines({ count }: { count: number }) {
  return (
    <div className="kbc-home-card__skeleton" aria-hidden="true">
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className="kbc-home-card__skeleton-line" />
      ))}
    </div>
  );
}

/// The HEAD chip — a repo's current branch/sha, linking to the commit page
/// hub (Phase C1). Three honest states: unborn (no commits at all — a
/// freshly `git init`'d repo), detached (a tag/sha checkout, no branch
/// name), or the ordinary named-branch case.
function HeadChip({ repoName, head }: { repoName: string; head: HeadInfo | null }) {
  if (!head || head.unborn) {
    return (
      <span className="kbc-home-card__head kbc-home-card__head--unborn" data-kbc-home-head="unborn">
        no commits yet
      </span>
    );
  }
  if (!head.sha) {
    return <span className="kbc-home-card__head">—</span>;
  }
  const label = head.branch ?? shortSha(head.sha);
  return (
    <Link
      className="kbc-home-card__head"
      to={commitUrl(repoName, head.sha)}
      title={`${head.detached ? "detached @ " : ""}${label} (${shortSha(head.sha)})`}
      data-kbc-home-head={head.sha}
    >
      {head.detached && <span className="kbc-home-card__head-tag">detached</span>}
      {label}
    </Link>
  );
}

/// F4 — one repo's dashboard card on Home: name + watcher badge + HEAD
/// chip, then three lazily-fetched sections (Continue reading from the
/// sessionStorage working set, Recent files from the files lane's own
/// empty-query frecency, Active branches from `/api/branches`), then a
/// footer link row. Each section is an INDEPENDENT React Query consumer —
/// branches/recent-files fire in parallel, no waterfall — and a failed
/// fetch renders a quiet inline note rather than breaking the whole card
/// (Home's own multi-repo fan-out must survive one repo's daemon-side
/// hiccup, same "never let one failure take down the rest" discipline as
/// kb's own federated fan-out, root CLAUDE.md invariant #28).
export default function RepoCard({ repo }: RepoCardProps) {
  const branches = useBranches(repo.name);
  const recentFiles = useRecentFiles(repo.name, RECENT_FILES_LIMIT);
  const workingSet = useWorkingSet(repo.name);
  // S2-D (design-s2.md §S2-D) — every matching lip/1 provider, preferring
  // the new `intel_providers` vec over the legacy single-valued `intel`
  // (`lib/diagnostics.ts`'s `effectiveIntelProviders`).
  const intelProviders = effectiveIntelProviders(repo);

  // "Continue reading" wants RECENCY (most-recently-touched first), unlike
  // the working-set strip's own stable insertion order (`WorkingSetStrip`'s
  // doc) — a "continue where I left off" affordance is naturally
  // recency-ranked.
  const continueReading = [...workingSet.entries].sort((a, b) => b.touchedAt - a.touchedAt).slice(0, CONTINUE_READING_LIMIT);

  const activeBranches = branches.data
    ? [...branches.data.branches]
        .filter((b) => b.name !== branches.data!.default)
        .sort((a, b) => (b.last?.author_time ?? 0) - (a.last?.author_time ?? 0))
        .slice(0, ACTIVE_BRANCHES_LIMIT)
    : null;

  return (
    <div className="kbc-home-card" data-kbc-home-card={repo.name}>
      <header className="kbc-home-card__head">
        <Link to={readerUrl(repo.name, "")} className="kbc-home-card__name" data-kbc-home-card-name>
          {repo.name}
        </Link>
        <span
          className={`kbc-repopopover__badge kbc-repopopover__badge--${repo.watcher}`}
          data-kbc-watcher-state={repo.watcher}
          title={`live-mirror watcher: ${repo.watcher}`}
        >
          {repo.watcher}
        </span>
      </header>
      <div className="kbc-home-card__subhead">
        <HeadChip repoName={repo.name} head={repo.head} />
        <span className="kbc-home-card__counts">
          {repo.file_count} files · {repo.symbol_count} symbols
        </span>
      </div>

      {intelProviders.length > 0 && (
        <div className="kbc-home-card__intel" data-kbc-home-intel>
          {intelProviders.map((p) => (
            <span
              key={p.provider}
              className={`kbc-home-card__intel-chip kbc-home-card__intel-chip--${p.alive ? "alive" : "dead"}`}
              title={`${p.provider} (${p.langs.join(", ")})${p.server_version ? ` v${p.server_version}` : ""} — ${
                p.alive ? "alive" : "not responding"
              }`}
              data-kbc-home-intel-provider={p.provider}
              data-kbc-home-intel-alive={p.alive}
            >
              <span className="kbc-home-card__intel-dot" aria-hidden="true" />
              {p.provider}
            </span>
          ))}
        </div>
      )}

      {continueReading.length > 0 && (
        <section className="kbc-home-card__section" data-kbc-home-continue>
          <h3 className="kbc-home-card__section-title">Continue reading</h3>
          <ul className="kbc-home-card__filelist">
            {continueReading.map((e) => {
              const { dir, base } = splitPath(e.path);
              return (
                <li key={e.path}>
                  <Link to={readerUrl(repo.name, e.path)} className="kbc-home-card__file">
                    {dir && <span className="kbc-home-card__file-dir">{dir}/</span>}
                    <span className="kbc-home-card__file-base">{base}</span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </section>
      )}

      <section className="kbc-home-card__section" data-kbc-home-recent>
        <h3 className="kbc-home-card__section-title">Recent files</h3>
        {recentFiles.isLoading ? (
          <SkeletonLines count={2} />
        ) : recentFiles.error ? (
          <p className="kbc-home-card__error">Couldn't load recent files</p>
        ) : recentFiles.data && recentFiles.data.hits.length > 0 ? (
          <ul className="kbc-home-card__filelist">
            {recentFiles.data.hits.map((hit) => {
              const { dir, base } = splitPath(hit.path);
              return (
                <li key={hit.path}>
                  <Link to={readerUrl(hit.repo, hit.path)} className="kbc-home-card__file">
                    {dir && <span className="kbc-home-card__file-dir">{dir}/</span>}
                    <span className="kbc-home-card__file-base">{base}</span>
                  </Link>
                </li>
              );
            })}
          </ul>
        ) : (
          <p className="kbc-home-card__muted">No files opened yet.</p>
        )}
      </section>

      <section className="kbc-home-card__section" data-kbc-home-branches>
        <h3 className="kbc-home-card__section-title">Active branches</h3>
        {branches.isLoading ? (
          <SkeletonLines count={2} />
        ) : branches.error ? (
          <p className="kbc-home-card__error">Couldn't load branches</p>
        ) : activeBranches && activeBranches.length > 0 ? (
          <ul className="kbc-home-card__branchlist">
            {activeBranches.map((b) => (
              <li className="kbc-home-card__branch-row" key={b.name} data-kbc-home-branch-row={b.name}>
                <Link to={readerUrl(repo.name, "", b.name)} className="kbc-home-card__branch-name">
                  {b.name}
                </Link>
                <span className="kbc-home-card__branch-chips">
                  <span className="kbc-home-card__chip" data-kbc-home-branch-ahead={b.ahead}>
                    <BranchAhead repo={repo.name} defaultBranch={branches.data!.default} branch={b} /> ahead
                  </span>
                  <span className="kbc-home-card__chip" data-kbc-home-branch-behind={b.behind}>
                    <BranchBehind repo={repo.name} defaultBranch={branches.data!.default} branch={b} /> behind
                  </span>
                </span>
                {(b.attribution.confidence === "trailer" || b.attribution.confidence === "exact") && (
                  <AttributionCard attribution={b.attribution} compact />
                )}
              </li>
            ))}
          </ul>
        ) : (
          <p className="kbc-home-card__muted">No other branches.</p>
        )}
      </section>

      <footer className="kbc-home-card__footer">
        <Link to={branchesUrl(repo.name)}>Branches</Link>
        <span aria-hidden="true">·</span>
        <Link to={`${codeBasePath(repo.name, "")}/~compare`}>Compare</Link>
        <span aria-hidden="true">·</span>
        <Link to={`${codeBasePath(repo.name, "")}/~todos`}>TODOs</Link>
        <span aria-hidden="true">·</span>
        <Link to={hotspotsUrl(repo.name)}>Hotspots</Link>
        <span aria-hidden="true">·</span>
        <Link to={reviewsUrl(repo.name)}>Reviews</Link>
        <span aria-hidden="true">·</span>
        <Link to={fullSearchUrl("", repo.name)}>Search in {repo.name}</Link>
      </footer>
    </div>
  );
}
