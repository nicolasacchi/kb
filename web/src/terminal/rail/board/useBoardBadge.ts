// RAILBOARD — the Board tab's live badge: open asks addressed to the operator.
// Reads the same ["slate", slug] query as the tab body, so it costs no second
// fetch; `slate.updated` invalidation keeps it live.

import { useSessionDetail } from "../../../hooks/useSessions";
import { useSlateBoard } from "../../../hooks/useSlates";
import { useTermRail } from "../context";
import { badgeCount } from "./boardRows";

export function useBoardBadge(): number | undefined {
  const { sid } = useTermRail();
  const { detail } = useSessionDetail(sid ?? null);
  const slug = detail?.project_slug ?? "";
  const { board } = useSlateBoard(slug);
  if (!slug) return undefined;
  const n = badgeCount(board);
  return n > 0 ? n : undefined;
}
