// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import WaitingChip from "./WaitingChip";
import { fetchLiveStatus, type LiveStatusRow } from "../../api/sessions";

vi.mock("../../api/sessions", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sessions")>();
  return {
    ...actual,
    fetchLiveStatus: vi.fn(),
  };
});

function row(overrides: Partial<LiveStatusRow> & Pick<LiveStatusRow, "session_id">): LiveStatusRow {
  return {
    harness: "claude",
    holder: "human",
    state: "waiting",
    source: "hook",
    confidence: "observed",
    since_unix: 1_800_000_000,
    since_secs: 0,
    resume: `claude -r ${overrides.session_id}`,
    blocked: false,
    ...overrides,
  };
}

function renderChip() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <WaitingChip />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("WaitingChip", () => {
  it("renders nothing (no DOM node) when nobody is waiting", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValueOnce([
      row({ session_id: "a", state: "working" }),
    ]);
    const { container } = renderChip();
    await waitFor(() => expect(fetchLiveStatus).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
    expect(screen.queryByTestId("header-waiting")).toBeNull();
  });

  it("renders nothing when the fleet is empty", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValueOnce([]);
    const { container } = renderChip();
    await waitFor(() => expect(fetchLiveStatus).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it("shows the count when N sessions are waiting, and links to the waiting lane", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValueOnce([
      row({ session_id: "a", state: "waiting" }),
      row({ session_id: "b", state: "cold" }),
      row({ session_id: "c", state: "working" }),
    ]);
    renderChip();
    await waitFor(() => {
      expect(screen.getByTestId("header-waiting")).toBeTruthy();
    });
    const chip = screen.getByTestId("header-waiting");
    expect(chip.textContent).toContain("2");
    expect(chip.getAttribute("href")).toBe("/sessions#kb-now-waiting");
  });
});
