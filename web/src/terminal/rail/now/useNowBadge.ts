// RAILNOW — the Now tab's badge: 1 only while the bound session is blocked on
// the operator. Reads the shared live-status query (no second poll).

import { useLiveSessions } from "../../../hooks/useLiveSessions";
import { useTermRail } from "../context";
import { nowBadge } from "./nowModel";

export function useNowBadge(): number | undefined {
  const { sid } = useTermRail();
  const { rows } = useLiveSessions();
  if (!sid) return undefined;
  return nowBadge(rows.find((r) => r.session_id === sid));
}
