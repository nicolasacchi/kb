// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { SinceYouLeft } from "./SinceYouLeft";
import type { ChangesResponse } from "../api/changes";

afterEach(cleanup);

const SINCE = 1_000;
const NOW_MS = (SINCE + 2 * 3600 + 14 * 60) * 1000;

const RESP: ChangesResponse = {
  since_unix: SINCE,
  to_unix: SINCE + 8_040,
  window_clamped: false,
  projects: [
    {
      project: "orchard",
      label: "orchard",
      source: "slate",
      latest_unix: SINCE + 100,
      sessions: [],
      sessions_total: 0,
      sessions_truncated: false,
      commit_count: 0,
      slate: {
        slug: "orchard",
        head_seq: 1,
        closed: false,
        posts: [{ seq: 1, kind: "hand", line: "hand off the pump", at: SINCE + 50, who: "claude/ab12", closed: false }],
        posts_total: 1,
        posts_truncated: false,
        other_total: 0,
        retracted_total: 0,
        archived_in_window: false,
      },
    },
  ],
  projects_total: 1,
  projects_truncated: false,
  sessions_scan_capped: false,
  trivial_sessions_hidden: 0,
};

function mount(since: number | null, seeded: ChangesResponse | null | undefined, kb: string | null = "canon") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  if (seeded !== undefined) qc.setQueryData(["changes", since], seeded);
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <SinceYouLeft since={since} nowMs={NOW_MS} kb={kb} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("SinceYouLeft", () => {
  it("renders_nothing_without_since", () => {
    const { container } = mount(null, RESP);
    expect(container.querySelector('[data-testid="since-you-left"]')).toBeNull();
    expect(container.innerHTML).toBe("");
  });

  it("renders_nothing_on_empty_or_error", () => {
    const empty = mount(SINCE, { ...RESP, projects: [], projects_total: 0 });
    expect(empty.container.innerHTML).toBe("");
    cleanup();
    // fetchChanges resolves null on any failure; that is what the cache holds.
    const failed = mount(SINCE, null);
    expect(failed.container.innerHTML).toBe("");
  });

  it("renders_rows_and_footer_link", () => {
    const { container, getByText } = mount(SINCE, RESP);
    const strip = container.querySelector('[data-testid="since-you-left"]');
    expect(strip).not.toBeNull();
    expect(strip?.getAttribute("aria-label")).toBe("since you left");
    expect(strip?.querySelector("h3")?.textContent).toContain("Since you left · 2h 14m");
    expect(strip?.querySelector("h3")?.textContent).toContain("across projects");
    expect(strip?.querySelectorAll("li")).toHaveLength(1);
    expect(strip?.textContent).toContain("orchard");
    expect(strip?.textContent).toContain("hand: hand off the pump");
    expect(getByText("orchard").closest("a")?.getAttribute("href")).toBe("/slates/orchard");
    const foot = getByText("Corpus activity since then").closest("a");
    expect(foot?.getAttribute("href")).toBe(`/ambient?since=${SINCE}&kb=canon`);
  });

  it("footer link omits kb fleet-wide", () => {
    const { getByText } = mount(SINCE, RESP, null);
    expect(getByText("Corpus activity since then").closest("a")?.getAttribute("href")).toBe(
      `/ambient?since=${SINCE}`,
    );
  });

  it("has_no_dismiss_control", () => {
    const { container } = mount(SINCE, RESP);
    expect(container.querySelector("button")).toBeNull();
    expect(container.querySelector('[aria-label*="ismiss"]')).toBeNull();
  });
});
