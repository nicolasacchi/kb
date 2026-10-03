// The Rebind / Unbind gating of one annotation thread (X5, X2 carry).
//
// `ThreadItem` offers "Bind to review…" / "Rebind" and "Unbind" only when the
// thread does NOT back a finding -- the daemon answers 409 for those, so the
// controls are replaced by a `kbc-annotations__bound-note` caption. The pure
// predicate (`annotationBacksFinding`) is unit-pinned elsewhere; this renders
// the real component so the gating itself (which control appears for which
// state) cannot regress unseen. No DOM: `renderToStaticMarkup` + a seeded
// query cache, this repo's idiom for a component under `environment: "node"`
// (`components/prose/ProseBlock.test.ts`). SSR runs no effects, so the
// findings read is the seeded entry, never a fetch.

import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { StaticRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import type { AnnotationView } from "../../api/types";
import { reviewFindingsKey } from "../../hooks/useReviews";
import type { AnnotationThread } from "../../lib/annotations";
import { ThreadItem } from "./AnnotationsPanel";

const REPO = "fixture";

function annotation(overrides: Partial<AnnotationView> = {}): AnnotationView {
  return {
    id: "ann-1",
    repo: REPO,
    path: "src/lib.rs",
    anchor: {},
    anchor_kind: "line",
    intent: "note",
    parent_id: null,
    body: "a note",
    author: "you",
    created_at: 1000,
    updated_at: 1000,
    resolved: false,
    line: 1,
    stale: false,
    line_end: null,
    sha: null,
    ...overrides,
  };
}

function render(parent: AnnotationView, findingAnnotationIds: string[] | null): string {
  const client = new QueryClient();
  if (findingAnnotationIds !== null && parent.review_id !== undefined) {
    client.setQueryData(
      reviewFindingsKey(REPO, parent.review_id, { include_superseded: true }),
      { findings: findingAnnotationIds.map((annotation_id) => ({ annotation_id })) },
    );
  }
  const thread: AnnotationThread = { parent, replies: [] };
  const noop = async () => true;
  try {
    return renderToStaticMarkup(
      h(
        QueryClientProvider,
        { client },
        h(
          StaticRouter,
          { location: "/" },
          h(ThreadItem, {
            repo: REPO,
            thread,
            onGotoLine: () => {},
            onToggleResolved: () => {},
            onDelete: () => {},
            onReply: noop,
            reviewTitleFor: () => undefined,
            onBindReview: noop,
            onUnbindReview: async () => {},
          }),
        ),
      ),
    );
  } finally {
    client.clear();
  }
}

describe("ThreadItem review-binding controls", () => {
  it("an unbound thread offers 'Bind to review…' and no Unbind", () => {
    const html = render(annotation(), null);
    expect(html).toContain("Bind to review…");
    expect(html).toContain("data-kbc-annot-review-toggle");
    expect(html).not.toContain("data-kbc-annot-review-unbind");
    expect(html).not.toContain("kbc-annotations__bound-note");
  });

  it("a bound thread that backs no finding offers Rebind and Unbind", () => {
    const html = render(annotation({ review_id: 7 }), ["some-other-annotation"]);
    expect(html).toContain("Rebind");
    expect(html).toContain("data-kbc-annot-review-unbind");
    expect(html).not.toContain("data-kbc-annot-backs-finding");
  });

  it("a thread that backs a finding offers neither, and says why", () => {
    const html = render(annotation({ id: "ann-1", review_id: 7 }), ["ann-1"]);
    expect(html).not.toContain("data-kbc-annot-review-toggle");
    expect(html).not.toContain("data-kbc-annot-review-unbind");
    expect(html).not.toContain("Rebind");
    expect(html).toContain("kbc-annotations__bound-note");
    expect(html).toContain("data-kbc-annot-backs-finding");
    expect(html).toContain("review binding is fixed");
  });
});
