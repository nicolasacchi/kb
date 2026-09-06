// kb desk v2 — slim "Changed since you last read" banner in the reader
// chrome (parent, never inside the iframe). Visible when the open artifact
// is a desk handoff (`source_relative` starts with `handoff/`) AND
// `changed_since_read`. Dismiss is per-artifact per-tab via sessionStorage;
// a newer `updated_unix` uses a different key and re-shows.

import { useCallback, useState } from "react";
import { Link } from "react-router-dom";
import { Icon } from "../icons";
import { useDesk } from "../../hooks/useDesk";
import type { DeskItem } from "../../api/desk";
import { artifactHref } from "../../lib/artifactHref";

export function deskBannerDismissKey(
  kb: string,
  id: string,
  updatedUnix: number,
): string {
  return `kb:desk-banner-dismissed:${kb}:${id}:${updatedUnix}`;
}

export function isHandoffPath(sourceRelative: string): boolean {
  return sourceRelative.startsWith("handoff/");
}

export function findDeskItem(
  items: DeskItem[],
  kb: string,
  id: string,
): DeskItem | undefined {
  return items.find((item) => item.kb === kb && item.id === id);
}

export function deskBannerVisible(
  item: DeskItem | undefined,
  dismissed: boolean,
): boolean {
  return (
    !!item &&
    isHandoffPath(item.source_relative) &&
    item.changed_since_read &&
    !dismissed
  );
}

function readDismissed(key: string): boolean {
  try {
    return sessionStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

function writeDismissed(key: string): void {
  try {
    sessionStorage.setItem(key, "1");
  } catch {
    // Private mode / quota — treat as dismissed for this render only.
  }
}

export default function DeskChangedBanner({
  kb,
  id,
}: {
  kb: string;
  id: string;
}) {
  const { items } = useDesk();
  const item = findDeskItem(items, kb, id);
  const key = item
    ? deskBannerDismissKey(item.kb, item.id, item.updated_unix)
    : "";
  // Local latch so a click re-renders without waiting for a storage event.
  // Compared against `key` so a newer updated_unix (different key) re-shows.
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);
  const dismissed =
    key !== "" && (dismissedKey === key || readDismissed(key));

  const onDismiss = useCallback(() => {
    if (!key) return;
    writeDismissed(key);
    setDismissedKey(key);
  }, [key]);

  if (!deskBannerVisible(item, dismissed)) return null;

  const prevHref =
    item!.last_opened_unix !== undefined
      ? artifactHref(item!.kb, item!.source_relative, {
          at: item!.last_opened_unix,
        })
      : null;

  return (
    <div className="kb-desk-banner" role="status" data-testid="desk-changed-banner">
      <span className="kb-desk-banner__msg">Changed since you last read</span>
      {prevHref && (
        <Link
          className="kb-desk-banner__prev"
          to={prevHref}
          data-testid="desk-banner-prev"
        >
          View previous version
        </Link>
      )}
      <button
        type="button"
        className="kb-desk-banner__x"
        aria-label="dismiss"
        title="dismiss"
        onClick={onDismiss}
      >
        <Icon.X aria-hidden />
      </button>
    </div>
  );
}
