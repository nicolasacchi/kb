import { useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";
import type { CommentOut } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useComments, useCommentsSummary } from "../hooks/useComments";
import { readerUrl } from "../lib/breadcrumbs";
import {
  ACTIONABLE_STATES,
  dashboardQueryParams,
  freshnessCaption,
  type CommentsDashboardFilters,
} from "../lib/comments";
import "../styles/comments.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

const KIND_LABELS: Record<string, string> = {
  doc: "Doc",
  annotation: "Annotation",
  directive: "Directive",
  section: "Section",
  licence: "Licence",
  generated: "Generated",
  commented_code: "Commented-out code",
  prose: "Prose",
};

const STATE_LABELS: Record<string, string> = {
  drifted: "Drifted docs",
  aged: "Aged annotations",
  unreasoned: "Unreasoned suppressions",
  unknown: "Unknown",
  fresh: "Fresh",
  none: "No state",
};

const PAGE_LIMIT_STEP = 100;
const MAX_LIMIT = 500;

function CommentRow({ repo, c }: { repo: string; c: CommentOut }) {
  return (
    <li className="kbc-comments-dash__row" data-kbc-comments-dash-row>
      <Link
        to={readerUrl(repo, c.path, undefined, c.line_start)}
        className="kbc-comments-dash__link"
      >
        <span className="kbc-comments-dash__kind" data-kbc-comment-kind={c.kind}>
          {KIND_LABELS[c.kind] ?? c.kind}
        </span>
        {c.keyword && <span className="kbc-comments-dash__keyword">{c.keyword}</span>}
        <span className="kbc-comments-dash__path">
          {c.path}:{c.line_start}
        </span>
        <span className="kbc-comments-dash__text">{c.text}</span>
      </Link>
      {c.state.state !== "none" && (
        <span
          className={`kbc-comments-dash__state kbc-comments-dash__state--${c.state.state}`}
          title={freshnessCaption(c.state)}
          data-kbc-comments-dash-state={c.state.state}
        >
          {c.state.state}
        </span>
      )}
    </li>
  );
}

/// One ACTIONABLE lane's section, given its OWN already-fetched
/// `GET /api/comments?state=<lane>` result — fetched by the PARENT (three
/// sibling `useComments` calls, one per `ACTIONABLE_STATES` entry) rather
/// than by this component, so the parent can also compute "are all three
/// lanes genuinely empty" from the SAME real data (never `GET /api/
/// comments/summary`'s `by_state`, which covers only `aged`/`unreasoned` —
/// `drifted` needs `git blame` and is deliberately absent from it, so it
/// can never answer "is there anything actionable" on its own).
function ActionableSection({
  repo,
  state,
  query,
}: {
  repo: string;
  state: string;
  query: ReturnType<typeof useComments>;
}) {
  const rows = query.data?.comments ?? [];
  if (query.isLoading) {
    return (
      <section className="kbc-comments-dash__section" data-kbc-comments-dash-section={state}>
        <h2>{STATE_LABELS[state] ?? state}</h2>
        <p className="kbc-comments-dash__muted">Loading…</p>
      </section>
    );
  }
  if (rows.length === 0) return null;
  return (
    <section className="kbc-comments-dash__section" data-kbc-comments-dash-section={state}>
      <h2>
        {STATE_LABELS[state] ?? state}{" "}
        <span className="kbc-comments-dash__count">{query.data?.total ?? rows.length}</span>
      </h2>
      {query.data?.truncated && (
        <p className="kbc-comments-dash__muted" data-kbc-comments-dash-truncated={state}>
          Showing {rows.length} of {query.data.total}.
        </p>
      )}
      <ul className="kbc-comments-dash__list">
        {rows.map((c) => (
          <CommentRow key={`${c.path}:${c.line_start}:${c.kind}:${state}`} repo={repo} c={c} />
        ))}
      </ul>
    </section>
  );
}

