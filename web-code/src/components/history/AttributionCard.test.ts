import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { LadderAttribution } from "../../api/types";
import { setKbSessionBase } from "../../lib/searchLanes";
import AttributionCard from "./AttributionCard";

// A5 — the session link carries the kb corpus (`?kb=`) when the ladder
// attribution names one, so the kb page opens on the right corpus.
function fixture(overrides: Partial<LadderAttribution> = {}): LadderAttribution {
  return {
    schema: "join/1",
    confidence: "trailer",
    via: "commit-trailer",
    session_id: "sess-0001",
    kb: "platform",
    sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ...overrides,
  };
}

function markup(attribution: LadderAttribution, compact = false): string {
  return renderToStaticMarkup(h(AttributionCard, { attribution, compact }));
}

describe("AttributionCard kb corpus (A5)", () => {
  it("link_carries_the_kb_corpus", () => {
    setKbSessionBase("http://127.0.0.1:4000");
    const html = markup(fixture());
    expect(html).toContain('href="http://127.0.0.1:4000/sessions/sess-0001?kb=platform"');
    expect(html).toContain('data-kbc-attrib-session-kb="platform"');
  });

  it("no_link_when_the_kb_lane_is_unavailable", () => {
    setKbSessionBase("");
    const html = markup(fixture());
    expect(html).not.toContain("data-kbc-attrib-session-link");
    expect(html).not.toContain('href="');
    expect(html).toContain("sess-0001");
    setKbSessionBase("http://127.0.0.1:4000");
  });

  it("compact_variant_unchanged", () => {
    setKbSessionBase("http://127.0.0.1:4000");
    const html = markup(fixture(), true);
    expect(html).toContain("kbc-attrib--compact");
    expect(html).toContain("kbc-attrib__confidence--trailer");
    expect(html).toContain("sess-0001");
  });

  it("absent_kb_keeps_the_unscoped_link", () => {
    setKbSessionBase("http://127.0.0.1:4000");
    const html = markup(fixture({ kb: undefined }));
    expect(html).toContain('href="http://127.0.0.1:4000/sessions/sess-0001"');
    expect(html).not.toContain("data-kbc-attrib-session-kb");
  });
});
