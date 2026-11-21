import { useEffect } from "react";
import { titleWithBadge } from "../lib/termAttention";

const SUFFIX = "kb";

// The route's own title and the attention count (sessions blocked on the
// operator) live here together, so every write to document.title is
// `(n) <title>` and a route change can never drop the badge.
let baseTitle = SUFFIX;
let attention = 0;

function apply(): void {
  document.title = titleWithBadge(baseTitle, attention);
}

// FUTERMATTN: the "(n) " tab-title prefix and the installed-app badge. Called
// from the live-status consumer; it opens no query or stream of its own.
export function setAttentionCount(n: number): void {
  const next = Number.isFinite(n) ? Math.max(0, Math.floor(n)) : 0;
  const changed = next !== attention;
  attention = next;
  apply();
  if (!changed || typeof navigator === "undefined") return;
  const nav = navigator as Navigator & {
    setAppBadge?: (n: number) => Promise<void>;
    clearAppBadge?: () => Promise<void>;
  };
  // The Badging API is best effort (installed PWAs); a refusal is not an error.
  if (next > 0) void nav.setAppBadge?.(next)?.catch(() => {});
  else void nav.clearAppBadge?.()?.catch(() => {});
}

// Sets document.title to `<title> · kb`, or just "kb" when title is
// null/empty (with the attention prefix when sessions need you). Restores
// "kb" on unmount so a route that doesn't set a title can't leave a stale
// tab name behind.
export function useDocumentTitle(title: string | null | undefined): void {
  useEffect(() => {
    const t = title?.trim();
    baseTitle = t ? `${t} · ${SUFFIX}` : SUFFIX;
    apply();
    return () => {
      baseTitle = SUFFIX;
      apply();
    };
  }, [title]);
}
