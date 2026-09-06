// Pure URL state for `/r/:repo/~recipes?recipe=&since=&limit=`.
// One builder + one parser so deep-links stay golden-pinned.

import { codeBasePath } from "./codeUrl";

export interface RecipesUrlState {
  recipe?: string;
  since?: string;
  limit?: number;
}

/// Build `/r/{repo}/~recipes[?recipe=&since=&limit=]`. Param order is always
/// `recipe`, `since`, `limit` (omit empties).
export function recipesUrl(repo: string, state: RecipesUrlState = {}): string {
  const base = `${codeBasePath(repo, "")}/~recipes`;
  const params = new URLSearchParams();
  if (state.recipe) params.set("recipe", state.recipe);
  if (state.since) params.set("since", state.since);
  if (state.limit !== undefined && Number.isFinite(state.limit) && state.limit > 0) {
    params.set("limit", String(state.limit));
  }
  const qs = params.toString();
  return qs ? `${base}?${qs}` : base;
}

/// Parse recipe URL search params (total: junk → empty fields).
export function parseRecipesSearch(search: string | URLSearchParams): RecipesUrlState {
  const sp = typeof search === "string" ? new URLSearchParams(search.startsWith("?") ? search.slice(1) : search) : search;
  const out: RecipesUrlState = {};
  const recipe = sp.get("recipe");
  if (recipe) out.recipe = recipe;
  const since = sp.get("since");
  if (since) out.since = since;
  const limitRaw = sp.get("limit");
  if (limitRaw) {
    const n = Number(limitRaw);
    if (Number.isFinite(n) && n > 0) out.limit = Math.floor(n);
  }
  return out;
}

/// Whether a catalog entry requires a `since` param (mirrors server enum).
export function recipeRequiresSince(params: Array<{ name: string; required: boolean }>): boolean {
  return params.some((p) => p.name === "since" && p.required);
}
