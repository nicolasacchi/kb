import { useMemo } from "react";
import { Link } from "react-router";
import EmptyState from "../components/EmptyState";
import RepoCard from "../components/home/RepoCard";
import { Icon } from "../components/icons";
import { useIdentity } from "../hooks/useIdentity";
import { useRepos } from "../hooks/useRepos";
import { useUnifiedInbox } from "../hooks/useUnifiedInbox";
import { readerUrl } from "../lib/breadcrumbs";
import { continueLocations } from "../lib/continueLocations";
import { getRecentLocations } from "../lib/navHistory";
import { computeInboxBadge } from "../lib/unifiedInbox";
import "../styles/home.css";

/// V70-A3S (design doc §Decisions D23) — the repo-less "One Inbox" summary,
/// reusing `routes/Inbox.tsx`'s own `useUnifiedInbox` hook (no duplicated
/// fetch logic — same `GET /api/inbox` query, cached under the same
/// `["inbox", "unified"]` key, so landing here first and then clicking
/// through to `/~inbox` is a cache hit). Deliberately a COMPACT summary
/// (counts + one link per lane into the real inbox), not a re-render of
/// `Inbox.tsx`'s row lists — Home's job is "should I go look," not a
/// second copy of the inbox itself.
function HomeInboxSummary() {
  const inbox = useUnifiedInbox();
  const data = inbox.data;
  const badge = data
    ? computeInboxBadge({ reviews: data.reviews, annotations: data.annotations, kb: data.kb })
    : null;
  const kbDeskCount = data?.kb.available && data.kb.desk ? data.kb.desk.attention : null;
  const kbCommentsCount = data?.kb.available && data.kb.comments ? data.kb.comments.total_open : null;

  return (
    <section className="kbc-home__section" aria-label="Inbox summary" data-kbc-home-inbox-summary>
      <h2 className="kbc-home__section-title">
        <Link to="/~inbox">Inbox</Link>
        {badge !== null && badge > 0 && (
          <span className="kbc-home__section-badge" data-kbc-home-inbox-summary-badge>
            {badge}
          </span>
        )}
      </h2>
      {inbox.isLoading ? (
        <p className="kbc-home__hint">Loading inbox…</p>
      ) : inbox.error ? (
        <p className="kbc-home__hint">Couldn't load the inbox.</p>
      ) : data ? (
        <ul className="kbc-home__inbox-lanes" data-kbc-home-inbox-lanes>
          <li>
            <Link to="/~inbox" data-kbc-home-inbox-lane="reviews">
              {data.reviews.length} review{data.reviews.length === 1 ? "" : "s"} awaiting you
            </Link>
          </li>
          <li>
            <Link to="/~inbox" data-kbc-home-inbox-lane="annotations">
              {data.annotations.length} open question{data.annotations.length === 1 ? "" : "s"} in working trees
            </Link>
          </li>
          {kbDeskCount !== null && (
            <li>
              <Link to="/~inbox" data-kbc-home-inbox-lane="kb-desk">
                {kbDeskCount} on your kb desk
              </Link>
            </li>
          )}
          {kbCommentsCount !== null && (
            <li>
              <Link to="/~inbox" data-kbc-home-inbox-lane="kb-comments">
                {kbCommentsCount} open kb comment{kbCommentsCount === 1 ? "" : "s"}
              </Link>
            </li>
          )}
        </ul>
      ) : null}
    </section>
  );
}

