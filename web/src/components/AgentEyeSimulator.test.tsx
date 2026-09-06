// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import AgentEyeSimulator from "./AgentEyeSimulator";
import { fetchRecall, type RecallResponse } from "../api/client";

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return { ...actual, fetchRecall: vi.fn() };
});

function renderSimulator() {
  // retry:false — the failure-path tests below need an immediate `isError`
  // transition, not TanStack's default 3-retry exponential backoff.
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <AgentEyeSimulator />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("AgentEyeSimulator", () => {
  it("shows the idle state and never calls recall for an empty prompt", async () => {
    renderSimulator();
    expect(screen.getByTestId("agent-eye-simulator-idle")).toBeTruthy();
    // Give the debounce a chance to fire — it must not call fetchRecall.
    await new Promise((r) => setTimeout(r, 400));
    expect(fetchRecall).not.toHaveBeenCalled();
  });

  it("renders the exact injection block for a successful recall", async () => {
    vi.mocked(fetchRecall).mockResolvedValueOnce({
      ms: 1,
      hits: [
        {
          id: "aaaaaaaaaaaa",
          kb: "notes",
          title: "ingest retry cap",
          path: "/n.html",
          source_relative: "n.html",
          score: 0.5,
          salience: 0.6,
          pinned: false,
          global: false,
          linked_kbs: [],
          recall_count: 0,
          recall_used_count: 0,
          recall_weekly: [],
          read_pct: 42,
          stopped_at: "usage",
        },
      ],
    } as RecallResponse);

    renderSimulator();
    fireEvent.change(screen.getByTestId("agent-eye-simulator-input"), {
      target: { value: "how does ingest retry work" },
    });

    await waitFor(
      () => {
        expect(screen.getByTestId("agent-eye-simulator-block").textContent).toContain(
          "Relevant memories from kb",
        );
      },
      { timeout: 2000 },
    );
    const block = screen.getByTestId("agent-eye-simulator-block").textContent ?? "";
    expect(block).toContain(
      "- ingest retry cap  [notes]  (id aaaaaaaaaaaa, read 42% — stopped at usage)",
    );
    expect(fetchRecall).toHaveBeenCalledWith(
      expect.objectContaining({ q: "how does ingest retry work", scope: "all", limit: 5 }),
      expect.anything(),
    );
  });

  it("renders a 'no hits' message when recall succeeds with zero hits", async () => {
    vi.mocked(fetchRecall).mockResolvedValueOnce({ ms: 1, hits: [] } as RecallResponse);
    renderSimulator();
    fireEvent.change(screen.getByTestId("agent-eye-simulator-input"), {
      target: { value: "something with no matches" },
    });
    await waitFor(
      () => {
        expect(screen.getByTestId("agent-eye-simulator-nohits")).toBeTruthy();
      },
      { timeout: 2000 },
    );
    expect(screen.queryByTestId("agent-eye-simulator-block")).toBeNull();
  });

  it("surfaces a recall failure as an alert, not a crash", async () => {
    vi.mocked(fetchRecall).mockRejectedValueOnce(new Error("daemon unreachable"));
    renderSimulator();
    fireEvent.change(screen.getByTestId("agent-eye-simulator-input"), {
      target: { value: "trigger a failure" },
    });
    await waitFor(
      () => {
        expect(screen.getByRole("alert").textContent).toContain("daemon unreachable");
      },
      { timeout: 2000 },
    );
  });
});
