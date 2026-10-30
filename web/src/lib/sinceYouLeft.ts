// OL IDEA5 — pure composition for the Front Page "Since you left" strip
// (components/SinceYouLeft.tsx). Everything here is deterministic and side-
// effect free; the clock is injected. Every COUNT comes from a server total
// (never from the length of a capped list), and the strip invents no ranking:
// the daemon already ordered groups by time.

import type { ChangesProject, ChangesResponse } from "../api/changes";
import { humanizeDuration } from "./time";

export type SinceDetailKind = "attention" | "commit" | "session";

export type SinceDetail = {
  kind: SinceDetailKind;
  text: string;
  /// In-app target; null when the line has none.
  href: string | null;
};

export type SinceRow = {
  key: string;
  label: string;
  href: string;
  counts: string;
  details: SinceDetail[];
};

export type SinceStrip = { rows: SinceRow[]; more: number };

export const SINCE_MAX_PROJECTS = 6;
export const SINCE_MAX_DETAILS = 2;

const ATTENTION_KINDS = new Set(["hand", "ask", "warn"]);
const SHORT_SHA = 7;

/// "Since you left · 2h 14m"; past two days the hours become whole days, and a
/// clamped window says so plainly instead of a number the server did not use.
export function sinceHeading(sinceUnix: number, nowMs: number, clamped: boolean): string {
  if (clamped) return "Since you left · over 31 days (showing the last 31)";
  const ms = Math.max(0, nowMs - sinceUnix * 1000);
  const span =
    ms >= 2 * 86_400_000 ? `${Math.floor(ms / 86_400_000)}d` : humanizeDuration(ms) || "moments";
  return `Since you left · ${span}`;
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

function sessionsHref(kb: string, sessionId: string): string {
  const q = new URLSearchParams();
  q.set("kb", kb);
  q.set("focus", sessionId);
  return `/sessions?${q.toString()}`;
}

/// A group's link: its slate when the group IS a slate, else the Sessions list
/// filtered to the project (`/sessions?project=` accepts the same id).
export function projectHref(p: Pick<ChangesProject, "project" | "source">): string {
  if (p.source === "slate") return `/slates/${encodeURIComponent(p.project)}`;
  return p.project ? `/sessions?project=${encodeURIComponent(p.project)}` : "/sessions";
}

/// "3 sessions · 7 commits · slate 2 new", from server totals only.
export function countsLine(p: ChangesProject): string {
  const parts: string[] = [];
  if (p.sessions_total > 0) parts.push(plural(p.sessions_total, "session", "sessions"));
  if (p.commit_count > 0) parts.push(plural(p.commit_count, "commit", "commits"));
  if (p.slate) {
    const n = p.slate.posts_total + p.slate.other_total + p.slate.retracted_total;
    if (n > 0) parts.push(`slate ${n} new`);
    else if (p.slate.archived_in_window) parts.push("slate rotated");
  }
  return parts.join(" · ");
}

function detailsFor(p: ChangesProject, max: number): SinceDetail[] {
  const out: SinceDetail[] = [];
  const push = (d: SinceDetail) => {
    if (out.length < max) out.push(d);
  };
  // 1. what is waiting on someone: open hand / ask / warn posts.
  if (p.slate) {
    const href = `/slates/${encodeURIComponent(p.slate.slug)}`;
    for (const post of p.slate.posts) {
      if (ATTENTION_KINDS.has(post.kind) && !post.closed) {
        push({ kind: "attention", text: `${post.kind}: ${post.line}`, href });
      }
    }
  }
  // 2. the newest commits, each under the session that recorded it.
  for (const s of p.sessions) {
    for (const c of s.commits) {
      if (c.kind !== "commit") continue;
      const sha = c.sha ? c.sha.slice(0, SHORT_SHA) : "";
      const subject = c.subject ?? "";
      const text = [sha, subject].filter(Boolean).join(" ");
      if (text) push({ kind: "commit", text, href: sessionsHref(s.kb, s.session_id) });
    }
  }
  // 3. the newest sessions, with how they closed.
  for (const s of p.sessions) {
    push({
      kind: "session",
      text: s.closing ? `${s.display_name} — ${s.closing}` : s.display_name,
      href: sessionsHref(s.kb, s.session_id),
    });
  }
  return out;
}

export function composeSinceStrip(
  resp: ChangesResponse,
  opts: { maxProjects?: number; maxDetails?: number } = {},
): SinceStrip {
  const maxProjects = opts.maxProjects ?? SINCE_MAX_PROJECTS;
  const maxDetails = opts.maxDetails ?? SINCE_MAX_DETAILS;
  const shown = resp.projects.slice(0, maxProjects);
  const rows: SinceRow[] = shown.map((p) => ({
    key: p.project || "(unattributed)",
    label: p.label,
    href: projectHref(p),
    counts: countsLine(p),
    details: detailsFor(p, maxDetails),
  }));
  // The server's own total, so groups the daemon capped are still counted.
  const more = Math.max(0, resp.projects_total - rows.length);
  return { rows, more };
}

/// The footer link to the corpus axis (artifacts, memories, comments) — the
/// Ambient page already answers it for a since.
export function ambientSinceHref(since: number, kb: string | null): string {
  const q = new URLSearchParams();
  q.set("since", String(since));
  if (kb) q.set("kb", kb);
  return `/ambient?${q.toString()}`;
}
