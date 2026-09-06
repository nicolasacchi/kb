// PRR-U5+U6 (design-ui.md §4 S4 — Timeline tab) — pure event→row mapping
// over `GET /api/reviews/{id}/timeline` (`review-timeline/1`,
// `review_timeline.rs`'s closed-vocab event stream: `review_created` ·
// `pr_bound` · `patchset` · `findings_import` · `finding_added` ·
// `disposition` · `verdict` · `finding_published` · `verdict_published` ·
// `comment`). This is the ONE place that narrows the server's loose
// per-kind payload — `TimelinePanel.tsx` renders `TimelineRow`s only, never
// branches on `kind` itself, so an unrecognized future kind can never crash
// the panel: it degrades to a plain label via the `default` branch below
// (§8 "the room never lies" — an honest generic row, not a blank gap or a
// thrown error).
import type { ReviewTimelineEvent } from "../api/types";
import { reviewDiffHref } from "./codeUrl";

export type TimelineIconKind =
  | "created"
  | "pr"
  | "patchset"
  | "import"
  | "finding"
  | "disposition"
  | "verdict"
  | "published"
  | "comment"
  /// PRR-F — a client-synthesized GitHub-origin row (`lib/githubThreads.ts`'s
  /// `githubTimelineRows`) — NOT a server `kind` (the server's closed vocab,
  /// listed in this module's own header doc, never emits this).
  | "github"
  | "unknown";

export interface TimelineRow {
  at: number;
  kind: string;
  icon: TimelineIconKind;
  label: string;
  detail?: string;
  /// In-app deep link (finding overlay / thread scroll+flash).
  href?: string;
  /// External GitHub URL (publish events only) — opens in a new tab.
  external?: string;
}

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}
function num(v: unknown): number | undefined {
  return typeof v === "number" ? v : undefined;
}
function strArr(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/// One event → one display row. `repo`/`reviewId` are needed to build the
/// in-app deep links (`reviewDiffHref`'s `finding=` grammar, §5) — the
/// timeline payload itself never carries a full URL.
export function timelineRow(e: ReviewTimelineEvent, repo: string, reviewId: number): TimelineRow {
  const at = num(e.at) ?? 0;
  const kind = str(e.kind) ?? "unknown";

  switch (kind) {
    case "review_created":
      return { at, kind, icon: "created", label: "Review created" };

    case "pr_bound": {
      const prNumber = num(e.pr_number);
      const slug = str(e.pr_repo_slug);
      return {
        at,
        kind,
        icon: "pr",
        label: prNumber != null ? `PR #${prNumber} bound` : "PR bound",
        detail: slug,
      };
    }

    case "patchset": {
      const psNumber = num(e.ps_number);
      const sha = str(e.tip_sha);
      return {
        at,
        kind,
        icon: "patchset",
        label: `ps${psNumber ?? "?"} captured`,
        detail: sha ? sha.slice(0, 10) : undefined,
      };
    }

    case "findings_import": {
      const slugs = strArr(e.slugs);
      const count = num(e.count) ?? slugs.length;
      return {
        at,
        kind,
        icon: "import",
        label: `${count} finding${count === 1 ? "" : "s"} imported`,
        detail: slugs.length > 0 ? slugs.join(", ") : undefined,
      };
    }

    case "finding_added": {
      const slug = str(e.slug);
      return {
        at,
        kind,
        icon: "finding",
        label: slug ? `finding added: ${slug}` : "finding added",
        detail: str(e.title),
        href: slug ? reviewDiffHref(repo, reviewId, undefined, { finding: slug }) : undefined,
      };
    }

    case "disposition": {
      const slug = str(e.slug);
      const state = str(e.state);
      const by = str(e.by);
      return {
        at,
        kind,
        icon: "disposition",
        label: `${slug ?? "finding"} → ${state ?? "?"}`,
        detail: by ? `by ${by}` : undefined,
        href: slug ? reviewDiffHref(repo, reviewId, undefined, { finding: slug }) : undefined,
      };
    }

    case "verdict": {
      const state = str(e.state);
      return { at, kind, icon: "verdict", label: `verdict set: ${state ?? "?"}` };
    }

    case "finding_published": {
      const slug = str(e.slug);
      return {
        at,
        kind,
        icon: "published",
        label: slug ? `${slug} published to GitHub` : "finding published to GitHub",
        href: slug ? reviewDiffHref(repo, reviewId, undefined, { finding: slug }) : undefined,
        external: str(e.url),
      };
    }

    case "verdict_published":
      return {
        at,
        kind,
        icon: "published",
        label: "verdict published to GitHub",
        external: str(e.url),
      };

    case "comment": {
      const path = str(e.path);
      const annotationId = str(e.annotation_id);
      const intent = str(e.intent);
      const author = str(e.author);
      const isReply = e.is_reply === true;
      const labelParts = [isReply ? "reply" : "comment"];
      if (intent) labelParts.push(intent);
      if (author) labelParts.push(author);
      return {
        at,
        kind,
        icon: "comment",
        label: labelParts.join(" · "),
        href: annotationId
          ? `${reviewDiffHref(repo, reviewId, path || undefined)}?thread=${encodeURIComponent(annotationId)}`
          : undefined,
      };
    }

    default:
      // Closed vocab server-side, but the client must never crash on an
      // unrecognized/future kind — render it honestly as its own label.
      return { at, kind, icon: "unknown", label: kind === "unknown" ? "unknown event" : kind };
  }
}

export function timelineRows(events: ReviewTimelineEvent[], repo: string, reviewId: number): TimelineRow[] {
  return events.map((e) => timelineRow(e, repo, reviewId));
}
