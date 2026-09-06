import { artifactHref } from "./artifactHref";
import type { ListEntry } from "../api/client";

// RL-track — one helper builds EVERY trail href (entry rows, the
// Continue-reading button, the QueueBar's prev/next), so the param
// discipline (?sec= from a Section anchor + ?list=&entry= trail context)
// can't drift between call sites.
//
// `null` for tombstoned entries (no source_relative to address) — the
// callers grey the row / skip the hop.
export function entryTrailHref(
  kb: string,
  listId: string,
  e: ListEntry,
): string | null {
  if (!e.source_relative) return null;
  return artifactHref(kb, e.source_relative, {
    sec: e.anchor?.kind === "section" ? e.anchor.id : undefined,
    list: listId,
    entry: e.id,
  });
}

/// The read-dot / mark-read toggle decision, shared by the detail rows
/// and the QueueBar: not-read → override read; derived-read → override
/// unread; override-read → clear back to derived.
export function nextReadToggle(e: ListEntry): "read" | "unread" | null {
  if (e.read_state !== "read") return "read";
  if (e.read_override === "read") return null;
  return "unread";
}

/// First entry worth continuing with: unread before in-progress, in
/// document order; tombstones never qualify.
export function continueTarget(entries: ListEntry[]): ListEntry | null {
  return (
    entries.find((e) => e.read_state === "unread" && !!e.source_relative) ??
    entries.find(
      (e) => e.read_state === "in_progress" && !!e.source_relative,
    ) ??
    null
  );
}
