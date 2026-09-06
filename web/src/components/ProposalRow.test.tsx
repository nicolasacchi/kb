// @vitest-environment jsdom
//
// MI-W3.R — the supersedes-target-title resolution (`useQuery` → `fetchDoc`,
// rendered as `.proposal-row__supersedes`) had zero coverage. The safety
// property that matters is the fallback: a proposal whose supersede target
// can no longer be resolved (already deleted/purged) must still render, and
// Approve/Reject must stay usable — never blocked by a failed lookup.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import ProposalRow from "./ProposalRow";
import ConfirmProvider from "./ConfirmProvider";
import { fetchDoc, type DocSummary, type ProposalItem } from "../api/client";

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return {
    ...actual,
    fetchDoc: vi.fn(),
  };
});

function makeItem(overrides: Partial<ProposalItem> = {}): ProposalItem {
  return {
    kb: "mem",
    id: "p_abc123def456",
    schema: "kb-proposal/1",
    created_at: 1_700_000_000,
    title: "A candidate memory",
    body: "the candidate body text",
    category: "memory-user",
    tags: [],
    global: true,
    linked_kbs: [],
    supersedes: "sup123456789",
    source: "agent",
    ...overrides,
  };
}

function renderRow(item: ProposalItem) {
  const queryClient = new QueryClient();
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <ConfirmProvider>
          <ProposalRow item={item} onApprove={vi.fn()} onReject={vi.fn()} />
        </ConfirmProvider>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("ProposalRow — supersedes-target title resolution", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders the resolved title when fetchDoc succeeds", async () => {
    vi.mocked(fetchDoc).mockResolvedValueOnce({
      id: "sup123456789",
      title: "Old memory being retired",
      path: "/srv/mem/old.html",
      folder: "",
      source_relative: "old.html",
    } as DocSummary);

    renderRow(makeItem());

    await waitFor(() => {
      expect(screen.getByTestId("proposal-row-supersedes").textContent).toContain(
        "Old memory being retired",
      );
    });
  });

  it("falls back to the bare id — and stays approvable — when fetchDoc fails", async () => {
    vi.mocked(fetchDoc).mockRejectedValueOnce(new Error("404 not found"));

    renderRow(makeItem());

    await waitFor(() => {
      expect(screen.getByTestId("proposal-row-supersedes").textContent).toContain(
        "sup123456789",
      );
    });
    // The failed lookup is cosmetic only — the row never disables or
    // errors out over it.
    expect(screen.queryByRole("alert")).toBeNull();
    const approve = screen.getByText("Approve") as HTMLButtonElement;
    expect(approve.disabled).toBe(false);
  });
});
