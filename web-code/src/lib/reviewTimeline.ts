// PRR-U5+U6 (design-ui.md §4 S4 — Timeline tab) — pure event→row mapping
// over `GET /api/reviews/{id}/timeline`. V73-K2c widens the source to
// `review-timeline/2` (17 kinds across 11 lanes, `sources[]` lane status,
// filters/paging) but keeps this module's own contract unchanged: it is
// the ONE place that narrows the server's loose per-kind payload —
// `TimelinePanel.tsx` renders `TimelineRow`s only, never branches on `kind`
// itself, so an unrecognized future kind can never crash the panel: it
// degrades to a plain label via the `default` branch below (§8 "the room
// never lies" — an honest generic row, not a blank gap or a thrown error).
import type { FieldRefs, ReviewTimelineAuthor, ReviewTimelineEvent } from "../api/types";
import { readerUrl } from "./breadcrumbs";
import { reviewDiffHref } from "./codeUrl";
import { sessionUrl } from "./searchLanes";

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
  /// V73-K2c — new server kinds (`review-timeline/2`'s five new lanes).
  | "pr_body"
  | "wt_comment"
  | "doc_revision"
  | "report"
  | "claim"
  | "turn"
  /// PRR-F — a client-synthesized GitHub-origin row (`lib/githubThreads.ts`'s
  /// `githubTimelineRows`) — NOT a server `kind` in v1. V73-K2c's own
  /// `github_comment` server kind reuses this SAME icon (one visual
  /// "GitHub" glyph regardless of which side produced the row).
  | "github"
  | "unknown";

export interface TimelineRow {
  at: number;
  kind: string;
  icon: TimelineIconKind;
  label: string;
  detail?: string;
  /// In-app deep link (finding overlay / thread scroll+flash / pseudo-file
  /// view / a plain reader location).
  href?: string;
  /// External URL (GitHub publish/comment events, or a session-read
  /// `#t-<uuid12>` deep link for a `turn` row) — opens in a new tab.
  external?: string;
  /// V73-K2c — present when the source event carried a well-formed author
  /// envelope. `TimelinePanel.tsx` renders it as the author register
  /// (human/agent/system + model + session link) beside the label.
  author?: ReviewTimelineAuthor;
  /// V73-K2c — the drift caption, verbatim, when the event's own ref moved
  /// (e.g. a PR description snapshotted against a since-superseded head).
  driftNote?: string;
  /// V76-B3 — the event's own `body_md` when the wire sent one, plus its
  /// per-request prose refs. Absent (not empty) when the event has no body,
  /// so `toEqual` on pre-B3 rows stays byte-identical.
  bodyMd?: string;
  bodyRefs?: FieldRefs;
}

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}
/// The raw author NAME for the `comment`/`wt_comment` kinds' inline label —
/// tolerant of both shapes: the v2 envelope object (`{kind, name, …}`,
/// correct post-V73-K2c server fix, review_timeline.rs's own doc on the
/// deleted duplicate-key `.with("author", …)` calls) and a bare string
/// (an unfixed/older daemon, or a hand-built test fixture). Never the
/// SOURCE of the author-register display (`row.author`, which only ever
/// comes from `isAuthor`'s strict object check below) — this is only the
/// inline "· <name>" fragment these two kinds' labels already carried
/// pre-K2c.
function authorNameOf(v: unknown): string | undefined {
  if (typeof v === "string") return v;
  if (typeof v === "object" && v !== null && typeof (v as { name?: unknown }).name === "string") {
    return (v as { name: string }).name;
  }
  return undefined;
}
function num(v: unknown): number | undefined {
  return typeof v === "number" ? v : undefined;
}
function strArr(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}
function isAuthor(v: unknown): v is ReviewTimelineAuthor {
  return typeof v === "object" && v !== null && typeof (v as { kind?: unknown }).kind === "string";
}
function driftNoteOf(v: unknown): string | undefined {
  if (typeof v !== "object" || v === null) return undefined;
  const note = (v as { note?: unknown }).note;
  return typeof note === "string" ? note : undefined;
}

