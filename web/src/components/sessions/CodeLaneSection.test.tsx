// @vitest-environment jsdom
//
// A5 — the code-lane section: renders nothing without a code_url, shows the
// one compound caption (+ Refresh) when the cross-origin fetch fails,
// renders commit rows with confidence badges and the not-in-checkout
// caption, and links ONLY visible commits. `useKbs` is stubbed so the
// corpus's code_url resolves deterministically.

import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { version as REACT_VERSION } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { CodeLaneSection } from "./CodeLaneSection";
import { COMPOUND_UNAVAILABLE } from "../../lib/codeFreshness";

vi.mock("../../hooks/useKbs", () => ({
  useKbs: () => ({
    data: [{ name: "platform", code_url: "https://code.example.test" }],
    isPending: false,
    error: null,
  }),
}));

const CARD = {
  schema: "kbc-session-card/1",
  sid: "sess-0001",
  now_unix: 1_700_000_000,
  commits: {
    available: true,
    total: 2,
    shown: 2,
    truncated: false,
    items: [
      {
        repo: "fixture",
        sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        subject: "fixture commit one",
        visible: true,
        confidence: "trailer",
        via: "commit-trailer",
        attributed_here: true,
      },
      {
        repo: "fixture",
        sha: "cccccccccccccccccccccccccccccccccccccccc",
        subject: "fixture commit three",
        visible: false,
        caption: "commit not in this checkout",
        confidence: "none",
        via: "not-in-checkout",
        attributed_here: false,
      },
    ],
  },
  reviews: { available: true, total: 0, shown: 0, items: [] },
  findings: {
    available: true,
    total: 0,
    by_severity: { blocker: 0, concern: 0, ok: 0 },
    basis: "author",
  },
  presence: { available: true, files: 0, newest_age_secs: 0 },
};

function wrapper() {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

function fetchCard(body: unknown) {
  const fn = vi.fn(async () =>
    new Response(JSON.stringify(body), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    }),
  );
  vi.stubGlobal("fetch", fn);
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("CodeLaneSection", () => {
  it("renders_nothing_without_code_url", async () => {
    fetchCard(CARD);
    // No kb at all: `useCodeUrlForKb` resolves nothing, the section is
    // absent entirely (not even a skeleton).
    const { container } = render(
      <CodeLaneSection sid="sess-0001" kb={undefined} />,
      { wrapper: wrapper() },
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(container.querySelector('[data-testid="code-lane"]')).toBeNull();
  });

  it("shows_the_compound_unreachable_caption_on_fetch_failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("Failed to fetch");
      }),
    );
    render(<CodeLaneSection sid="sess-0001" kb="platform" />, {
      wrapper: wrapper(),
    });
    await waitFor(() =>
      expect(screen.getByText(COMPOUND_UNAVAILABLE)).toBeTruthy(),
    );
    expect(screen.getByRole("button", { name: "Refresh" })).toBeTruthy();
  });

  it("renders_commit_rows_with_confidence_and_not_in_checkout_caption", async () => {
    fetchCard(CARD);
    render(<CodeLaneSection sid="sess-0001" kb="platform" />, {
      wrapper: wrapper(),
    });
    await waitFor(() => expect(screen.getByText(/trailer/)).toBeTruthy());
    expect(screen.getByText("fixture commit one")).toBeTruthy();
    expect(screen.getByText(/commit not in this checkout/)).toBeTruthy();
  });

  it("link_only_for_visible_commits", async () => {
    fetchCard(CARD);
    render(<CodeLaneSection sid="sess-0001" kb="platform" />, {
      wrapper: wrapper(),
    });
    const link = await screen.findByText("fixture commit one");
    const anchor = link.closest("a");
    expect(anchor?.getAttribute("href")).toBe(
      "https://code.example.test/r/fixture/~commit/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    const notVisible = screen.getByText("fixture commit three");
    expect(notVisible.closest("a")).toBeNull();
  });

  it("renders_under_react_19", async () => {
    expect(REACT_VERSION.startsWith("19.")).toBe(true);
    fetchCard(CARD);
    render(<CodeLaneSection sid="sess-0001" kb="platform" />, {
      wrapper: wrapper(),
    });
    await waitFor(() => expect(screen.getByText(/trailer/)).toBeTruthy());
  });
});
