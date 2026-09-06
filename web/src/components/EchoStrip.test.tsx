// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { EchoStrip } from "./EchoStrip";
import { fetchEchoes, type EchoesResponseWithBeliefs } from "../api/client";

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return {
    ...actual,
    fetchEchoes: vi.fn(),
  };
});

function renderStrip(kb: string | null = "kb1") {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <EchoStrip kb={kb} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function response(
  overrides: Partial<EchoesResponseWithBeliefs> = {},
): EchoesResponseWithBeliefs {
  return {
    items: [],
    today: "2026-08-21",
    beliefs: [],
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  sessionStorage.clear();
});

describe("EchoStrip — CT-E6 beliefs lane", () => {
  it("renders nothing when both items and beliefs are empty", async () => {
    vi.mocked(fetchEchoes).mockResolvedValueOnce(response());
    const { container } = renderStrip();
    await waitFor(() => expect(fetchEchoes).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it("shows a belief row even when there are no anniversary items", async () => {
    vi.mocked(fetchEchoes).mockResolvedValueOnce(
      response({
        beliefs: [
          {
            id: "mem1",
            kb: "kb1",
            title: "the deploy needs the token",
            created: 1_700_000_000,
            months_ago: 12,
            status: "active",
          },
        ],
      }),
    );
    renderStrip();
    await waitFor(() => {
      expect(screen.getByText(/the deploy needs the token/)).toBeTruthy();
    });
    expect(screen.getByText(/still active/)).toBeTruthy();
    const link = screen.getByText(/the deploy needs the token/).closest("a");
    expect(link?.getAttribute("href")).toBe("/memory?kb=kb1");
  });

  it("renders the superseded and forgotten statuses honestly", async () => {
    vi.mocked(fetchEchoes).mockResolvedValueOnce(
      response({
        beliefs: [
          {
            id: "old-mem",
            kb: "kb1",
            title: "old fact",
            created: 1_700_000_000,
            months_ago: 12,
            status: "superseded",
            superseded_by: "new-mem",
            superseded_at: 1_700_100_000,
          },
          {
            id: "gone-mem",
            kb: "kb1",
            title: "gone fact",
            created: 1_700_000_000,
            months_ago: 24,
            status: "forgotten",
          },
        ],
      }),
    );
    renderStrip();
    await waitFor(() => expect(screen.getByText(/old fact/)).toBeTruthy());
    expect(screen.getByText(/superseded 2023-11-16/)).toBeTruthy();
    expect(screen.getByText(/gone fact/)).toBeTruthy();
    expect(screen.getByText(/forgotten/)).toBeTruthy();
  });

  it("surfaces the epoch-honesty caveat when present", async () => {
    vi.mocked(fetchEchoes).mockResolvedValueOnce(
      response({
        beliefs: [
          {
            id: "mem1",
            kb: "kb1",
            title: "old fact",
            created: 1_700_000_000,
            months_ago: 36,
            status: "active",
          },
        ],
        beliefs_tombstone_caveat: "this anniversary window reaches back to 123 unix, before 456 unix",
      }),
    );
    renderStrip();
    await waitFor(() => {
      expect(
        screen.getByText(/reaches back to 123 unix, before 456 unix/),
      ).toBeTruthy();
    });
  });
});
