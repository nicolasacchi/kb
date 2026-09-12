import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { ApiError, createSet, postRecipeMaterialise, postRecipeTrust } from "../api/client";
import type { KbcRunOut, KbcStepRun } from "../api/types";
import type { RecipeCatalogEntry, RecipeItem } from "../api/types";
import EmptyState from "../components/EmptyState";
import ErrorBoundary from "../components/ErrorBoundary";
import { Icon } from "../components/icons";
import RecipeAutoForm from "../components/recipes/RecipeAutoForm";
import RecipeHome from "../components/recipes/RecipeHome";
import RecipeResultViews from "../components/recipes/RecipeResultViews";
import RecipeTrustBadge from "../components/recipes/RecipeTrustBadge";
import CensusPanel from "../components/recipes/CensusPanel";
import {
  useRecipeCatalog,
  useRecipeReplay,
  useRecipeRunV2,
  useRecipeShow,
} from "../hooks/useRecipes";
import { useLoopback } from "../hooks/useLoopback";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";
import { getRecentLocations } from "../lib/navHistory";
import { addrHref, addrsToSetSpans } from "../lib/recipeAddr";
import {
  emptyRecipeUrlState,
  parseRecipeSearch,
  recipeCliLine,
  recipeReplayUrl,
  recipeRunUrl,
  recipeValidateAll,
  type ParamFieldError,
} from "../lib/recipeUrl";
import {
  parseRecipesSearch,
  recipeRequiresSince,
  recipesUrl,
} from "../lib/recipesUrl";
import { useRecipeRun, useRecipesCatalog } from "../hooks/useRecipes";
import { formatAttentionScore } from "../lib/attentionRamp";
import { setUrl } from "../lib/setsUrl";
import { readerUrl } from "../lib/breadcrumbs";
import { ClassBadge } from "../components/hierarchy/HierarchyPanel";
import { copyToClipboard } from "../editor/vimReader";
import { toast } from "../lib/toast";
import { tokenOf, resolve as resolveCommand } from "../commands/dispatch";
import { useCommandScope } from "../commands/CommandRoot";
import type { RecipeHandlers } from "./recipeCommands";
import "../styles/recipes.css";

const LEGACY_LIMIT_OPTIONS = [25, 50, 100, 250, 500] as const;

/// Ported verbatim from the pre-L3c `Recipes.tsx` — label for legacy catalog
/// entries, never paraphrase e.g. god-functions as "god".
function legacyRecipePageLabel(entry: RecipeCatalogEntry): string {
  const d = entry.description.trim();
  const cut = d.indexOf(".");
  return cut > 0 ? d.slice(0, cut) : d || entry.name;
}

function legacyTermsEntries(terms: RecipeItem["terms"]): Array<[string, string]> {
  if (!terms || typeof terms !== "object") return [];
  return Object.entries(terms).map(([k, v]) => [
    k,
    v == null ? "—" : typeof v === "number" ? String(v) : String(v),
  ]);
}

function legacyItemKey(item: RecipeItem, i: number): string {
  return `${item.path ?? ""}:${item.symbol ?? ""}:${item.line ?? ""}:${i}`;
}

/// V74-L3c — `~recipes` reworked as the recipe HOME (D11). Old `recipes/1`
/// deep links (`?recipe=&since=&limit=`) keep resolving unchanged — see
/// `LegacyRecipesPanel` below for the coexistence decision. New links use a
/// completely disjoint query-key set (`?slug=&p.<name>=&ctx.<field>=&scope=
/// &limit=&view=`, `lib/recipeUrl.ts`), so the two grammars never collide on
/// one URL.
export default function Recipes() {
  useListScrollRestoration();
  const { repo = "", id: replayId } = useParams<{ repo: string; id?: string }>();
  const [searchParams] = useSearchParams();

  const urlState = useMemo(() => parseRecipeSearch(searchParams), [searchParams]);
  const legacyUrlState = useMemo(() => parseRecipesSearch(searchParams), [searchParams]);

  useCommandScope("recipe");

  if (replayId) {
    return <RecipeReplay repo={repo} id={replayId} />;
  }

  if (!urlState.slug) {
    return (
      <div className="kbc-recipes kbc-recipe-page" id="main" data-kbc-recipes>
        <header className="kbc-recipes__head">
          <h1 className="kbc-recipes__title">Recipes — {repo}</h1>
          <p className="kbc-recipes__hint" data-kbc-recipes-hint>
            Named, parameterised questions over this repo's own indexes — pick one below, or run a
            legacy attention query further down.
          </p>
        </header>
        <RecipeHomeSection repo={repo} />
        <LegacyRecipesPanel repo={repo} legacyUrlState={legacyUrlState} />
      </div>
    );
  }

  return <RecipeRunPage repo={repo} slug={urlState.slug} />;
}

