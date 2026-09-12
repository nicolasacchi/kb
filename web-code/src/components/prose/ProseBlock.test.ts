import { createElement as h, type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { StaticRouter } from "react-router";
import { Route, Routes } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import type { FieldRefs, ReviewComment, ReviewDetailPr, ReviewFinding, ReviewReport } from "../../api/types";
import { reviewFindingsKey, reviewReportKey, reviewTimelineKey } from "../../hooks/useReviews";
import { codeUrl } from "../../lib/codeUrl";
import type { DiffCommentsApi } from "../../lib/reviewComments";
import { docBlocks } from "../../lib/reviewDoc";
import ConfirmProvider from "../ConfirmProvider";
import DiffThread from "../diff/DiffThread";
import ClaimRegister from "../reviews/ClaimRegister";
import DocMarkdown from "../reviews/DocMarkdown";
import FindingCard from "../reviews/FindingCard";
import ReportHero from "../reviews/ReportHero";
import ReportPanel from "../reviews/ReportPanel";
import TimelinePanel from "../reviews/TimelinePanel";
import ProseBlock from "./ProseBlock";

const repo = "fixture";
const reviewId = 8;
const revision = "abcdef0123456789abcdef0123456789abcdef01";

function prose(field: string, prefix = "See ") {
  const path = `${field}.rs`;
  const token = `${path}:1`;
  const text = `${prefix}${token}.`;
  const refs: FieldRefs = {
    refs: [{
      kind: "path", text: token, path, line_start: 1,
      span: { start: prefix.length, end: prefix.length + token.length },
      resolution: { state: "exact", path, line: 1 },
    }],
    truncated: false,
  };
  return { text, refs, token, path };
}

function render(element: ReactElement, client = new QueryClient()) {
  try {
    return renderToStaticMarkup(h(QueryClientProvider, { client },
      h(StaticRouter, { location: `/r/${repo}/~reviews/${reviewId}?tab=report` },
        h(ConfirmProvider, { children: h(Routes, null,
          h(Route, { path: "/r/:repo/*", element })) }))));
  } finally {
    client.clear();
  }
}

// Inspect actual rendered anchors, not component source or mocked props. A
// missing text/ref pair, inert span, or wrong field's offsets fails here.
function expectLink(html: string, field: ReturnType<typeof prose>, ref?: string) {
  const anchors = html.match(/<a\b[^>]*data-kbc-prose-ref="path"[^>]*>[\s\S]*?<\/a>/g) ?? [];
  const anchor = anchors.find((a) => a.replace(/<[^>]*>/g, "").startsWith(field.token));
  expect(anchor, `live prose link for ${field.token}`).toBeDefined();
  const href = anchor?.match(/\bhref="([^"]*)"/)?.[1].replaceAll("&amp;", "&");
  expect(href).toBe(codeUrl({ repo, path: field.path, line: 1, ref }));
}

const title = prose("title");
const rationale = prose("rationale", "The bug is in ");
const recommendation = prose("recommendation", "Fix ");
const finding: ReviewFinding = {
  slug: "f-prose-path", severity: "blocker", category: "Correctness",
  location: { kind: "single", path: rationale.path, lines: [1], removed: false },
  title: title.text, title_refs: title.refs,
  rationale: rationale.text, rationale_refs: rationale.refs,
  recommendation: recommendation.text, recommendation_refs: recommendation.refs,
  evidence: null, origin: "import", author: "agent", disposition: null,
  published_state: "unpublished", published_at: null, published_url: null,
  superseded: false, superseded_reason: null, content_updated_at: null,
  annotation_id: "annotation", import_batch_id: "batch", created_at: 1, updated_at: 1,
  resolution: { line: 1, line_end: null, orphaned: false, confidence: "exact" },
  thread_count: 0, unresolved_count: 0,
};
const review: ReviewDetailPr = {
  schema: "review/1", id: reviewId, repo, title: "Review", base_ref: "main", head_ref: "feature-x",
  session_id: null, state: "open", created_at: 1, updated_at: 1, patchsets: [],
  verdict: null, verdict_stale: false,
};

function reportMarkup(report: ReviewReport) {
  const client = new QueryClient();
  client.setQueryData(reviewReportKey(repo, reviewId), report);
  client.setQueryData(reviewFindingsKey(repo, reviewId, { ps: "latest" }), { findings: [finding] });
  return render(h(ReportPanel, { repo, review, ps: "latest", claims: [], onOpenFilesTab: () => {} }), client);
}

function commentsApi(joinedFinding?: ReviewFinding): DiffCommentsApi {
  const unexpected = async (): Promise<never> => { throw new Error("Render must not mutate comments"); };
  return {
    reviewId, ps: "latest", byLine: new Map(), orphansByPath: new Map(),
    findingsById: new Map(joinedFinding ? [["annotation", joinedFinding]] : []), overlay: "all",
    onCreate: unexpected, onCreateFinding: unexpected, onReply: unexpected,
    onResolve: unexpected, onDelete: unexpected, onSetSuggestion: unexpected,
    onClearSuggestion: unexpected, onApplySuggestion: unexpected,
    onSetDisposition: unexpected, onClearDisposition: unexpected,
  };
}

function thread(): ReviewComment {
  return {
    id: "annotation", path: "feature_x.rs", intent: "comment", body: "", author: "agent",
    created_at: 1, updated_at: 1, resolved: false, anchor_kind: "line", side: "new", ps_number: 1,
    resolution: { line: 1, orphaned: false, resolved_against: { ps: 1, sha: revision } },
    suggestion: null, replies: [],
  };
}

