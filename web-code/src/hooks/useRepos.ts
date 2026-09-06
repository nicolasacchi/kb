import { useQuery } from "@tanstack/react-query";
import { fetchRepos } from "../api/client";

export function useRepos() {
  return useQuery({
    queryKey: ["repos"],
    queryFn: fetchRepos,
  });
}
