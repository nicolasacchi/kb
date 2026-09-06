// W2.1 — pure row-assembly for the reader inspector's "Story" tab: a single
// deterministic timeline merging the artifact's origin session, every
// session that later touched it (with its steering decisions + git
// commits), a per-artifact comments beat, and its version history. No
// fetching, no React — BiographyTab.tsx owns the hooks (useArtifactSessions /
// useReview / useVersions, all already-shipped, per invariant #23) and calls
// `assembleBiography` with their results.
//
// "Detected, not ground truth" (borrowed from kb-code's Attribution ladder,
// look only — see the W2.1 recon §4): an unresolved commit shows its sha
// prefix honestly, never a fabricated subject.

import type { ArtifactSessionOut } from "../api/sessions";
import type { Comment } from "../api/client";
import type { Version } from "../api/versions";

export type BioEventKind =
  | "origin"
  | "session"
  | "decision"
  | "commit"
  | "comments"
  | "version";

// origin/session share every field — a session is either the artifact's
// birth (`authored`) or a later touch; the discriminant is `kind`, not a
// different shape, so a row renderer can treat them almost identically.
export type OriginBioEvent = {
  kind: "origin";
  id: string;
  unix: number;
  sessionId: string;
  kb: string;
  displayName: string;
  firstUserPrompt?: string;
  read: boolean;
  wrote: boolean;
  edited: boolean;
};

export type SessionBioEvent = {
  kind: "session";
  id: string;
  unix: number;
  sessionId: string;
  kb: string;
  displayName: string;
  firstUserPrompt?: string;
  read: boolean;
  wrote: boolean;
  edited: boolean;
};

export type DecisionBioEvent = {
  kind: "decision";
  id: string;
  unix: number;
  sessionId: string;
  kb: string;
  // "question" | "plan" (DecisionOut.kind is a plain string wire type).
  decisionKind: string;
  prompt: string;
  answer?: string;
};

export type CommitBioEvent = {
  kind: "commit";
  id: string;
  unix: number;
  sessionId: string;
  kb: string;
  // Best-available prefix — never fabricated when unresolved.
  sha: string;
  // Present ONLY when `resolved` — an unresolved row never shows a subject,
  // even if the (untrusted) column happened to carry one.
  subject?: string;
  resolved: boolean;
};

export type CommentsBioEvent = {
  kind: "comments";
  id: string;
  unix: number;
  openCount: number;
  resolvedCount: number;
};

export type VersionBioEvent = {
  kind: "version";
  id: string;
  unix: number;
  ref: string;
  label: string;
  short: string;
  source: Version["source"];
};

export type BioEvent =
  | OriginBioEvent
  | SessionBioEvent
  | DecisionBioEvent
  | CommitBioEvent
  | CommentsBioEvent
  | VersionBioEvent;

export type BiographyInputs = {
  sessions: ArtifactSessionOut[];
  // The loaded review's comments (empty when no review file exists yet).
  comments: Comment[];
  versions: Version[];
};

function sessionKey(kb: string, sessionId: string): string {
  return `${kb}:${sessionId}`;
}

// Deterministic sort: newest event first. `Array.prototype.sort` is a
// stable sort (ES2019+, every engine this SPA targets), so ties (equal
// `unix`) keep the order they were pushed in below — sessions arrive
// newest-first from the server (invariant #11), and each session's own
// decisions/commits arrive in server seq order, so the construction order
// below IS the deterministic tie-break; no secondary sort key needed.
function stableSortDesc(events: BioEvent[]): BioEvent[] {
  return [...events].sort((a, b) => b.unix - a.unix);
}

/** Pure row assembly — no fetching. See the module doc for provenance. */
export function assembleBiography(inputs: BiographyInputs): BioEvent[] {
  const events: BioEvent[] = [];

  for (const s of inputs.sessions) {
    const key = sessionKey(s.kb, s.session_id);
    if (s.authored) {
      events.push({
        kind: "origin",
        id: `origin:${key}`,
        unix: s.started_at,
        sessionId: s.session_id,
        kb: s.kb,
        displayName: s.display_name,
        firstUserPrompt: s.first_user_prompt,
        read: s.read,
        wrote: s.wrote,
        edited: s.edited,
      });
    } else {
      events.push({
        kind: "session",
        id: `session:${key}`,
        unix: s.started_at,
        sessionId: s.session_id,
        kb: s.kb,
        displayName: s.display_name,
        firstUserPrompt: s.first_user_prompt,
        read: s.read,
        wrote: s.wrote,
        edited: s.edited,
      });
    }

    s.decisions.forEach((d, i) => {
      events.push({
        kind: "decision",
        id: `decision:${key}:${i}`,
        unix: s.started_at,
        sessionId: s.session_id,
        kb: s.kb,
        decisionKind: d.kind,
        prompt: d.prompt,
        answer: d.answer,
      });
    });

    s.commits.forEach((c, i) => {
      const sha = c.sha ?? (c.sha_full ? c.sha_full.slice(0, 10) : undefined);
      events.push({
        kind: "commit",
        id: `commit:${key}:${sha ?? i}`,
        unix: s.started_at,
        sessionId: s.session_id,
        kb: s.kb,
        sha: sha ?? "?",
        subject: c.resolved ? c.subject : undefined,
        resolved: c.resolved,
      });
    });
  }

  // One beat for the whole comment thread (open/resolved counts), not a row
  // per comment — a heavily-commented artifact would otherwise flood the
  // timeline. Dated at the newest comment so it sorts with the rest.
  if (inputs.comments.length > 0) {
    let open = 0;
    let resolved = 0;
    let newest = 0;
    for (const c of inputs.comments) {
      if (c.status === "open") open += 1;
      else resolved += 1;
      const t = Math.floor(Date.parse(c.createdAt) / 1000);
      if (Number.isFinite(t) && t > newest) newest = t;
    }
    events.push({
      kind: "comments",
      id: "comments:beat",
      unix: newest,
      openCount: open,
      resolvedCount: resolved,
    });
  }

  for (const v of inputs.versions) {
    events.push({
      kind: "version",
      id: `version:${v.ref}`,
      unix: v.ts_unix,
      ref: v.ref,
      label: v.label,
      short: v.short,
      source: v.source,
    });
  }

  return stableSortDesc(events);
}

// A biography can accrue a lot of version/commit rows on a long-lived
// artifact; cap what renders by default and let the tab expand into the
// rest on demand (no re-fetch — everything's already in `events`).
export const BIOGRAPHY_VISIBLE_CAP = 40;

export function splitBiography(
  events: BioEvent[],
  cap: number = BIOGRAPHY_VISIBLE_CAP,
): { visible: BioEvent[]; collapsed: BioEvent[] } {
  if (events.length <= cap) return { visible: events, collapsed: [] };
  return { visible: events.slice(0, cap), collapsed: events.slice(cap) };
}