/// V72-J2 (D8) — `/r/:repo/~comments`: comments/1's dashboard. DEFAULT is
/// the actionable slice (drifted docs · aged annotations · unreasoned
/// directives, `ACTIONABLE_STATES` — the exact three lanes `kb-code
/// comments audit` prints), each its own server-paged section with a true
/// total. Facet chips (kind, keyword, path prefix) narrow every lane's
/// server query at once; "show everything" swaps to ONE unfiltered,
/// server-paged list. `~todos` (unchanged) links forward to this page as
/// the richer, kind-aware replacement.
export default function Comments() {
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const [showEverything, setShowEverything] = useState(false);
  const [filters, setFilters] = useState<CommentsDashboardFilters>({
    kind: null,
    keyword: null,
    state: null,
    pathPrefix: null,
  });
  const [pathInput, setPathInput] = useState("");
  const [limit, setLimit] = useState(PAGE_LIMIT_STEP);

  const summary = useCommentsSummary(repo);

  const kindChips = useMemo(() => {
    const entries = Object.entries(summary.data?.by_kind ?? {});
    return entries.sort((a, b) => b[1] - a[1]);
  }, [summary.data]);
  const keywordChips = useMemo(() => {
    const entries = Object.entries(summary.data?.by_keyword ?? {});
    return entries.sort((a, b) => b[1] - a[1]);
  }, [summary.data]);

  const everythingQuery = useComments(
    showEverything ? dashboardQueryParams(repo, filters, filters.state, limit) : undefined,
  );

  // The three ACTIONABLE lanes — three EXPLICIT `useComments` calls (never a
  // `.map()` over `ACTIONABLE_STATES`, which would call a hook from inside a
  // loop) so the sections below render off ALREADY-FETCHED data and this
  // component can compute "are all three lanes genuinely empty" from that
  // SAME real data — never from `GET /api/comments/summary`'s `by_state`,
  // which excludes `drifted` entirely (it needs `git blame`, per that
  // route's own doc) and so can never answer that question on its own.
  const driftedQuery = useComments(
    showEverything ? undefined : dashboardQueryParams(repo, filters, "drifted", limit),
  );
  const agedQuery = useComments(
    showEverything ? undefined : dashboardQueryParams(repo, filters, "aged", limit),
  );
  const unreasonedQuery = useComments(
    showEverything ? undefined : dashboardQueryParams(repo, filters, "unreasoned", limit),
  );
  const actionableQueries: Record<string, ReturnType<typeof useComments>> = {
    drifted: driftedQuery,
    aged: agedQuery,
    unreasoned: unreasonedQuery,
  };
  const actionableLoading = ACTIONABLE_STATES.some((s) => actionableQueries[s].isLoading);
  const actionableAllEmpty =
    !actionableLoading && ACTIONABLE_STATES.every((s) => (actionableQueries[s].data?.comments.length ?? 0) === 0);

  function applyPathPrefix() {
    setFilters((f) => ({ ...f, pathPrefix: pathInput.trim() || null }));
  }

  const boundsCaption = summary.data
    ? `${summary.data.total} comments indexed · ${summary.data.state_basis.excluded_reason}`
    : null;

  return (
    <div className="kbc-comments-dash" id="main" data-kbc-comments-dash>
      <header className="kbc-comments-dash__head">
        <h1 className="kbc-comments-dash__title">Comments — {repo}</h1>
        <p className="kbc-comments-dash__hint">
          comments/1: doc · annotation · directive · section · licence · generated · commented-out
          code · prose.
        </p>
        {boundsCaption && <p className="kbc-comments-dash__bounds">{boundsCaption}</p>}
      </header>

      <div className="kbc-comments-dash__filters">
        <div className="kbc-comments-dash__chips" role="group" aria-label="kind filter">
          <button
            type="button"
            className={"kbc-comments-dash__chip" + (filters.kind === null ? " is-on" : "")}
            onClick={() => setFilters((f) => ({ ...f, kind: null }))}
            data-kbc-comments-dash-chip="kind-all"
          >
            All kinds
          </button>
          {kindChips.map(([kind, count]) => (
            <button
              key={kind}
              type="button"
              className={"kbc-comments-dash__chip" + (filters.kind === kind ? " is-on" : "")}
              onClick={() => setFilters((f) => ({ ...f, kind: f.kind === kind ? null : kind }))}
              data-kbc-comments-dash-chip={`kind-${kind}`}
            >
              {KIND_LABELS[kind] ?? kind}
              <span className="kbc-comments-dash__chip-n">{count}</span>
            </button>
          ))}
        </div>
        {keywordChips.length > 0 && (
          <div className="kbc-comments-dash__chips" role="group" aria-label="keyword filter">
            <button
              type="button"
              className={"kbc-comments-dash__chip" + (filters.keyword === null ? " is-on" : "")}
              onClick={() => setFilters((f) => ({ ...f, keyword: null }))}
              data-kbc-comments-dash-chip="keyword-all"
            >
              All keywords
            </button>
            {keywordChips.map(([kw, count]) => (
              <button
                key={kw}
                type="button"
                className={"kbc-comments-dash__chip" + (filters.keyword === kw ? " is-on" : "")}
                onClick={() => setFilters((f) => ({ ...f, keyword: f.keyword === kw ? null : kw }))}
                data-kbc-comments-dash-chip={`keyword-${kw}`}
              >
                {kw}
                <span className="kbc-comments-dash__chip-n">{count}</span>
              </button>
            ))}
          </div>
        )}
        <div className="kbc-comments-dash__row2">
          <input
            className="kbc-comments-dash__search"
            type="search"
            value={pathInput}
            onChange={(e) => setPathInput(e.target.value)}
            onBlur={applyPathPrefix}
            onKeyDown={(e) => {
              if (e.key === "Enter") applyPathPrefix();
            }}
            placeholder="Path prefix…"
            aria-label="path prefix filter"
            data-kbc-comments-dash-path
          />
          <label className="kbc-comments-dash__toggle">
            <input
              type="checkbox"
              checked={showEverything}
              onChange={(e) => setShowEverything(e.target.checked)}
              data-kbc-comments-dash-show-everything
            />
            Show everything
          </label>
          {showEverything && (
            <select
              className="kbc-comments-dash__state-select"
              value={filters.state ?? ""}
              onChange={(e) => setFilters((f) => ({ ...f, state: e.target.value || null }))}
              aria-label="state filter"
              data-kbc-comments-dash-state-select
            >
              <option value="">Any state</option>
              {["drifted", "aged", "unreasoned", "unknown", "fresh", "none"].map((s) => (
                <option key={s} value={s}>
                  {STATE_LABELS[s] ?? s}
                </option>
              ))}
            </select>
          )}
        </div>
      </div>

      {!showEverything && (
        <div className="kbc-comments-dash__sections">
          {ACTIONABLE_STATES.map((state) => (
            <ActionableSection key={state} repo={repo} state={state} query={actionableQueries[state]} />
          ))}
          {actionableAllEmpty && (
            <EmptyState
              icon={<Icon.Comment />}
              title="Nothing actionable"
              hint="No drifted docs, aged annotations, or unreasoned suppressions match the current filters. Toggle “show everything” to browse the full index."
            />
          )}
        </div>
      )}

      {showEverything && (
        <div className="kbc-comments-dash__sections" data-kbc-comments-dash-everything>
          {everythingQuery.isLoading && <div className="kbc-comments-dash__muted">Loading…</div>}
          {everythingQuery.error && (
            <div className="kbc-comments-dash__error">{(everythingQuery.error as Error).message}</div>
          )}
          {everythingQuery.data && everythingQuery.data.comments.length === 0 && (
            <EmptyState icon={<Icon.Comment />} title="No comments" hint="Nothing matches the current filters." />
          )}
          {everythingQuery.data && everythingQuery.data.comments.length > 0 && (
            <>
              <p className="kbc-comments-dash__muted">
                Showing {everythingQuery.data.comments.length} of {everythingQuery.data.total}
                {everythingQuery.data.scan.truncated && " (state filter scanned a bounded window — see notes)"}
              </p>
              {(everythingQuery.data.notes ?? []).map((n) => (
                <p key={n} className="kbc-comments-dash__note">
                  {n}
                </p>
              ))}
              <ul className="kbc-comments-dash__list">
                {everythingQuery.data.comments.map((c) => (
                  <CommentRow key={`${c.path}:${c.line_start}:${c.kind}`} repo={repo} c={c} />
                ))}
              </ul>
              {everythingQuery.data.truncated && (
                <button
                  type="button"
                  className="kbc-comments-dash__more"
                  disabled={limit >= MAX_LIMIT}
                  onClick={() => setLimit((l) => Math.min(MAX_LIMIT, l + PAGE_LIMIT_STEP))}
                  data-kbc-comments-dash-more
                >
                  Show more
                </button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