/// One event → one display row's KIND-SPECIFIC fields (`at`/`kind`/`icon`/
/// `label`/`detail`/`href`/`external` only — `author`/`driftNote` are
/// applied uniformly by `timelineRow` below, so no branch here needs to
/// remember to carry them). `repo`/`reviewId` build the in-app deep links
/// (`reviewDiffHref`'s grammar, §5) — the timeline payload itself never
/// carries a full URL.
function coreRow(e: ReviewTimelineEvent, repo: string, reviewId: number): TimelineRow {
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
      const author = authorNameOf(e.author);
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

    // ── V73-K2c — the five new server lanes ──────────────────────────────

    case "pr_body": {
      const path = str(e.path);
      return {
        at,
        kind,
        icon: "pr_body",
        label: "PR description snapshotted",
        detail: str(e.blob_sha)?.slice(0, 10),
        href: path ? reviewDiffHref(repo, reviewId, path) : undefined,
      };
    }

    case "wt_comment": {
      const path = str(e.path);
      const intent = str(e.intent);
      const author = authorNameOf(e.author);
      const isReply = e.is_reply === true;
      const labelParts = ["working-tree", isReply ? "reply" : "comment"];
      if (intent) labelParts.push(intent);
      if (author) labelParts.push(author);
      return {
        at,
        kind,
        icon: "wt_comment",
        label: labelParts.join(" · "),
        detail: path,
        href: path ? readerUrl(repo, path) : undefined,
      };
    }

    case "doc_revision": {
      const revision = num(e.revision);
      const psNumber = num(e.ps_number);
      const tier = str(e.tier);
      return {
        at,
        kind,
        icon: "doc_revision",
        label: `review document revision ${revision ?? "?"}${psNumber != null ? ` (ps${psNumber})` : ""}`,
        detail: tier ? `tier: ${tier}` : undefined,
        href: reviewDiffHref(repo, reviewId, "~review/review.md"),
      };
    }

    case "report": {
      const verdict = str(e.verdict);
      return {
        at,
        kind,
        icon: "report",
        label: verdict ? `agent report: ${verdict}` : "agent report set",
      };
    }

    case "claim": {
      const claimKind = str(e.claim_kind);
      const subject = str(e.subject);
      const state = str(e.state);
      const path = str(e.subject_path);
      return {
        at,
        kind,
        icon: "claim",
        label: `${claimKind ?? "claim"}${subject ? ` on ${subject}` : ""}`,
        detail: state,
        href: path ? readerUrl(repo, path) : undefined,
      };
    }

    case "github_comment": {
      const path = str(e.path);
      return {
        at,
        kind,
        icon: "github",
        label: "GitHub comment",
        href: path ? reviewDiffHref(repo, reviewId, path) : undefined,
        external: str(e.html_url),
      };
    }

    case "turn": {
      const tool = str(e.tool);
      const path = str(e.path);
      const tier = str(e.tier);
      const sessionId = str(e.session_id);
      const turnId = str(e.turn_id);
      return {
        at,
        kind,
        icon: "turn",
        label: `${tool ?? "edit"} touched ${path ?? "?"}`,
        detail: tier,
        href: path ? readerUrl(repo, path) : undefined,
        external: sessionId && turnId ? `${sessionUrl(sessionId)}#${turnId}` : undefined,
      };
    }

    default:
      // Closed vocab server-side, but the client must never crash on an
      // unrecognized/future kind — render it honestly as its own label.
      return { at, kind, icon: "unknown", label: kind === "unknown" ? "unknown event" : kind };
  }
}

/// One event → one display row. Applies `author`/`driftNote` uniformly on
/// top of `coreRow`'s kind-specific fields — both are `undefined` for a
/// v1-shaped event (no envelope at all), which `toEqual` treats as absent,
/// keeping every pre-K2c row byte-identical.
export function timelineRow(e: ReviewTimelineEvent, repo: string, reviewId: number): TimelineRow {
  const core = coreRow(e, repo, reviewId);
  const bodyMd = typeof e.body_md === "string" ? e.body_md : undefined;
  const bodyRefs =
    e.body_refs && typeof e.body_refs === "object" && Array.isArray(e.body_refs.refs)
      ? e.body_refs
      : undefined;
  return {
    ...core,
    author: isAuthor(e.author) ? e.author : undefined,
    driftNote: driftNoteOf(e.drift),
    ...(bodyMd != null ? { bodyMd, bodyRefs } : {}),
  };
}

export function timelineRows(events: ReviewTimelineEvent[], repo: string, reviewId: number): TimelineRow[] {
  return events.map((e) => timelineRow(e, repo, reviewId));
}
