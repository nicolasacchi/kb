import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { StaticRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import type { RepoListEntry } from "../../api/types";
import RepoCard from "./RepoCard";

// V77-P2 (task 3) — the "catching up" chip beside the existing watcher
// badge. `renderToStaticMarkup` (no effects, no hydration) is this repo's
// established way to vitest-pin a component's rendered markup without
// React Testing Library (see `components/prose/ProseBlock.test.ts`'s same
// pattern) — `useBranches`/`useRecentFiles` are left unseeded (they render
// their honest loading skeleton, never touched by this test) since neither
// bears on the header chip this unit adds.
function fixtureRepo(overrides: Partial<RepoListEntry> = {}): RepoListEntry {
  return {
    name: "fixture",
    path: "/repos/fixture",
    file_count: 42,
    symbol_count: 420,
    head: null,
    watcher: "watching",
    intel: null,
    catching_up: false,
    settled_at: null,
    ...overrides,
  };
}

function markup(repo: RepoListEntry): string {
  const client = new QueryClient();
  try {
    return renderToStaticMarkup(
      h(
        QueryClientProvider,
        { client },
        h(StaticRouter, { location: "/" }, h(RepoCard, { repo })),
      ),
    );
  } finally {
    client.clear();
  }
}

describe("RepoCard catching-up chip (V77-P2)", () => {
  it("renders the data-kbc-catching-up hook while a slow-lane job is outstanding", () => {
    const html = markup(fixtureRepo({ catching_up: true, settled_at: null }));
    expect(html).toContain("data-kbc-catching-up");
    expect(html).toMatch(/catching up/);
  });

  it("omits the chip once the repo has settled", () => {
    const html = markup(fixtureRepo({ catching_up: false, settled_at: 1_757_000_000 }));
    expect(html).not.toContain("data-kbc-catching-up");
  });

  it("omits the chip for a repo the daemon has never run slow-lane work for", () => {
    // `catching_up: false` + `settled_at: null` — the honest "nothing to
    // catch up on" state (sink::RepoActivity's doc), distinct from
    // "settled" but rendered identically: no chip either way.
    const html = markup(fixtureRepo({ catching_up: false, settled_at: null }));
    expect(html).not.toContain("data-kbc-catching-up");
  });

  it("still renders the existing watcher badge beside the new chip", () => {
    const html = markup(fixtureRepo({ catching_up: true, watcher: "polling" }));
    expect(html).toContain('data-kbc-watcher-state="polling"');
    expect(html).toContain("data-kbc-catching-up");
  });
});
