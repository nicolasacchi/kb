// TERMNOTIFY - the terminals read for the host, gated like useNavState: the
// shared ["terms"] query only runs for an admitted caller (no 401/403/404
// refetch storm on deployments where [terminal] is off).
import { useQuery } from "@tanstack/react-query";
import type { TermRow } from "../../api/terms";
import { termsQueryOptions } from "../useTerms";

export function useNotifyTerms(admitted: boolean): {
  terms: TermRow[];
  isLoading: boolean;
  isError: boolean;
} {
  const q = useQuery({ ...termsQueryOptions, enabled: admitted });
  return { terms: q.data ?? [], isLoading: q.isPending, isError: q.isError };
}
