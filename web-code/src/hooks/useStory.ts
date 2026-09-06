import { useQuery } from "@tanstack/react-query";
import { fetchStory } from "../api/client";

/// `GET /api/story` for the provenance tab's "File story" section (CT-E2)
/// — an expensive server call (whole-file blame plus one bounded
/// `git log -L` per region), so it is fetched only while the section is
/// actually MOUNTED (the provenance tab open with a line selected — see
/// `routes/Reader.tsx`'s `whyPanel`), and cached under the default
/// `staleTime: Infinity` (`api/queryClient.ts`) keyed on repo + path.
export function useStory(repo: string | undefined, path: string | undefined, enabled = true) {
  return useQuery({
    queryKey: ["story", repo, path],
    queryFn: () => fetchStory(repo as string, path as string),
    enabled: enabled && repo !== undefined && path !== undefined,
    retry: false,
  });
}
