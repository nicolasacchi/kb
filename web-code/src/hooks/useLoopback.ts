import { useRepos } from "./useRepos";
import { isLoopbackCaller } from "../lib/loopback";

/// Whether THIS caller can reach the daemon's loopback-only routes — the
/// daemon's own verdict, riding the `["repos"]` query every route already
/// holds (see `lib/loopback.ts` for why it is not derived from the hostname).
export function useLoopback(): boolean {
  const repos = useRepos();
  return isLoopbackCaller(repos.data);
}
