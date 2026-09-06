// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import HygieneQueue from "./HygieneQueue";
import {
  fetchMemoryTriage,
  forgetMemory,
  patchMemorySalience,
  pinMemory,
  type MemoryTriageItem,
  type MemoryTriageResponse,
} from "../api/client";

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return {
    ...actual,
    fetchMemoryTriage: vi.fn(),
    pinMemory: vi.fn(),
    patchMemorySalience: vi.fn(),
    forgetMemory: vi.fn(),
  };
});

function renderQueue() {
  // retry:false — the failure-path tests below need an immediate `isError`
  // transition, not TanStack's default 3-retry exponential backoff.
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <HygieneQueue />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const ITEM: MemoryTriageItem = {
  kb: "globalmem",
  id: "aaaaaaaaaaaa",
  title: "Old Fact",
  source_relative: "old.html",
  reason_kind: "below_floor_now",
  reason: "salience 0.10 is at/below the 0.15 floor — excluded from recall now",
  urgency: 0.8,
  salience: 0.1,
  floor: 0.15,
};

/** FIX2 — `high_salience_threshold`/`dormant_days` are wire-supplied
 * constants (kb_core::triage::HIGH_SALIENCE_THRESHOLD/DORMANT_DAYS),
 * REQUIRED on every real `MemoryTriageResponse`. One helper here so every
 * fixture below carries them without re-typing the real values at each
 * call site. */
function triageResponse(items: MemoryTriageItem[], scanned: number): MemoryTriageResponse {
  return { items, scanned, high_salience_threshold: 0.7, dormant_days: 60 };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("HygieneQueue", () => {
  it("renders the empty state honestly", async () => {
    vi.mocked(fetchMemoryTriage).mockResolvedValueOnce(triageResponse([], 42));
    renderQueue();
    await waitFor(() => expect(screen.getByTestId("hygiene-queue-empty")).toBeTruthy());
  });

  it("surfaces a fetch failure as an alert", async () => {
    vi.mocked(fetchMemoryTriage).mockRejectedValueOnce(new Error("daemon down"));
    renderQueue();
    await waitFor(() => {
      expect(screen.getByRole("alert").textContent).toContain("daemon down");
    });
  });

  it("renders a queued item with its justification and urgency", async () => {
    vi.mocked(fetchMemoryTriage).mockResolvedValueOnce(triageResponse([ITEM], 42));
    renderQueue();
    await waitFor(() => {
      expect(screen.getByText("Old Fact")).toBeTruthy();
    });
    expect(screen.getByTestId("hygiene-queue-reason").textContent).toBe(
      "salience 0.10 is at/below the 0.15 floor — excluded from recall now",
    );
    expect(screen.getByText("0.80")).toBeTruthy();
  });

  it("pin action calls pinMemory and refreshes the queue on success", async () => {
    vi.mocked(fetchMemoryTriage)
      .mockResolvedValueOnce(triageResponse([ITEM], 1))
      .mockResolvedValueOnce(triageResponse([], 1));
    vi.mocked(pinMemory).mockResolvedValueOnce({ pinned: true });
    renderQueue();
    await waitFor(() => expect(screen.getByText("Old Fact")).toBeTruthy());

    fireEvent.click(screen.getByText("pin"));

    await waitFor(() => {
      expect(pinMemory).toHaveBeenCalledWith("globalmem", "aaaaaaaaaaaa");
    });
    await waitFor(() => expect(screen.getByTestId("hygiene-queue-empty")).toBeTruthy());
  });

  it("a rejected pin shows an inline error and leaves the item in place", async () => {
    vi.mocked(fetchMemoryTriage).mockResolvedValue(triageResponse([ITEM], 1));
    vi.mocked(pinMemory).mockRejectedValueOnce(new Error("network error"));
    renderQueue();
    await waitFor(() => expect(screen.getByText("Old Fact")).toBeTruthy());

    fireEvent.click(screen.getByText("pin"));

    await waitFor(() => {
      expect(screen.getByRole("alert").textContent).toContain("pin failed");
    });
    // The item is still there — a failed mutation never silently vanishes.
    expect(screen.getByText("Old Fact")).toBeTruthy();
  });

  it("forget action calls forgetMemory", async () => {
    vi.mocked(fetchMemoryTriage).mockResolvedValue(triageResponse([ITEM], 1));
    vi.mocked(forgetMemory).mockResolvedValueOnce(undefined);
    renderQueue();
    await waitFor(() => expect(screen.getByText("Old Fact")).toBeTruthy());
    fireEvent.click(screen.getByText("forget"));
    await waitFor(() => {
      expect(forgetMemory).toHaveBeenCalledWith("globalmem", "aaaaaaaaaaaa");
    });
  });

  it("+salience action bumps salience by 0.2, capped at 1", async () => {
    vi.mocked(fetchMemoryTriage).mockResolvedValue(
      triageResponse([{ ...ITEM, salience: 0.9 }], 1),
    );
    vi.mocked(patchMemorySalience).mockResolvedValueOnce({
      id: "aaaaaaaaaaaa",
      salience: 1,
    });
    renderQueue();
    await waitFor(() => expect(screen.getByText("Old Fact")).toBeTruthy());
    fireEvent.click(screen.getByText("+salience"));
    await waitFor(() => {
      expect(patchMemorySalience).toHaveBeenCalledWith("globalmem", "aaaaaaaaaaaa", 1);
    });
  });
});
