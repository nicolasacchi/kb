import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import type { BranchFactRow as Row, BranchView } from "../../api/types";
import { useCommandScope } from "../../commands/CommandRoot";
import { resolve as resolveCommand, tokenOf } from "../../commands/dispatch";
import { useBranchFacts, useSetBranchFavourite, useStartBranchReview } from "../../hooks/useBranchFacts";
import { useLoopback } from "../../hooks/useLoopback";
import { isTypingTarget } from "../../lib/isTypingTarget";
import {
  BRANCH_DENSITY_STORAGE_KEY,
  BRANCH_VIEWS,
  BRANCH_VIEW_HINTS,
  BRANCH_VIEW_LABELS,
  branchesPageUrl,
  cycleBranchView,
  parseBranchDensity,
  parseBranchesSearch,
  type BranchDensity,
} from "../../lib/branchViews";
import { compareUrl, reviewUrl } from "../../lib/codeUrl";
import type { BranchesHandlers } from "../../routes/branchesCommands";
import BranchFactRow from "./BranchFactRow";
import ConflictRadarPanel from "./ConflictRadarPanel";

// V75-M3 (D15) — `~branches` as VIEWS, not tabs.
//
// What makes this a "view" rather than a tab: the selection is the URL
// (`?view=`), the membership rules ride the RESPONSE (`rules.views`, and
// each row's own `reasons[]`), and the counts on the selector come off the
// wire (`view_counts`) rather than from a client-side filter. A tab bar
// that recomputed membership here would be a second answer to a question
// the daemon already answered — the shape D15 rejects.
//
// Two caveats this component RENDERS rather than hides:
//
//  * `view_counts.merged` is ancestry-only unless the patch-id probe ran,
//    and `rules.view_counts_note` says so. The selector shows the note on
//    the merged view rather than quietly displaying a number that the
//    merged view itself will disagree with.
//  * "Compare with common base" is loopback-only server-side, so the CTA is
//    ABSENT for a remote caller, never rendered-and-disabled (a disabled
//    row is a map of the mutation surface — `kbc-actions/1`'s rule).

export interface BranchViewsProps {
  repo: string;
}

const PAGE_LIMIT = 50;

