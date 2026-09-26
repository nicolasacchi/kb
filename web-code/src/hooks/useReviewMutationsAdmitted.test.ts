// V80-F2 — `useReviewMutationsAdmitted` over a REAL `QueryClientProvider`.
// `renderToStaticMarkup` (no jsdom, no React Testing Library) is this
// repo's established way to vitest-pin a hook's return value through a
// tiny rendered probe (`components/prose/ProseBlock.test.ts`'s
// `client.setQueryData(...)` precedent, same idiom applied to a hook
// instead of a component). Pre-seeding the `["identity"]` cache entry
// `useIdentity` reads is enough — `useIdentity`'s own `staleTime:
// Infinity` (see that hook's doc) means a query that already has data at
// mount never refetches, so no case here ever makes a network attempt.
//
// V80-F2b — a real compatibility bug caught post-merge (`review-room.spec.ts`
// failed against a server binary predating this field: the disposition
// button rendered disabled+captioned even though the loopback gate would
// have admitted the write). `admitted` must be `true` UNLESS the daemon
// EXPLICITLY said `false` — an absent field (older daemon), a failed
// fetch, and an in-flight one all read `admitted: true` / `unknown: true`
// now, never `admitted: false`.

import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { IdentityOut } from "../api/types";
import { useReviewMutationsAdmitted } from "./useReviewMutationsAdmitted";

function Probe() {
  const { admitted, unknown } = useReviewMutationsAdmitted();
  return h("div", { "data-admitted": String(admitted), "data-unknown": String(unknown) });
}

/// `seed === undefined` simulates "no identity fetch has landed yet" via
/// `defaultOptions.queries.enabled: false` (a real react-query knob, not a
/// mock) — `useIdentity`'s query then never calls `fetchIdentity` at all.
/// Every OTHER case pre-seeds the cache directly instead, so no case here
/// ever touches the network.
function markup(seed: Partial<IdentityOut> | undefined): string {
  const client =
    seed === undefined
      ? new QueryClient({ defaultOptions: { queries: { enabled: false } } })
      : new QueryClient();
  if (seed !== undefined) {
    client.setQueryData<IdentityOut>(["identity"], {
      name: "kb-code",
      version: "0.0.0",
      ...seed,
    });
  }
  try {
    return renderToStaticMarkup(h(QueryClientProvider, { client }, h(Probe)));
  } finally {
    client.clear();
  }
}

describe("useReviewMutationsAdmitted", () => {
  it("reads admitted:true, unknown:false when the daemon's per-request probe explicitly said so", () => {
    const html = markup({ review_mutations_admitted: true });
    expect(html).toContain('data-admitted="true"');
    expect(html).toContain('data-unknown="false"');
  });

  it("reads admitted:false ONLY on an explicit false (the [review] remote_mutations default for a non-loopback caller)", () => {
    const html = markup({ review_mutations_admitted: false });
    expect(html).toContain('data-admitted="false"');
    expect(html).toContain('data-unknown="false"');
  });

  it("V80-F2b: an older daemon that omits the field entirely reads admitted:true (never pre-empts a write it might admit), unknown:true", () => {
    const html = markup({});
    expect(html).toContain('data-admitted="true"');
    expect(html).toContain('data-unknown="true"');
  });

  it("V80-F2b: no identity fetch has landed yet (cold cache) reads admitted:true, unknown:true", () => {
    const html = markup(undefined);
    expect(html).toContain('data-admitted="true"');
    expect(html).toContain('data-unknown="true"');
  });
});
