import { useQuery } from "@tanstack/react-query";
import { fetchFile } from "../api/client";

export function useFile(repo: string | undefined, path: string | undefined, ref: string | undefined) {
  return useQuery({
    queryKey: ["file", repo, path, ref ?? null],
    queryFn: () => fetchFile(repo as string, path as string, ref),
    enabled: repo !== undefined && path !== undefined,
  });
}
