import { useQuery } from "@tanstack/react-query";
import { fetchFrames } from "../api/client";

/// `GET /api/frames` is a build-time table. Cache it forever; a reload
/// is the only thing that can change it.
export function useFrames() {
  return useQuery({
    queryKey: ["frames"],
    queryFn: fetchFrames,
    staleTime: Infinity,
  });
}
