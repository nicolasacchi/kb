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

  it("OL IDEA1: blocked only -> 'N blocked', danger class, links to the blocked lane", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValueOnce([
      row({ session_id: "a", state: "working", holder: "agent", blocked: true }),
    ]);
    renderChip();
    await waitFor(() => expect(screen.getByTestId("header-waiting")).toBeTruthy());
    const chip = screen.getByTestId("header-waiting");
    expect(chip.textContent).toBe("1 blocked");
    expect(chip.getAttribute("href")).toBe("/sessions#kb-now-blocked");
    expect(chip.className).toBe("kb-waiting kb-waiting--blocked");
    expect(chip.hasAttribute("data-kb-blocked")).toBe(true);
    expect(chip.getAttribute("aria-label")).toBe("1 session blocked on you");
  });

  it("OL IDEA1: blocked + waiting -> both counts and the plural aria text", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValueOnce([
      row({ session_id: "a", state: "working", holder: "agent", blocked: true }),
      row({ session_id: "b", state: "stalled", holder: "agent", blocked: true }),
      row({ session_id: "c", state: "waiting" }),
    ]);
    renderChip();
    await waitFor(() => expect(screen.getByTestId("header-waiting")).toBeTruthy());
    const chip = screen.getByTestId("header-waiting");
    expect(chip.textContent).toBe("2 blocked · 1 waiting");
    expect(chip.getAttribute("title")).toBe("2 sessions blocked on you · 1 waiting on you");
    expect(chip.getAttribute("aria-label")).toBe("2 sessions blocked on you · 1 waiting on you");
  });

  it("OL IDEA1: with no blocked rows the chip is the plain waiting chip", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValueOnce([row({ session_id: "a", state: "waiting" })]);
    renderChip();
    await waitFor(() => expect(screen.getByTestId("header-waiting")).toBeTruthy());
    const chip = screen.getByTestId("header-waiting");
    expect(chip.className).toBe("kb-waiting");
    expect(chip.hasAttribute("data-kb-blocked")).toBe(false);
  });
});