function RecipeHomeSection({ repo }: { repo: string }) {
  const catalogQ = useRecipeCatalog(repo);
  return (
    <section data-kbc-recipe-home-section>
      {catalogQ.isLoading && <div className="kbc-recipes__muted">Loading catalog…</div>}
      {catalogQ.error && (
        <div className="kbc-recipes__error">{(catalogQ.error as Error).message}</div>
      )}
      {catalogQ.data && <RecipeHome repo={repo} recipes={catalogQ.data.recipes} />}
      {catalogQ.data && catalogQ.data.problems.length > 0 && (
        <div className="kbc-recipes__banner" role="status" data-kbc-recipe-load-problems>
          {catalogQ.data.problems.length} repo recipe file(s) failed to load:{" "}
          {catalogQ.data.problems.map((p) => `${p.path} (${p.message})`).join("; ")}
        </div>
      )}
    </section>
  );
}

// ── the FROZEN legacy recipes/1 panel (byte-compatible with the pre-L3c UI) ──
//
// Coexistence decision (this unit): `~recipes` becomes the home page above;
// the six `recipes/1` recipes now ALSO run on the new runner (as native
// `home: "builtin"` adapters in the catalog, same slugs), so this panel is
// no longer the only way to reach them — but it stays, ALWAYS mounted as a
// plain page section below the new home, for (a) any bookmarked
// `?recipe=&since=&limit=` deep link, which this component still parses
// unchanged, and (b) the frozen `GET /api/recipes` wire itself, unchanged
// per docs/kb-code.md. A user who wants the richer runner (typed form, four
// views, census) picks the same recipe from the "Repo hygiene"/"I'm
// reviewing a PR"/etc. sections above instead.
/// Always mounted (NOT a collapsed `<details>`) — "a section of it", not a
/// hidden fallback, so `e2e/recipes.spec.ts`'s pre-existing assertions (the
/// catalog visible with no `?recipe=` at all) keep passing unchanged; the
/// new intent-groups home above it is what makes this section feel
/// secondary, not a disclosure toggle hiding it.
function LegacyRecipesPanel({
  repo,
  legacyUrlState,
}: {
  repo: string;
  legacyUrlState: ReturnType<typeof parseRecipesSearch>;
}) {
  return (
    <section className="kbc-recipes__legacy" data-kbc-recipes-legacy>
      <h2 className="kbc-recipes__legacy-title">Legacy recipes (recipes/1 — the original six, unchanged)</h2>
      <LegacyRecipesBody repo={repo} legacyUrlState={legacyUrlState} />
    </section>
  );
}

