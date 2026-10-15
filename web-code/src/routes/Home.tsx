import { StateCard } from "@kb/ui/StateCard";
import { Link } from "react-router";
import DaemonStatus from "../components/home/DaemonStatus";
import HomeContinue from "../components/home/HomeContinue";
import HomeNeedsYou from "../components/home/HomeNeedsYou";
import RepoCard from "../components/home/RepoCard";
import EmptyState from "../components/EmptyState";
import MetaLine from "../components/MetaLine";
import PageHeader from "../components/PageHeader";
import { Icon } from "../components/iconSet";
import { useRepos } from "../hooks/useRepos";
import "../styles/home.css";

/// OL-C7 — Home is a place, not a status page. Top to bottom: where you were
/// (the Continue hero, with real code under it), what needs you (only the
/// non-empty inbox lanes), then your repos. Diagnostics (review store, fetch
/// credential, remote-mutation flag) live on the Settings page; Home keeps a
/// one-line "Daemon status -> Settings" link at the bottom.
export default function Home() {
  const repos = useRepos();
  const list = repos.data?.repos ?? [];
  const catchingUp = list.filter((r) => r.catching_up).map((r) => r.name);
  const totals = list.reduce(
    (acc, r) => ({ files: acc.files + r.file_count, symbols: acc.symbols + r.symbol_count }),
    { files: 0, symbols: 0 },
  );
  const showTotals = !repos.isLoading && !repos.error && list.length > 0;

  return (
    <div className="kbc-home" id="main">
      <PageHeader
        title="kb-code"
        lede={
          <>
            Press <kbd>⌘K</kbd> to search across every repo.
          </>
        }
        actions={
          // S2-A — the unified inbox's second documented entry point
          // besides the TopBar inbox icon link.
          <Link to="/~inbox" className="kbc-home__inbox-link" data-kbc-home-inbox>
            <Icon.List width={13} height={13} aria-hidden />
            Inbox
          </Link>
        }
      />
      <MetaLine
        className="kbc-home__meta"
        items={[
          showTotals && (
            <span data-kbc-home-summary>
              {list.length} repo{list.length === 1 ? "" : "s"} · {totals.files} file
              {totals.files === 1 ? "" : "s"} · {totals.symbols} symbol{totals.symbols === 1 ? "" : "s"}
            </span>
          ),
        ]}
      />

      <div className="kbc-home__lanes" data-kbc-home-lanes>
        <HomeContinue />
        <HomeNeedsYou />
      </div>

      <div className="kbc-home__section kbc-home__section--repos">
        <h2 className="kbc-home__label">Repos</h2>
        {catchingUp.length > 0 && (
          <StateCard
            kind="degraded"
            title="Indexing the mirror…"
            reason={`Still indexing ${catchingUp.join(", ")}. Search results may be incomplete until it settles.`}
            headingLevel={3}
          />
        )}
        {repos.isLoading ? (
          <StateCard kind="loading" title="Loading repos…" />
        ) : repos.error ? (
          <StateCard
            kind="degraded"
            title="Couldn't load repos."
            reason={(repos.error as Error).message}
            headingLevel={3}
          />
        ) : list.length === 0 ? (
          <EmptyState
            icon={<Icon.Folder />}
            title="No repos configured"
            hint={
              <>
                Add one under <code>[[repos]]</code> in <code>kb-code.toml</code>, then restart the daemon.
              </>
            }
          />
        ) : (
          <div className="kbc-home__grid" data-kbc-home-grid>
            {list.map((r) => (
              <RepoCard key={r.name} repo={r} />
            ))}
          </div>
        )}
      </div>

      {list.length > 0 && <DaemonStatus />}
    </div>
  );
}