export default function BranchViews({ repo }: BranchViewsProps) {
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const urlState = useMemo(() => parseBranchesSearch(searchParams), [searchParams]);
  const loopback = useLoopback();
  useCommandScope("branches");

  // The filter box is DEBOUNCED into the URL: every keystroke would
  // otherwise be a history entry and a fetch. The input's own value is
  // local; `?q=` is the committed one.
  const [draftQuery, setDraftQuery] = useState(urlState.q);
  useEffect(() => setDraftQuery(urlState.q), [urlState.q]);

  const [focusIdx, setFocusIdx] = useState(0);
  const [pendingReview, setPendingReview] = useState<string | null>(null);

  // D16 keeps a reading-density preference BROWSER-LOCAL. localStorage is
  // the home; `?density=` only mirrors it so a shared link carries it.
  const density: BranchDensity = urlState.density;
  const setDensity = useCallback(
    (d: BranchDensity) => {
      try {
        window.localStorage.setItem(BRANCH_DENSITY_STORAGE_KEY, d);
      } catch {
        // A privacy-mode browser with no storage is not an error here — the
        // URL still carries the choice for this navigation.
      }
      navigate(branchesPageUrl(repo, { ...urlState, radar: urlState.radar ?? undefined, density: d }));
    },
    [navigate, repo, urlState],
  );

  useEffect(() => {
    if (searchParams.get("density")) return;
    try {
      const stored = parseBranchDensity(window.localStorage.getItem(BRANCH_DENSITY_STORAGE_KEY));
      if (stored !== "comfortable") setDensity(stored);
    } catch {
      // See above.
    }
    // Once, on mount: a stored preference seeds the URL, never the reverse.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const facts = useBranchFacts(repo, {
    view: urlState.view,
    q: urlState.q,
    prefix: urlState.prefix,
    fav: urlState.fav,
    limit: PAGE_LIMIT,
  });
  const favourite = useSetBranchFavourite(repo);
  const startReview = useStartBranchReview(repo);

  const rows = facts.data?.rows ?? [];
  useEffect(() => setFocusIdx(0), [urlState.view, urlState.q, urlState.prefix, urlState.fav]);

  const go = useCallback(
    (patch: Partial<Parameters<typeof branchesPageUrl>[1]>) => {
      navigate(
        branchesPageUrl(repo, {
          ...urlState,
          radar: urlState.radar ?? undefined,
          ...patch,
        }),
      );
    },
    [navigate, repo, urlState],
  );

  const compareWithBase = useCallback(
    (row: Row) => {
      setPendingReview(row.name);
      startReview.mutate(
        { ref: row.name, base: "auto" },
        {
          onSettled: () => setPendingReview(null),
          onSuccess: (out) => navigate(reviewUrl(repo, out.review.id)),
        },
      );
    },
    [navigate, repo, startReview],
  );

  const focused: Row | undefined = rows[focusIdx];

  const handlers: BranchesHandlers = {
    "branches.next": () => setFocusIdx((i) => Math.min(i + 1, Math.max(rows.length - 1, 0))),
    "branches.prev": () => setFocusIdx((i) => Math.max(i - 1, 0)),
    "branches.compare": () => {
      // Through the builder, never hand-assembled — root CLAUDE.md #35, and
      // `nav/rawUrls.test.ts` is the gate. The three-dot form is the one
      // every review reads (`base...head`), so the key and the row's own
      // link resolve to the identical URL.
      if (focused) {
        navigate(compareUrl(repo, { from: focused.base.ref ?? "", to: focused.name, threeDot: true }));
      }
    },
    "branches.start-review": () => {
      if (focused && loopback) compareWithBase(focused);
    },
    "branches.view-next": () => go({ view: cycleBranchView(urlState.view, 1) }),
    "branches.view-prev": () => go({ view: cycleBranchView(urlState.view, -1) }),
    "branches.favourite": () => {
      if (focused) favourite.mutate({ ref: focused.full_ref, on: !focused.favourite });
    },
    "branches.radar": () => {
      const against = facts.data?.default ?? focused?.base.ref ?? null;
      go({ radar: urlState.radar ? undefined : (against ?? undefined) });
    },
    "branches.fold-prefix": () => {
      if (urlState.prefix) return go({ prefix: "" });
      const head = focused?.name.includes("/") ? `${focused.name.split("/")[0]}/` : "";
      if (head) go({ prefix: head });
    },
    "branches.density": () => setDensity(density === "compact" ? "comfortable" : "compact"),
  };

  function onKeyDown(e: React.KeyboardEvent) {
    // Guard 1, this surface's copy: never steal a key out of the filter box
    // or any other input (`CommandRoot`'s own first guard, and the reason a
    // bare `L` can be both a view cycle and a letter you type).
    if (isTypingTarget(e.target)) return;
    const cmd = resolveCommand(tokenOf(e), "branches");
    if (!cmd) return;
    const handler = (handlers as Record<string, (() => void) | undefined>)[cmd.id];
    if (!handler) return;
    e.preventDefault();
    handler();
  }

  const counts = facts.data?.view_counts ?? {};
  const rules = facts.data?.rules;

  return (
    // eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions
    <section
      className={`kbc-bviews kbc-bviews--${density}`}
      data-kbc-bviews
      data-kbc-bviews-view={urlState.view}
      data-kbc-bviews-density={density}
      onKeyDown={onKeyDown}
      tabIndex={-1}
      aria-label="branch views"
    >
      <div className="kbc-bviews__selector" role="tablist" aria-label="branch view">
        {BRANCH_VIEWS.map((v: BranchView) => (
          <button
            key={v}
            type="button"
            role="tab"
            aria-selected={urlState.view === v}
            className={`kbc-bviews__view ${urlState.view === v ? "kbc-bviews__view--on" : ""}`}
            data-kbc-bview={v}
            title={BRANCH_VIEW_HINTS[v]}
            onClick={() => go({ view: v })}
          >
            {BRANCH_VIEW_LABELS[v]}
            {counts[v] !== undefined && (
              <span className="kbc-bviews__count" data-kbc-bview-count={v}>
                {counts[v]}
              </span>
            )}
          </button>
        ))}
      </div>

      <div className="kbc-bviews__controls">
        <input
          type="search"
          className="kbc-bviews__filter"
          value={draftQuery}
          placeholder="Filter — or an atom: branch: touches: by: agent:"
          aria-label="filter branches"
          data-kbc-bviews-filter
          onChange={(e) => setDraftQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") go({ q: draftQuery });
          }}
          onBlur={() => go({ q: draftQuery })}
        />
        <button
          type="button"
          className={`kbc-bviews__toggle ${urlState.fav ? "kbc-bviews__toggle--on" : ""}`}
          aria-pressed={urlState.fav}
          data-kbc-bviews-fav
          onClick={() => go({ fav: !urlState.fav })}
        >
          ★ starred
        </button>
        <button
          type="button"
          className="kbc-bviews__toggle"
          data-kbc-bviews-density
          onClick={() => setDensity(density === "compact" ? "comfortable" : "compact")}
        >
          {density === "compact" ? "Comfortable" : "Compact"}
        </button>
        <button
          type="button"
          className={`kbc-bviews__toggle ${urlState.radar ? "kbc-bviews__toggle--on" : ""}`}
          aria-pressed={!!urlState.radar}
          // The radar merges every candidate against the DEFAULT branch,
          // which is a fact off the wire. Before it arrives there is no
          // target, so the button is disabled rather than clickable-and-
          // inert — a control that silently does nothing is the worse of
          // the two honest answers.
          disabled={!urlState.radar && !facts.data?.default}
          data-kbc-bviews-radar
          onClick={() => go({ radar: urlState.radar ? undefined : (facts.data?.default ?? undefined) })}
        >
          Conflict radar
        </button>
      </div>

      {facts.data && facts.data.prefixes.length > 0 && (
        <div className="kbc-bviews__prefixes" data-kbc-bviews-prefixes>
          {urlState.prefix && (
            <button type="button" className="kbc-chip" data-kbc-bviews-prefix-clear onClick={() => go({ prefix: "" })}>
              ✕ {urlState.prefix}
            </button>
          )}
          {facts.data.prefixes.map((p) => (
            <button
              key={p.prefix}
              type="button"
              className={`kbc-chip ${urlState.prefix === p.prefix ? "kbc-chip--on" : ""}`}
              data-kbc-bviews-prefix={p.prefix}
              onClick={() => go({ prefix: urlState.prefix === p.prefix ? "" : p.prefix })}
            >
              {p.prefix} <span className="kbc-bviews__count">{p.count}</span>
            </button>
          ))}
        </div>
      )}

      {facts.data?.diagnostics.map((d) => (
        <p key={`${d.token}:${d.message}`} className="kbc-bviews__diag" data-kbc-bviews-diag role="status">
          {d.message}
        </p>
      ))}
      {facts.data?.degraded.map((d) => (
        <p key={d.lane} className="kbc-bviews__diag" data-kbc-bviews-degraded={d.lane} role="status">
          {d.lane}: {d.reason}
        </p>
      ))}

      {urlState.radar && (
        <ConflictRadarPanel
          repo={repo}
          against={urlState.radar}
          q={urlState.q || undefined}
          onClose={() => go({ radar: undefined })}
        />
      )}

      {facts.isLoading && <p className="kbc-reader__hint">Loading branch facts…</p>}
      {facts.error && (
        <p className="kbc-reader__hint kbc-reader__hint--error" data-kbc-bviews-error>
          {(facts.error as Error).message}
        </p>
      )}

      {facts.data && (
        <>
          <ul className="kbc-bviews__rows" data-kbc-bviews-rows>
            {rows.map((row, i) => (
              <BranchFactRow
                key={row.full_ref}
                repo={repo}
                row={row}
                focused={i === focusIdx}
                compact={density === "compact"}
                onCompareWithBase={loopback && pendingReview === null ? compareWithBase : null}
                onToggleFavourite={(r) => favourite.mutate({ ref: r.full_ref, on: !r.favourite })}
                onFocus={() => setFocusIdx(i)}
              />
            ))}
          </ul>
          {rows.length === 0 && (
            <p className="kbc-reader__hint" data-kbc-bviews-empty>
              No branch is in the {BRANCH_VIEW_LABELS[urlState.view]} view
              {urlState.q ? ` for ${urlState.q}` : ""}.
            </p>
          )}
          {startReview.error && (
            <p className="kbc-reader__hint kbc-reader__hint--error" data-kbc-bviews-review-error>
              {(startReview.error as Error).message}
            </p>
          )}

          {rules && (
            <footer className="kbc-bviews__rules" data-kbc-bviews-rules>
              <p>
                <strong>views</strong> {rules.views}
              </p>
              <p>
                <strong>base</strong> {rules.base_note} — ladder{" "}
                {rules.base_ladder.join(" → ")}
              </p>
              <p data-kbc-bviews-stale-rule>
                <strong>stale</strong> {rules.stale.rule}
                {rules.stale.degraded_reason ? ` — NOT APPLIED: ${rules.stale.degraded_reason}` : ""}
              </p>
              <p data-kbc-bviews-merged-rule>
                <strong>merged</strong> {rules.merged.rule} — probed{" "}
                {rules.merged.patch_id_probed} of {rules.merged.patch_id_candidates} candidate(s),
                cap {rules.merged.patch_id_cap}
              </p>
              <p data-kbc-bviews-agent-rule>
                <strong>agent</strong> exact = {rules.agent.exact}; likely = {rules.agent.likely};{" "}
                never = {rules.agent.never}
              </p>
              {rules.touches && (
                <p data-kbc-bviews-touches-rule>
                  <strong>touches</strong> {rules.touches.path} — scanned {rules.touches.scanned} of{" "}
                  {rules.touches.candidates}, cap {rules.touches.cap}
                </p>
              )}
              <p data-kbc-bviews-counts-note>{rules.view_counts_note}</p>
            </footer>
          )}
        </>
      )}
    </section>
  );
}
