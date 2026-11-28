// CMDSETTINGS: the `["term","commands"]` query. staleTime Infinity, never
// polls: the `term.commands` SSE event invalidates it through the shared
// bridge (api/queryClient.ts), and a successful save writes the PUT answer
// straight into the cache.
import { queryOptions, useQuery } from "@tanstack/react-query";
import { fetchCommands } from "./api";

export const TERM_COMMANDS_KEY = ["term", "commands"] as const;

export const termCommandsQueryOptions = queryOptions({
  queryKey: TERM_COMMANDS_KEY,
  queryFn: ({ signal }) => fetchCommands(signal),
  staleTime: Infinity,
});

export function useTermCommands() {
  return useQuery(termCommandsQueryOptions);
}
