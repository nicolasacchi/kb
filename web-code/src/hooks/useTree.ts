import { useQuery } from "@tanstack/react-query";
import { fetchTree } from "../api/client";

/// One directory's lazily-loaded listing (`GET /api/tree`, per-directory —
/// see `FileTree`'s own doc for how these compose into the virtualized
/// row list). `ref` undefined = working tree default (mirrors the server's
/// own "no ref = HEAD" default for the tree endpoint... actually `tree`
/// always defaults server-side to HEAD when `ref` is omitted; passing
/// `undefined` here lets that default apply).
export function useTree(repo: string | undefined, path: string, ref: string | undefined) {
  return useQuery({
    queryKey: ["tree", repo, path, ref ?? null],
    queryFn: () => fetchTree(repo as string, path, ref),
    enabled: repo !== undefined,
  });
}