/// A near-byte-faithful port of the pre-L3c `Recipes.tsx` run panel (see
/// `git show origin/main:web-code/src/routes/Recipes.tsx`) — every
/// `data-kbc-recipes-*` attribute the EXISTING `e2e/recipes.spec.ts` and
/// any external bookmark/script rely on stays put, so this coexistence
/// decision doesn't quietly break either. The only changes from the
/// original: it renders as a plain page SECTION below the new home instead
/// of owning the whole page, and it reads `legacyUrlState`/navigates through
/// `recipesUrl` (both unchanged, `lib/recipesUrl.ts`) instead of the
/// page-level `urlState` the original had direct access to.
function LegacyRecipesBody({
  repo,
  legacyUrlState,
}: {
  repo: string;
  legacyUrlState: ReturnType<typeof parseRecipesSearch>;
}) {
  const navigate = useNavigate();
  const catalogQ = useRecipesCatalog();
  const recipes = catalogQ.data?.recipes ?? [];

  const selectedName = legacyUrlState.recipe ?? "";
  const selected = recipes.find((r) => r.name === selectedName) ?? null;
  const needsSince = selected ? recipeRequiresSince(selected.params) : false;

  const [sinceDraft, setSinceDraft] = useState(legacyUrlState.since ?? "");
  const [limitDraft, setLimitDraft] = useState<number>(legacyUrlState.limit ?? 50);
  const [expanded, setExpanded] = useState<string | null>(null);

  useEffect(() => {
    setSinceDraft(legacyUrlState.since ?? "");
    if (legacyUrlState.limit) setLimitDraft(legacyUrlState.limit);
  }, [legacyUrlState.since, legacyUrlState.limit, legacyUrlState.recipe]);

  const canAutoRun =
    !!selectedName && (!needsSince || !!(legacyUrlState.since && legacyUrlState.since.length > 0));

  const runQ = useRecipeRun(
    canAutoRun
      ? {
          name: selectedName,
          repo,
          since: legacyUrlState.since,
          limit: legacyUrlState.limit ?? limitDraft,
        }
      : undefined,
    canAutoRun,
  );

  function selectRecipe(name: string) {
    setExpanded(null);
    navigate(recipesUrl(repo, { recipe: name, since: sinceDraft || undefined, limit: limitDraft }));
  }

  function onRun() {
    if (!selectedName) return;
    setExpanded(null);
    navigate(
      recipesUrl(repo, {
        recipe: selectedName,
        since: sinceDraft.trim() || undefined,
        limit: limitDraft,
      }),
    );
  }

  const items = runQ.data?.items ?? [];
  const hasSymbolCol = items.some((it) => it.symbol != null);
  const hasScoreCol = items.some((it) => it.score != null);
  const hasClassCol = items.some((it) => it.class != null);

  return (
    <div className="kbc-recipes__layout" data-kbc-recipes-legacy-body>
      <aside className="kbc-recipes__catalog" data-kbc-recipes-catalog>
        <h2 className="kbc-recipes__sub">Catalog</h2>
        {catalogQ.isLoading && <div className="kbc-recipes__muted">Loading…</div>}
        {catalogQ.error && (
          <div className="kbc-recipes__error">{(catalogQ.error as Error).message}</div>
        )}
        <ul className="kbc-recipes__list">
          {recipes.map((r) => {
            const active = r.name === selectedName;
            return (
              <li key={r.name}>
                <button
                  type="button"
                  className={
                    "kbc-recipes__cat-item" + (active ? " kbc-recipes__cat-item--active" : "")
                  }
                  onClick={() => selectRecipe(r.name)}
                  data-kbc-recipes-cat={r.name}
                  aria-current={active ? "true" : undefined}
                >
                  <span className="kbc-recipes__cat-name" data-kbc-recipes-cat-name>
                    {r.name}
                  </span>
                  <span className="kbc-recipes__cat-desc">{legacyRecipePageLabel(r)}</span>
                  {r.needs.length > 0 && (
                    <span className="kbc-recipes__cat-needs" data-kbc-recipes-needs>
                      needs: {r.needs.join(", ")}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      </aside>

      <section className="kbc-recipes__run" data-kbc-recipes-run>
        {!selected ? (
          <EmptyState
            icon={<Icon.List />}
            title="Pick a recipe"
            hint="Select a catalog entry to configure and run it."
          />
        ) : (
          <>
            <h2 className="kbc-recipes__sub" data-kbc-recipes-selected={selected.name}>
              {selected.name}
            </h2>
            <p className="kbc-recipes__desc" data-kbc-recipes-desc>
              {selected.description}
            </p>

            <div className="kbc-recipes__controls">
              {needsSince && (
                <label className="kbc-recipes__lab">
                  since
                  <input
                    className="kbc-recipes__input"
                    type="text"
                    value={sinceDraft}
                    onChange={(e) => setSinceDraft(e.target.value)}
                    placeholder="YYYY-MM-DD or git ref"
                    aria-label="since date or ref"
                    data-kbc-recipes-since
                  />
                  <span className="kbc-recipes__field-hint">
                    Required: git ref or ISO date (YYYY-MM-DD)
                  </span>
                </label>
              )}
              <label className="kbc-recipes__lab">
                Limit
                <select
                  className="kbc-recipes__limit"
                  value={limitDraft}
                  onChange={(e) => setLimitDraft(Number(e.target.value))}
                  aria-label="recipe limit"
                  data-kbc-recipes-limit
                >
                  {LEGACY_LIMIT_OPTIONS.map((n) => (
                    <option key={n} value={n}>
                      {n}
                    </option>
                  ))}
                </select>
              </label>
              <button
                type="button"
                className="kbc-recipes__go"
                onClick={onRun}
                data-kbc-recipes-run-btn
                disabled={needsSince && !sinceDraft.trim()}
              >
                Run
              </button>
            </div>

            {runQ.isLoading && <div className="kbc-recipes__muted">Running…</div>}
            {runQ.error && (
              <div className="kbc-recipes__error" data-kbc-recipes-error>
                {runQ.error instanceof ApiError
                  ? runQ.error.message
                  : (runQ.error as Error).message}
              </div>
            )}

            {runQ.data?.inputs_missing && runQ.data.inputs_missing.length > 0 && (
              <div className="kbc-recipes__banner" role="status" data-kbc-recipes-missing>
                not computed: {runQ.data.inputs_missing.join(", ")}
              </div>
            )}
            {runQ.data?.note && (
              <div className="kbc-recipes__banner kbc-recipes__banner--note" role="status" data-kbc-recipes-note>
                {runQ.data.note}
              </div>
            )}
            {runQ.data?.truncated && (
              <div className="kbc-recipes__banner" role="status" data-kbc-recipes-trunc>
                showing the capped set ({runQ.data.items.length} of {runQ.data.total})
              </div>
            )}

            {runQ.isSuccess
              && items.length === 0
              && (runQ.data?.inputs_missing?.length ?? 0) === 0 && (
              <EmptyState
                icon={<Icon.List />}
                title="No rows"
                hint="This recipe returned an empty result set for the current inputs."
              />
            )}

            {items.length > 0 && (
              <div className="kbc-recipes__table-wrap">
                <table className="kbc-recipes__table" data-kbc-recipes-table>
                  <thead>
                    <tr>
                      <th>Path</th>
                      {hasSymbolCol && <th>Symbol</th>}
                      {hasScoreCol && <th>Score</th>}
                      {hasClassCol && <th>Class</th>}
                      <th>Terms</th>
                    </tr>
                  </thead>
                  <tbody>
                    {items.map((item, i) => {
                      const key = legacyItemKey(item, i);
                      const open = expanded === key;
                      const terms = legacyTermsEntries(item.terms);
                      return (
                        <tr key={key} data-kbc-recipes-row={key}>
                          <td>
                            {item.path ? (
                              <Link
                                to={readerUrl(
                                  repo,
                                  item.path,
                                  undefined,
                                  typeof item.line === "number" ? item.line : undefined,
                                )}
                                className="kbc-recipes__path"
                                data-kbc-recipes-path
                              >
                                {item.path}
                              </Link>
                            ) : (
                              "—"
                            )}
                          </td>
                          {hasSymbolCol && (
                            <td data-kbc-recipes-symbol>{item.symbol ?? "—"}</td>
                          )}
                          {hasScoreCol && (
                            <td data-kbc-recipes-score>
                              {item.score != null
                                ? formatAttentionScore(Number(item.score))
                                : "—"}
                            </td>
                          )}
                          {hasClassCol && (
                            <td>
                              {item.class ? (
                                <ClassBadge className={String(item.class)} />
                              ) : (
                                "—"
                              )}
                            </td>
                          )}
                          <td>
                            {terms.length > 0 ? (
                              <>
                                <button
                                  type="button"
                                  className={
                                    "kbc-recipes__terms-btn" + (open ? " is-open" : "")
                                  }
                                  onClick={() =>
                                    setExpanded((cur) => (cur === key ? null : key))
                                  }
                                  aria-expanded={open}
                                  data-kbc-recipes-terms-btn
                                >
                                  terms {open ? "▾" : "▸"}
                                </button>
                                {open && (
                                  <dl className="kbc-recipes__terms" data-kbc-recipes-terms>
                                    {terms.map(([k, v]) => (
                                      <div key={k}>
                                        <dt>{k}</dt>
                                        <dd>{v}</dd>
                                      </div>
                                    ))}
                                  </dl>
                                )}
                              </>
                            ) : (
                              <span className="kbc-recipes__muted">—</span>
                            )}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </>
        )}
      </section>
    </div>
  );
}

function RecipeReplay({ repo, id }: { repo: string; id: string }) {
  const replayQ = useRecipeReplay(id);
  // View-switcher + census-detail state, same shape `RecipeRunPage` owns
  // for a live run — WITHOUT this, `RecipeResultViews`' tabs would render
  // but do nothing on click (there is no parent state for them to flip),
  // since only `CensusPanel` has its own local-state fallback for an
  // unmanaged `onToggleExpanded`.
  const [activeViewByStep, setActiveViewByStep] = useState<Record<string, string>>({});
  const [censusExpanded, setCensusExpanded] = useState<Record<string, boolean>>({});

  if (replayQ.isLoading) return <div className="kbc-recipes__muted">Loading replay…</div>;
  if (replayQ.error) {
    return (
      <div className="kbc-recipes kbc-recipe-page" id="main">
        <EmptyState
          icon={<Icon.Warn />}
          title="Couldn't load this run"
          hint={replayQ.error instanceof ApiError ? replayQ.error.message : String(replayQ.error)}
        />
        <Link to={recipeRunUrl(repo)} className="kbc-recipes__back-link">
          ← Back to recipes
        </Link>
      </div>
    );
  }
  if (!replayQ.data) return null;
  return (
    <div className="kbc-recipes kbc-recipe-page" id="main" data-kbc-recipes>
      <RunHeader repo={repo} run={replayQ.data} />
      {replayQ.data.replay && (
        <div className="kbc-recipes__banner" role="status" data-kbc-recipe-replay-banner>
          Materialised {new Date(replayQ.data.replay.created_unix * 1000).toLocaleString()}
          {replayQ.data.replay.stale && " — the corpus has re-indexed since (stale)"}
        </div>
      )}
      {/* A render bug in one step's view (a shape this unit didn't
          anticipate from a real corpus) must not blank the header/replay
          banner above it — isolate the results half in its OWN boundary,
          same component `app.tsx` already uses at the route level. */}
      <ErrorBoundary resetKey={replayQ.data.recipe}>
        <RunHonesty run={replayQ.data} />
        <RunSteps
          repo={repo}
          run={replayQ.data}
          activeViewByStep={activeViewByStep}
          onSelectView={(stepId, viewId) => setActiveViewByStep((m) => ({ ...m, [stepId]: viewId }))}
          censusExpanded={censusExpanded}
          onToggleCensus={(stepId) => setCensusExpanded((m) => ({ ...m, [stepId]: !m[stepId] }))}
        />
      </ErrorBoundary>
    </div>
  );
}

function RunHeader({ repo, run }: { repo: string; run: KbcRunOut }) {
  return (
    <header className="kbc-recipes__head">
      <h1 className="kbc-recipes__title" data-kbc-recipe-run-title={run.recipe}>
        {run.title}
      </h1>
      <p className="kbc-recipes__hint">
        <RecipeTrustBadge state={run.trust} /> · {run.home} · {run.repo}
      </p>
      <Link to={recipeRunUrl(repo)} className="kbc-recipes__back-link">
        ← All recipes
      </Link>
    </header>
  );
}

/// The auto-form + run + result-views page for one selected `kbc-recipe/1`
/// slug (`?slug=`). Local DRAFT state (params/scope/ctx) drives the
/// live-updating, copyable CLI line; an explicit Run commits the draft to
/// the URL, which is what the actual `GET …/run` query is keyed on — same
/// draft/committed split the old runner used (`sinceDraft`/`limitDraft`
/// above), so typing never fires a real (budgeted, server-side) run.
function RecipeRunPage({ repo, slug }: { repo: string; slug: string }) {
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const loopback = useLoopback();
  const urlState = useMemo(() => parseRecipeSearch(searchParams), [searchParams]);

  const showQ = useRecipeShow(slug, repo);
  const spec = showQ.data?.recipe;

  const [draftParams, setDraftParams] = useState<Record<string, string>>(urlState.params);
  const [draftScope, setDraftScope] = useState(urlState.scope ?? "");
  const [draftCtx, setDraftCtx] = useState<Record<string, string>>(urlState.ctx);
  const [activeViewByStep, setActiveViewByStep] = useState<Record<string, string>>({});
  const [focusedStepIdx, setFocusedStepIdx] = useState(0);
  const [focusedRowIdx, setFocusedRowIdx] = useState(0);
  const [censusExpanded, setCensusExpanded] = useState<Record<string, boolean>>({});

  const lastLoc = useMemo(
    () => getRecentLocations(1).find((l) => l.repo === repo),
    [repo],
  );
  const ctxAutoDetected = useMemo<Record<string, string>>(() => {
    const out: Record<string, string> = {};
    if (lastLoc) out.path = lastLoc.path;
    return out;
  }, [lastLoc]);

  // Re-seed drafts when the SLUG changes (a different recipe selected) —
  // never when only some other url field changes, so live typing is never
  // clobbered by our own committed-state effects.
  useEffect(() => {
    setDraftParams(urlState.params);
    setDraftScope(urlState.scope ?? "");
    setDraftCtx({ ...ctxAutoDetected, ...urlState.ctx });
    setFocusedStepIdx(0);
    setFocusedRowIdx(0);
    setActiveViewByStep({});
    setCensusExpanded({});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [slug]);

  const trust = spec?.trust;
  const trustBlocksRun = trust === "untrusted" || trust === "changed";

  const runQuery = trustBlocksRun
    ? undefined
    : { slug, repo, scope: urlState.scope, limit: urlState.limit, params: urlState.params, ctx: urlState.ctx };
  const runQ = useRecipeRunV2(runQuery, !!urlState.slug);
  const run = runQ.data;

  useEffect(() => {
    setFocusedStepIdx(0);
    setFocusedRowIdx(0);
  }, [run]);

  const errors: ParamFieldError[] = spec ? recipeValidateAll(spec.params ?? [], draftParams) : [];
  const serverFieldErrors = useMemo(() => {
    if (!(runQ.error instanceof ApiError) || runQ.error.status !== 400) return undefined;
    const out: Record<string, string> = {};
    for (const p of spec?.params ?? []) {
      if (runQ.error.message.includes(p.name)) out[p.name] = runQ.error.message;
    }
    return Object.keys(out).length > 0 ? out : undefined;
  }, [runQ.error, spec]);

  const cliLine = recipeCliLine({
    slug,
    repo,
    scope: draftScope || undefined,
    limit: urlState.limit,
    params: draftParams,
    ctx: draftCtx,
  });

  function commitAndRun() {
    navigate(
      recipeRunUrl(repo, {
        ...emptyRecipeUrlState(),
        slug,
        scope: draftScope || undefined,
        limit: urlState.limit,
        view: urlState.view,
        params: draftParams,
        ctx: draftCtx,
      }),
    );
  }

  const steps = run?.steps ?? [];
  const focusedStep: KbcStepRun | undefined = steps[focusedStepIdx];
  const viewsForFocusedStep = run?.views.filter((v) => v.step === focusedStep?.id) ?? [];

  function cycleView(delta: number) {
    if (!focusedStep || viewsForFocusedStep.length === 0) return;
    const currentId = activeViewByStep[focusedStep.id] ?? viewsForFocusedStep[0].id;
    const idx = viewsForFocusedStep.findIndex((v) => v.id === currentId);
    const next = viewsForFocusedStep[(idx + delta + viewsForFocusedStep.length) % viewsForFocusedStep.length];
    setActiveViewByStep((m) => ({ ...m, [focusedStep.id]: next.id }));
  }

  async function doMaterialise() {
    try {
      const out = await postRecipeMaterialise({
        slug,
        repo,
        scope: draftScope || undefined,
        limit: urlState.limit,
        params: draftParams,
        ctx: draftCtx,
      });
      navigate(recipeReplayUrl(repo, out.run_id));
    } catch (e) {
      toast.err(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function doTrust() {
    try {
      await postRecipeTrust(slug, repo);
      showQ.refetch();
    } catch (e) {
      toast.err(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function doSaveAsSet() {
    if (!focusedStep) return;
    const view = viewsForFocusedStep.find((v) => v.id === (activeViewByStep[focusedStep.id] ?? viewsForFocusedStep[0]?.id));
    const rows = focusedStep.rows;
    const { spans, skipped } = addrsToSetSpans(rows);
    if (spans.length === 0) {
      toast.warn("Nothing in this step's rows can become a reading-set span (no path).");
      return;
    }
    const name = window.prompt(
      "Save this step's rows as a reading set:",
      `${slug} — ${view?.title ?? focusedStep.title ?? focusedStep.id}`,
    );
    if (name === null) return;
    try {
      const set = await createSet({ repo, name, spans });
      toast.ok(skipped > 0 ? `Saved (${skipped} row(s) skipped — no path)` : "Saved as a reading set", {
        label: "open",
        to: setUrl(repo, set.id),
      });
    } catch (e) {
      toast.err(e instanceof ApiError ? e.message : String(e));
    }
  }

  function doCopyCli() {
    copyToClipboard(cliLine);
    toast.ok(`Copied: ${cliLine}`);
  }

  function openFocusedRow() {
    if (!focusedStep) return;
    const addr = focusedStep.rows[focusedRowIdx];
    if (!addr) return;
    // The SAME mapping `AddressCell` renders every link through — a
    // keyboard `Enter` and a mouse click resolve to the identical URL.
    const href = addrHref(repo, addr);
    if (href) navigate(href);
  }

  const handlers: RecipeHandlers = {
    "recipe.run": commitAndRun,
    "recipe.view-next": () => cycleView(1),
    "recipe.view-prev": () => cycleView(-1),
    "recipe.step-next": () => setFocusedStepIdx((i) => Math.min(i + 1, Math.max(steps.length - 1, 0))),
    "recipe.step-prev": () => setFocusedStepIdx((i) => Math.max(i - 1, 0)),
    "recipe.census-open": () => {
      if (!focusedStep) return;
      setCensusExpanded((m) => ({ ...m, [focusedStep.id]: !m[focusedStep.id] }));
    },
    "recipe.materialise": () => {
      if (loopback) doMaterialise();
    },
    "recipe.save-as-set": doSaveAsSet,
    "recipe.trust": () => {
      if (loopback && trustBlocksRun) doTrust();
    },
    "recipe.copy-cli": doCopyCli,
    "recipe.row-next": () =>
      setFocusedRowIdx((i) => Math.min(i + 1, Math.max((focusedStep?.rows.length ?? 1) - 1, 0))),
    "recipe.row-prev": () => setFocusedRowIdx((i) => Math.max(i - 1, 0)),
    "recipe.open": openFocusedRow,
  };

  function onKeyDown(e: React.KeyboardEvent) {
    const cmd = resolveCommand(tokenOf(e), "recipe");
    if (!cmd) return;
    const handler = (handlers as Record<string, (() => void) | undefined>)[cmd.id];
    if (!handler) return;
    e.preventDefault();
    handler();
  }

  return (
    <div className="kbc-recipes kbc-recipe-page" id="main" onKeyDown={onKeyDown} data-kbc-recipes>
      <header className="kbc-recipes__head">
        <Link to={recipeRunUrl(repo)} className="kbc-recipes__back-link">
          ← All recipes
        </Link>
        <h1 className="kbc-recipes__title" data-kbc-recipe-run-title={slug}>
          {spec?.title ?? slug}
        </h1>
        {spec && (
          <p className="kbc-recipes__hint">
            <RecipeTrustBadge state={spec.trust} /> · {spec.home}
          </p>
        )}
        {spec?.description_md && <p className="kbc-recipes__hint">{spec.description_md}</p>}
      </header>

      {trustBlocksRun && spec && (
        <div className="kbc-recipe-trust-gate" data-kbc-recipe-trust-gate={trust}>
          <p>
            {trust === "changed"
              ? "This repo-versioned recipe's bytes changed since it was last trusted."
              : "This repo-versioned recipe hasn't been trusted yet — running it refuses until it is."}
          </p>
          {spec.trust_diff && (
            <pre className="kbc-recipe-trust-gate__diff" data-kbc-recipe-trust-diff>
              {spec.trust_diff}
            </pre>
          )}
          <code className="kbc-recipe-cli-line" data-kbc-recipe-trust-cli>
            kb-code recipe trust {slug} --repo {repo}
          </code>
          {loopback && (
            <button type="button" onClick={doTrust} data-kbc-recipe-trust-btn>
              Trust
            </button>
          )}
        </div>
      )}

      {spec && (
        <RecipeAutoForm
          specs={spec.params ?? []}
          params={draftParams}
          onParamChange={(name, v) => setDraftParams((p) => ({ ...p, [name]: v }))}
          errors={errors}
          serverFieldErrors={serverFieldErrors}
          scope={draftScope}
          onScopeChange={setDraftScope}
          ctx={draftCtx}
          onCtxChange={(field, v) => setDraftCtx((c) => ({ ...c, [field]: v }))}
          ctxAutoDetected={ctxAutoDetected}
        />
      )}

      <div className="kbc-recipe-cli-bar" data-kbc-recipe-cli-bar>
        <code className="kbc-recipe-cli-line">{cliLine}</code>
        <button type="button" onClick={doCopyCli} data-kbc-recipe-copy-cli title="Copy (Alt-y)">
          <Icon.Copy />
        </button>
        <button
          type="button"
          className="kbc-recipes__go"
          onClick={commitAndRun}
          disabled={trustBlocksRun || errors.length > 0}
          data-kbc-recipes-run-btn
          title="Run (Alt-Enter)"
        >
          Run
        </button>
        {loopback && !trustBlocksRun && (
          <button type="button" onClick={doMaterialise} data-kbc-recipe-materialise title="Materialise (Alt-m)">
            <Icon.ClipboardCheck /> Materialise
          </button>
        )}
      </div>

      {runQ.isLoading && <div className="kbc-recipes__muted">Running…</div>}
      {runQ.error && !(runQ.error instanceof ApiError && runQ.error.status === 403) && (
        <div className="kbc-recipes__error" data-kbc-recipes-error>
          {runQ.error instanceof ApiError ? runQ.error.message : String(runQ.error)}
        </div>
      )}

      {/* Same isolation as `RecipeReplay` — a bug rendering one step's rows
          against a real corpus must not blank the form/CLI-bar above it. */}
      {run && (
        <ErrorBoundary resetKey={`${slug}:${run.honesty.generation}`}>
          <RunHonesty run={run} />
          <RunSteps
            repo={repo}
            run={run}
            focusedStepIdx={focusedStepIdx}
            focusedRowIdx={focusedRowIdx}
            activeViewByStep={activeViewByStep}
            onSelectView={(stepId, viewId) => setActiveViewByStep((m) => ({ ...m, [stepId]: viewId }))}
            censusExpanded={censusExpanded}
            onToggleCensus={(stepId) => setCensusExpanded((m) => ({ ...m, [stepId]: !m[stepId] }))}
          />
        </ErrorBoundary>
      )}
    </div>
  );
}

function RunHonesty({ run }: { run: KbcRunOut }) {
  const h = run.honesty;
  return (
    <p className="kbc-recipes__hint" data-kbc-recipe-honesty>
      {h.elapsed_ms}ms of a {h.budget_ms}ms budget
      {h.budget_exhausted && " — budget exhausted, some steps may be incomplete"}
      {h.notes && h.notes.length > 0 && ` (${h.notes.join("; ")})`}
    </p>
  );
}

function RunSteps({
  repo,
  run,
  focusedStepIdx,
  focusedRowIdx,
  activeViewByStep,
  onSelectView,
  censusExpanded,
  onToggleCensus,
}: {
  repo: string;
  run: KbcRunOut;
  focusedStepIdx?: number;
  focusedRowIdx?: number;
  activeViewByStep?: Record<string, string>;
  onSelectView?: (stepId: string, viewId: string) => void;
  censusExpanded?: Record<string, boolean>;
  onToggleCensus?: (stepId: string) => void;
}) {
  return (
    <div className="kbc-recipe-steps" data-kbc-recipe-steps>
      {run.steps.map((step, i) => {
        const views = run.views.filter((v) => v.step === step.id);
        const focused = i === focusedStepIdx;
        return (
          <section
            key={step.id}
            className={"kbc-recipe-step" + (focused ? " kbc-recipe-step--focus" : "")}
            data-kbc-recipe-step={step.id}
          >
            <h3 className="kbc-recipe-step__title">
              {step.title ?? step.id}
              <span className="kbc-recipe-step__meta">
                {step.ms}ms · {step.rows.length} of {step.total}
                {step.truncated && " (truncated)"}
              </span>
            </h3>
            {step.rows.length === 0 ? (
              <CensusPanel
                census={step.census}
                expanded={censusExpanded?.[step.id]}
                onToggleExpanded={onToggleCensus ? () => onToggleCensus(step.id) : undefined}
              />
            ) : (
              <RecipeResultViews
                repo={repo}
                step={step}
                views={views}
                activeViewId={activeViewByStep?.[step.id]}
                onSelectView={(viewId) => onSelectView?.(step.id, viewId)}
                focusedRowIndex={focused ? (focusedRowIdx ?? null) : null}
              />
            )}
          </section>
        );
      })}
    </div>
  );
}