describe("prose surface text/ref contract", () => {
  it("FindingCard renders links from title, rationale and recommendation refs", () => {
    const html = render(h(FindingCard, { repo, reviewId, finding }));
    for (const field of [title, rationale, recommendation]) expectLink(html, field);
  });

  it("ReportPanel retains summary, verdict and finding prose links through its zones", () => {
    const summary = prose("summary");
    const verdict = prose("verdict", "Blocked by ");
    // Keep the hero on its deck so it cannot mask a missing summary link.
    const html = reportMarkup({ deck: "Report deck.", summary: summary.text, summary_refs: summary.refs,
      verdict: "concern", verdict_body: verdict.text, verdict_body_refs: verdict.refs });
    for (const field of [summary, verdict, title, rationale, recommendation]) expectLink(html, field);
  });

  it("ReportHero uses deck refs rather than summary refs", () => {
    const deck = prose("deck", "  ");
    const summary = prose("summary");
    const report = { deck: deck.text, deck_refs: deck.refs, summary: summary.text, summary_refs: summary.refs };
    const html = render(h(ReportHero, { repo, review, report, findings: [], files: [], onFilterFindings: () => {} }));
    expectLink(html, deck);
  });

  it("ReportHero preserves summary offsets when selecting the first paragraph", () => {
    const summary = prose("lede", "  ");
    const html = render(h(ReportHero, { repo, review,
      report: { summary: `${summary.text}\n\nSecond paragraph.`, summary_refs: summary.refs },
      findings: [], files: [], onFilterFindings: () => {} }));
    expectLink(html, summary);
  });

  it("ClaimRegister renders the claim body with its refs", () => {
    const body = prose("claim");
    const html = render(h(ClaimRegister, { repo, reviewId, scopeLabel: "this review", claims: [{
      schema: "kbc-claim/1", id: "claim", repo, review_id: reviewId, subject_kind: "review",
      subject: "review", kind: "note", body_md: body.text, refs: body.refs,
      evidence: [], state: "unanchored", caption: "Review claim", created_at: 1,
    }] }));
    expectLink(html, body);
  });

  it("DocMarkdown overlays non-card text, bold and inline-code runs", () => {
    const fields = [prose("doc"), prose("bold", ""), prose("code", "")];
    const text = `${fields[0].text} **${fields[1].token}** and \`${fields[2].token}\``;
    const refs: FieldRefs = { truncated: false, refs: fields.map((field) => ({
      ...field.refs.refs[0], span: { start: text.indexOf(field.token), end: text.indexOf(field.token) + field.token.length },
    })) };
    const html = render(h(DocMarkdown, { blocks: docBlocks(text, new Map(), true), repo, reviewId,
      proseRefs: refs, foldedRefs: new Set<string>(), cardsFolded: false, focusedRef: null, onToggleFold: () => {} }));
    for (const field of fields) expectLink(html, field);
  });

  it("TimelinePanel retains body refs through timelineRows", () => {
    const body = prose("timeline");
    const client = new QueryClient();
    client.setQueryData(reviewTimelineKey(repo, reviewId, { github: false, limit: 100 }), {
      schema: "review-timeline/2", events: [{ at: 1, kind: "report", lane: "report", body_md: body.text, body_refs: body.refs }],
      sources: [], total: 1, returned: 1, offset: 0, limit: 100, filters: { kinds: [] },
    });
    expectLink(render(h(TimelinePanel, { repo, reviewId }), client), body);
  });

  it("DiffThread retains finding prose refs in its joined-finding branch", () => {
    const html = render(h(DiffThread, { thread: thread(), comments: commentsApi(finding) }));
    for (const field of [title, rationale, recommendation]) expectLink(html, field);
  });

  it("DiffThread retains comment and reply body refs", () => {
    const body = prose("comment");
    const reply = prose("reply", "Also see ");
    const comment = { ...thread(), body: body.text, body_refs: body.refs,
      replies: [{ id: "reply", parent_id: "annotation", path: "feature_x.rs", intent: "comment",
        body: reply.text, body_refs: reply.refs, author: "agent", created_at: 1, updated_at: 1, resolved: false }] };
    const html = render(h(DiffThread, { thread: comment, comments: commentsApi() }));
    expectLink(html, body);
    expectLink(html, reply);
  });
});

describe("rendered path Location Contract", () => {
  it("uses codeUrl for a resolved patchset path and line", () => {
    const field = prose("feature_x", "The bug is in ");
    const refs = { ...field.refs, refs: field.refs.refs.map((r) => ({ ...r,
      path: "old.rs", line_start: 99, resolution: { ...r.resolution!, ref: revision },
    })) };
    expectLink(render(h(ProseBlock, { text: field.text, refs, repo, reviewId })), field, revision);
  });

  it("keeps a genuinely orphaned path visible but non-navigable", () => {
    const field = prose("missing");
    const refs = { ...field.refs, refs: field.refs.refs.map((r) => ({ ...r, resolution: { state: "orphan" } })) };
    const html = render(h(ProseBlock, { text: field.text, refs, repo, reviewId }));
    expect(html).toContain(field.token);
    expect(html).not.toMatch(/<a\b/);
    expect(html).toContain('data-kbc-prose-orphan=""');
  });
});
