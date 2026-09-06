import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ApiError } from "../api/client";
import type { RecipeCatalogEntry, RecipeItem } from "../api/types";
import EmptyState from "../components/EmptyState";
import { ClassBadge } from "../components/hierarchy/HierarchyPanel";
import { Icon } from "../components/icons";
import { useRecipeRun, useRecipesCatalog } from "../hooks/useRecipes";
import { readerUrl } from "../lib/breadcrumbs";
import { formatAttentionScore } from "../lib/attentionRamp";
import {
  parseRecipesSearch,
  recipeRequiresSince,
  recipesUrl,
} from "../lib/recipesUrl";
import "../styles/recipes.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

const LIMIT_OPTIONS = [25, 50, 100, 250, 500] as const;

/// Label for catalog entries — never paraphrase god-functions as "god".
function recipePageLabel(entry: RecipeCatalogEntry): string {
  // Prefer the first sentence of the catalog description as the human label.
  const d = entry.description.trim();
  const cut = d.indexOf(".");
  return cut > 0 ? d.slice(0, cut) : d || entry.name;
}

function termsEntries(terms: RecipeItem["terms"]): Array<[string, string]> {
  if (!terms || typeof terms !== "object") return [];
  return Object.entries(terms).map(([k, v]) => [
    k,
    v == null ? "—" : typeof v === "number" ? String(v) : String(v),
  ]);
}

function itemKey(item: RecipeItem, i: number): string {
  return `${item.path ?? ""}:${item.symbol ?? ""}:${item.line ?? ""}:${i}`;
}

/// V3.3-U1 — `/r/:repo/~recipes`: catalog + run panel + ranked results.
export default function Recipes() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const urlState = useMemo(() => parseRecipesSearch(searchParams), [searchParams]);

  const catalogQ = useRecipesCatalog();
  const recipes = catalogQ.data?.recipes ?? [];

  const selectedName = urlState.recipe ?? "";
  const selected = recipes.find((r) => r.name === selectedName) ?? null;
  const needsSince = selected ? recipeRequiresSince(selected.params) : false;

  // Local draft for since/limit; URL is source of truth for a committed run.
  const [sinceDraft, setSinceDraft] = useState(urlState.since ?? "");
  const [limitDraft, setLimitDraft] = useState<number>(urlState.limit ?? 50);
  const [expanded, setExpanded] = useState<string | null>(null);

  // Keep drafts in sync when URL deep-link changes.
  useEffect(() => {
    setSinceDraft(urlState.since ?? "");
    if (urlState.limit) setLimitDraft(urlState.limit);
  }, [urlState.since, urlState.limit, urlState.recipe]);

  // Auto-run when URL has a recipe (and since when required).
  const canAutoRun =
    !!selectedName &&
    (!needsSince || !!(urlState.since && urlState.since.length > 0));

  const runQ = useRecipeRun(
    canAutoRun
      ? {
          name: selectedName,
          repo,
          since: urlState.since,
          limit: urlState.limit ?? limitDraft,
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
    <div className="kbc-recipes" id="main" data-kbc-recipes>
      <header className="kbc-recipes__head">
        <h1 className="kbc-recipes__title">Recipes — {repo}</h1>
        <p className="kbc-recipes__hint" data-kbc-recipes-hint>
          Named attention queries over symbols, history, and sessions — look-here
          signals, not quality verdicts.
        </p>
      </header>

      <div className="kbc-recipes__layout">
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
                    <span className="kbc-recipes__cat-desc">{recipePageLabel(r)}</span>
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
                    {LIMIT_OPTIONS.map((n) => (
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

              {/* Absence ≠ zero: when inputs_missing is set the recipe did
                  NOT run over real data — only the "not computed" banner may
                  speak; an "empty result set" message would contradict it. */}
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
                        const key = itemKey(item, i);
                        const open = expanded === key;
                        const terms = termsEntries(item.terms);
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
    </div>
  );
}