/// V70-A3S (design doc §Decisions D23) — a repo-LESS "continue where you
/// left off," spanning every repo the operator has touched (unlike
/// `RepoCard`'s own per-repo "Continue reading" section, which reads the
/// SEPARATE sessionStorage working set scoped to one repo). Reads
/// `lib/navHistory.ts`'s browser-local location ring via
/// `lib/continueLocations.ts` — no new fetch, no new endpoint, same
/// "already-tracked client-side" posture Reader.tsx's own start-here card
/// (`ReaderStartCards`) documents for its per-repo equivalent.
function HomeContinueCard() {
  // Read once on mount (mirrors `ReaderStartCards`'s own `useMemo(() =>
  // getRecentFiles()…, [repo])` posture) — Home is a landing page, not a
  // live view that needs to react to jumps recorded elsewhere mid-session.
  const recent = useMemo(() => continueLocations(getRecentLocations()), []);
  if (recent.length === 0) return null;
  return (
    <section
      className="kbc-home__section"
      aria-label="Continue where you left off"
      data-kbc-home-continue-global
    >
      <h2 className="kbc-home__section-title">Continue where you left off</h2>
      <ul className="kbc-home__continue-list" data-kbc-home-continue-list>
        {recent.map((f) => (
          <li key={`${f.repo}\0${f.path}`}>
            <Link
              to={readerUrl(f.repo, f.path, undefined, f.line)}
              className="kbc-home__continue-row"
              data-kbc-home-continue-row={`${f.repo}:${f.path}`}
            >
              <span className="kbc-home__continue-repo">{f.repo}</span>
              <span className="kbc-home__continue-loc">
                {f.path}:{f.line}
              </span>
            </Link>
          </li>
        ))}
      </ul>
    </section>
  );
}

/// F4 — Home stops being a bare landing (F1's "pick a repo from the pill"
/// stub) and becomes kb-code's fleet dashboard: kb's own gallery-equivalent
/// (root CLAUDE.md's "fleet monitoring = SPA Settings" idiom, applied to a
/// single kb-code daemon's OWN configured repos rather than a fleet of
/// daemons). One card per repo (`components/home/RepoCard.tsx`) — each
/// card's branches/recent-files/working-set reads are independent React
/// Query consumers fired in parallel (no waterfall: a slow repo's fetch
/// never blocks another card from rendering its own data). V70-A3S adds
/// the two repo-LESS lanes above (`HomeInboxSummary`/`HomeContinueCard`,
/// design doc §Decisions D23's "One Inbox" landing + continue card).
export default function Home() {
  const repos = useRepos();
  const identity = useIdentity();
  const list = repos.data?.repos ?? [];
  const totals = list.reduce(
    (acc, r) => ({ files: acc.files + r.file_count, symbols: acc.symbols + r.symbol_count }),
    { files: 0, symbols: 0 },
  );

  return (
    <div className="kbc-home" id="main">
      <header className="kbc-home__head">
        <div className="kbc-home__head-row">
          <h1 className="kbc-home__title">kb-code</h1>
          {/* S2-A — unified inbox (design-s2.md §S2-A): repo-less, so Home
              (the one route with no active repo requirement) is its other
              documented entry point besides the TopBar Explore menu. */}
          <Link to="/~inbox" className="kbc-home__inbox-link" data-kbc-home-inbox>
            <Icon.List width={13} height={13} aria-hidden />
            Inbox
          </Link>
        </div>
        <p className="kbc-home__hint">
          Press <kbd>⌘K</kbd> to search across every repo.
        </p>
        {!repos.isLoading && !repos.error && list.length > 0 && (
          <p className="kbc-home__summary" data-kbc-home-summary>
            {list.length} repo{list.length === 1 ? "" : "s"} · {totals.files} file{totals.files === 1 ? "" : "s"} ·{" "}
            {totals.symbols} symbol{totals.symbols === 1 ? "" : "s"}
          </p>
        )}
        {/* S2-B (design-s2.md §S2-B) — `remote_mutations` is OPTIONAL on the
         * wire (absent on a daemon built before it landed); the chip only
         * renders once the field is actually present, never a fabricated
         * "off" for an older daemon that simply doesn't know the concept. */}
        {identity.data?.remote_mutations !== undefined && (
          <p
            className="kbc-home__capability"
            data-kbc-home-remote-mutations={identity.data.remote_mutations}
          >
            Remote review mutations: {identity.data.remote_mutations ? "on" : "off"}
          </p>
        )}
      </header>

      <div className="kbc-home__lanes" data-kbc-home-lanes>
        <HomeInboxSummary />
        <HomeContinueCard />
      </div>

      {repos.isLoading ? (
        <div className="kbc-reader__hint">Loading repos…</div>
      ) : repos.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(repos.error as Error).message}</div>
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
  );
}
