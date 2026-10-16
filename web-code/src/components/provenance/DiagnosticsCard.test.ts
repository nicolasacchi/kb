import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import type { LaneFactOut } from "../../api/types";
import DiagnosticsCard from "./DiagnosticsCard";

const fact = {
  lane: "rubocop",
  kind: "diagnostic",
  value: { message: "Style/FrozenStringLiteralComment: missing", cop: "Style/Frozen" },
  severity: "warning",
  line: 3,
  line_end: null,
} as unknown as LaneFactOut;

describe("DiagnosticsCard row layout (OL-P5)", () => {
  it("keeps the message and the lane meta inside one body, message first", () => {
    const client = new QueryClient();
    let html = "";
    try {
      html = renderToStaticMarkup(
        h(
          QueryClientProvider,
          { client },
          h(DiagnosticsCard, {
            repo: "r",
            path: "a.rb",
            laneFacts: [fact],
            onJumpLine: () => {},
          }),
        ),
      );
    } finally {
      client.clear();
    }
    const body = html.match(/<span class="kbc-diagnostics__body">(.*?)<\/span><\/span><\/button>/s);
    expect(body).not.toBeNull();
    const inner = body![1];
    const msg = inner.indexOf("kbc-diagnostics__message");
    const meta = inner.indexOf("kbc-diagnostics__meta");
    expect(msg).toBeGreaterThanOrEqual(0);
    expect(meta).toBeGreaterThan(msg);
    expect(inner).toContain("lane:rubocop");
  });
});
