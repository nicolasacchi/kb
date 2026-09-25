// V80-F2 — the ONE place the disabled-with-caption state under an
// EXPLICIT `review_mutations_admitted: false` is exercisable at all: the
// loopback Playwright harness (`e2e/review-room.spec.ts`) always boots
// loopback, so `admitted` is always `true` there and the segment is never
// disabled — see that spec's own note. `renderToStaticMarkup` + a
// pre-seeded `["identity"]` cache entry (this repo's established pattern,
// see `hooks/useReviewMutationsAdmitted.test.ts`'s same technique) drives
// `useReviewMutationsAdmitted` with no mocking at all; `usePutReviewVerdict`'s
// mutation is never triggered by this test (no click, just rendered
// markup), so it needs no seed/mock either.
//
// V80-F2b — a real compatibility bug caught post-merge: an ABSENT field
// (an older daemon) must read as ENABLED, not disabled — see the
// "degrades to ENABLED" case below, which used to (wrongly) assert
// disabled.

import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { StaticRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { IdentityOut, ReviewDetail } from "../../api/types";
import VerdictBar from "./VerdictBar";

const REVIEW: ReviewDetail = {
  schema: "kbc-review/1",
  id: 7,
  repo: "r",
  title: "a title",
  base_ref: "main",
  head_ref: "feature",
  session_id: null,
  state: "open",
  created_at: 0,
  updated_at: 0,
  patchsets: [],
  verdict: null,
  verdict_stale: false,
  base: { mode: null, branch: null, set_by: "legacy", source: null, state: null, merge_base: null, fetched_at: null, last_fetch: null, fetched_via: null },
  warnings: [],
};

/// `admitted === undefined` seeds the identity cache WITHOUT the field at
/// all (an older daemon that never sends it).
function markup(admitted: boolean | undefined): string {
  const client = new QueryClient();
  client.setQueryData<IdentityOut>(["identity"], {
    name: "kb-code",
    version: "0.0.0",
    ...(admitted !== undefined ? { review_mutations_admitted: admitted } : {}),
  });
  try {
    return renderToStaticMarkup(
      h(
        QueryClientProvider,
        { client },
        h(StaticRouter, { location: "/" }, h(VerdictBar, { repo: "r", reviewId: 7, review: REVIEW })),
      ),
    );
  } finally {
    client.clear();
  }
}

describe("VerdictBar", () => {
  it("renders the segment ENABLED when the pre-probe admits this caller", () => {
    const html = markup(true);
    expect(html).not.toContain("disabled");
    expect(html).not.toContain("data-kbc-review-verdict-loopback");
  });

  it("renders the segment DISABLED-WITH-CAPTION, never hidden, when the pre-probe refuses", () => {
    const html = markup(false);
    // Never hidden — root CLAUDE.md's honesty rule ("a filter names what
    // it hides"): all three verdict buttons stay in the markup, each
    // `disabled` and carrying the caption as its `title`.
    const buttons = html.match(/data-kbc-review-verdict="[^"]+"/g) ?? [];
    expect(buttons.length).toBe(3);
    expect((html.match(/disabled=""/g) ?? []).length).toBe(3);
    expect(html).toContain('title="review writes are loopback-only');
    expect(html).toContain("data-kbc-review-verdict-loopback");
    expect(html).toContain("set [review] remote_mutations = true");
  });

  it("V80-F2b: degrades to ENABLED, no caption — an older daemon that omits the field entirely must never be pre-empted", () => {
    const html = markup(undefined);
    expect(html).not.toContain("disabled");
    expect(html).not.toContain("data-kbc-review-verdict-loopback");
  });
});
