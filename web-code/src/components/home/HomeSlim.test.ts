import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { StaticRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import type { RepoListEntry } from "../../api/types";
import DaemonStatus from "./DaemonStatus";
import RepoCard from "./RepoCard";

const repo: RepoListEntry = {
  name: "fixture",
  path: "/repos/fixture",
  file_count: 1,
  symbol_count: 2,
  head: null,
  watcher: "watching",
  intel: null,
  catching_up: false,
  settled_at: null,
};

function markup(el: ReturnType<typeof h>): string {
  const client = new QueryClient();
  try {
    return renderToStaticMarkup(
      h(QueryClientProvider, { client }, h(StaticRouter, { location: "/" }, el)),
    );
  } finally {
    client.clear();
  }
}

describe("Home slimming (OL-C7)", () => {
  it("RepoCard no longer carries the Continue/Recent duplicates or store diagnostics", () => {
    const html = markup(h(RepoCard, { repo }));
    expect(html).not.toContain("data-kbc-home-continue");
    expect(html).not.toContain("data-kbc-home-recent");
    expect(html).not.toContain("data-kbc-home-store");
    expect(html).not.toContain("Fetch credential");
    expect(html).toContain("data-kbc-home-branches");
  });

  it("Daemon status is one link to Settings, with no diagnostics body", () => {
    const html = markup(h(DaemonStatus));
    expect(html).not.toContain("<details");
    expect(html).toContain('href="/~settings"');
    expect(html).toContain("Daemon status");
    expect(html).not.toContain("data-kbc-home-store");
  });
});
});
