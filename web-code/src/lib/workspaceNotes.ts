// V70-A10 ("Workspaces v0") — pure helpers for the workspace notes panel:
// composer payload shapes and the general/code-anchored grouping. Mirrors
// `lib/annotations.ts`'s own "pure, React/CM6-free, unit-tested without a
// network mock" posture — `groupThreads` itself is reused as-is (a
// workspace note thread has the exact same parent+replies shape any other
// annotation thread does; the only thing new here is bucketing a
// workspace's own listing into "general notes first, then code-anchored
// ones grouped by file" for display, D26's own stated grammar).

import type { AnnotationView } from "../api/types";
import type { CreateAnnotationInput } from "../api/client";
import { groupThreads, type AnnotationThread } from "./annotations";

export { groupThreads };
export type { AnnotationThread };

/// A workspace note composer's in-progress state. Deliberately narrower
/// than `lib/annotations.ts`'s `ComposerDraft`: a workspace note is either
/// a general path-less note or a plain `line` note at the current
/// selection — the richer `range`/`symbol`/`diff` kind picker stays the
/// file-scoped `AnnotationsPanel`'s job, not this composer's.
export interface WorkspaceNoteDraft {
  repo: string;
  setId: string;
  body: string;
  /// Present + non-empty ⇒ a code-anchored note at `path:line`. Absent/
  /// empty ⇒ a general, path-less note (`anchor_kind: "set"`).
  path?: string;
  line?: number;
}

/// Build the exact `POST /api/annotations` body for a new workspace note
/// (general OR code-anchored, see [`WorkspaceNoteDraft`]'s doc), or `null`
/// when there's nothing worth sending (an empty/whitespace-only body, or a
/// `path` given with no positive `line`) — same "the composer's Save
/// button just disables on `null`" contract `lib/annotations.ts`'s
/// `buildCreatePayload` documents.
export function buildWorkspaceNotePayload(draft: WorkspaceNoteDraft): CreateAnnotationInput | null {
  const body = draft.body.trim();
  if (!body) return null;
  const path = draft.path?.trim();
  if (path) {
    if (draft.line === undefined || draft.line < 1) return null;
    return { repo: draft.repo, path, line: draft.line, body, set_id: draft.setId };
  }
  return { repo: draft.repo, path: "", anchor_kind: "set", body, set_id: draft.setId };
}

/// Build the exact `POST /api/annotations` body for a REPLY within a
/// workspace (no anchor, no `line`, no `set_id` — inherited server-side
/// from the parent, mirrors `lib/annotations.ts`'s `buildReplyPayload`
/// omitting `review_id` for the same reason) — `null` for an empty/
/// whitespace-only body.
export function buildWorkspaceReplyPayload(
  repo: string,
  parentId: string,
  rawBody: string,
): CreateAnnotationInput | null {
  const body = rawBody.trim();
  if (!body) return null;
  return { repo, path: "", parent_id: parentId, body };
}

/// Partition a workspace's threads into the panel's two sections: general
/// (path-less) notes first, then code-anchored ones GROUPED BY FILE (in
/// first-appearance order — the server returns `annotations` oldest-first,
/// `groupThreads`'s own order preserved, so the first file to get a note is
/// the first group shown).
export interface WorkspaceNoteGroups {
  general: AnnotationThread[];
  byFile: { path: string; threads: AnnotationThread[] }[];
}

export function groupWorkspaceNotes(annotations: AnnotationView[]): WorkspaceNoteGroups {
  const threads = groupThreads(annotations);
  const general: AnnotationThread[] = [];
  const byFileOrder: string[] = [];
  const byFileMap = new Map<string, AnnotationThread[]>();
  for (const t of threads) {
    const path = t.parent.path;
    if (!path) {
      general.push(t);
      continue;
    }
    const existing = byFileMap.get(path);
    if (existing) {
      existing.push(t);
    } else {
      byFileMap.set(path, [t]);
      byFileOrder.push(path);
    }
  }
  return {
    general,
    byFile: byFileOrder.map((path) => ({ path, threads: byFileMap.get(path) as AnnotationThread[] })),
  };
}
