import { useQuery } from "@tanstack/react-query";
import {
  fetchRecipeCatalog,
  fetchRecipeReplay,
  fetchRecipeRun,
  fetchRecipeRunV2,
  fetchRecipesCatalog,
  fetchRecipeShow,
  type FetchRecipeRunParams,
  type RecipeRunQuery,
} from "../api/client";
import { BEHAVIORAL_STALE_MS } from "./useBehavioral";

/// Catalog is pure/global — generous staleTime (history-derived recipes
/// share the same cadence as behavioral surfaces).
export function useRecipesCatalog(enabled = true) {
  return useQuery({
    queryKey: ["recipes", "catalog"] as const,
    queryFn: () => fetchRecipesCatalog(),
    enabled,
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

export function recipeRunKey(params: FetchRecipeRunParams | undefined) {
  return [
    "recipes",
    "run",
    params?.repo,
    params?.name,
    params?.since ?? null,
    params?.limit ?? null,
    params?.scope ?? null,
  ] as const;
}

/// One recipe run, keyed on (repo, recipe, since, limit).
export function useRecipeRun(params: FetchRecipeRunParams | undefined, enabled = true) {
  return useQuery({
    queryKey: recipeRunKey(params),
    queryFn: () => fetchRecipeRun(params as FetchRecipeRunParams),
    enabled:
      enabled &&
      params !== undefined &&
      params.repo !== "" &&
      params.name !== "",
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

// --- V74-L3c — `kbc-recipe/1`, the new runner's own hooks -------------------

/// The recipe home's catalog — per-repo (home/trust/CLI line all depend on
/// the repo's own `.kbc/recipes/*.toml`, not just the recipe id).
export function useRecipeCatalog(repo: string, enabled = true) {
  return useQuery({
    queryKey: ["recipe", "catalog", repo] as const,
    queryFn: () => fetchRecipeCatalog(repo),
    enabled: enabled && repo !== "",
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

/// One recipe's full document — params/steps/views/source/trust-diff, for
/// the auto-form's field list and the trust affordance's diff panel.
export function useRecipeShow(slug: string | undefined, repo: string, enabled = true) {
  return useQuery({
    queryKey: ["recipe", "show", repo, slug] as const,
    queryFn: () => fetchRecipeShow(slug as string, repo),
    enabled: enabled && repo !== "" && !!slug,
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

function recipeRunV2Key(q: RecipeRunQuery | undefined) {
  return [
    "recipe",
    "run",
    q?.repo,
    q?.slug,
    q?.scope ?? null,
    q?.limit ?? null,
    q ? JSON.stringify(Object.entries(q.params ?? {}).sort()) : null,
    q ? JSON.stringify(Object.entries(q.ctx ?? {}).sort()) : null,
  ] as const;
}

/// A live run — mutates nothing server-side (`GET`), so ordinary
/// query-cache semantics apply; `retry: false` matches the old runner's own
/// choice (a 400/403/404 is a decision to surface, not a transient fault).
export function useRecipeRunV2(q: RecipeRunQuery | undefined, enabled = true) {
  return useQuery({
    queryKey: recipeRunV2Key(q),
    queryFn: () => fetchRecipeRunV2(q as RecipeRunQuery),
    enabled: enabled && q !== undefined && q.repo !== "" && q.slug !== "",
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

/// A materialised run replay (`~recipes/runs/:id`) — immutable once
/// created, so an `Infinity` staleTime is honest, not merely convenient.
export function useRecipeReplay(id: string | undefined, enabled = true) {
  return useQuery({
    queryKey: ["recipe", "replay", id] as const,
    queryFn: () => fetchRecipeReplay(id as string),
    enabled: enabled && !!id,
    staleTime: Infinity,
    retry: false,
  });
}
