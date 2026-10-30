import { describe, expect, it } from "vitest";
import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { StaticRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { PresenceChip, PresenceLine } from "./PresencePulse";
import { presenceKey } from "../hooks/usePresence";
import type { PresenceSnapshot } from "../lib/presence";

function client(files: PresenceSnapshot["files"] | null): QueryClient {
  const c = new QueryClient();
  if (files) {
    const snap: PresenceSnapshot = {
      schema: "kbc-presence/1",
      available: true,
      now_unix: 0,
      files,
      fetchedPerf: performance.now(),
    };
    c.setQueryData(presenceKey("r"), snap);
  }
  return c;
}
const row = (path: string, state = "working") => ({
  repo: "r",
  path,
  session_id: "3f2a9c",
  harness: "claude",
  state,
  age_secs: 5,
});
const render = (c: QueryClient, el: ReturnType<typeof h>) =>
  renderToStaticMarkup(h(QueryClientProvider, { client: c }, h(StaticRouter, { location: "/" }, el)));

describe("presence pulse (Home line + Spine chip)", () => {
  it("renders the line from the shared presence query", () => {
    const html = render(client([row("a.rs"), row("b.rs")]), h(PresenceLine, { repo: "r" }));
    expect(html).toContain("data-kbc-home-presence");
    expect(html).toContain("1 session working · 2 files in 10 min");
  });
  it("renders the chip with a working count", () => {
    const html = render(client([row("a.rs")]), h(PresenceChip, { repo: "r" }));
    expect(html).toContain("data-kbc-topbar-presence");
    expect(html).toContain("1 working");
  });
  it("renders nothing when quiet or unseeded", () => {
    expect(render(client([]), h(PresenceLine, { repo: "r" }))).toBe("");
    expect(render(client(null), h(PresenceChip, { repo: "r" }))).toBe("");
    expect(render(client([row("a.rs")]), h(PresenceChip, { repo: undefined }))).toBe("");
  });
});
