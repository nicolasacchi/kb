import { useQuery } from "@tanstack/react-query";
import {
  fetchRecipeRun,
  fetchRecipesCatalog,
  type FetchRecipeRunParams,
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
