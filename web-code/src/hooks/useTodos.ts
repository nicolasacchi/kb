import { useQuery } from "@tanstack/react-query";
import { fetchTodos, type FetchTodosParams } from "../api/client";

/// Phase N — `GET /api/todos`. Key includes every filter so chip/scope
/// changes fetch a distinct cache entry (no silent stale bleed).
export function todosQueryKey(params: FetchTodosParams | undefined) {
  return [
    "todos",
    params?.repo,
    params?.marker ?? null,
    params?.path_prefix ?? null,
    params?.scope ?? null,
    params?.limit ?? null,
  ] as const;
}

export function useTodos(params: FetchTodosParams | undefined) {
  return useQuery({
    queryKey: todosQueryKey(params),
    queryFn: () => fetchTodos(params as FetchTodosParams),
    enabled: params !== undefined && params.repo !== "",
  });
}
