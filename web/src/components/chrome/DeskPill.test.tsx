// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import DeskPill from "./DeskPill";
import { fetchDesk, type DeskItem, type DeskResponse } from "../../api/desk";
import { artifactHref } from "../../lib/artifactHref";

vi.mock("../../api/desk", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/desk")>();
  return {
    ...actual,
    fetchDesk: vi.fn(),
  };
});

function item(overrides: Partial<DeskItem> = {}): DeskItem {
  return {
    kb: "docs",
    id: "abc123def456",
    source_relative: "handoff/ticket-123.md",
    title: "Ticket 123",
    updated_unix: 1_787_764_927,
    comments_open: 2,
    comments_total: 3,
    read_state: "unread",
    last_opened_unix: 1_787_764_000,
    changed_since_read: true,
    ...overrides,
  };
}

function payload(overrides: Partial<DeskResponse> = {}): DeskResponse {
  return { items: [item()], attention: 1, ...overrides };
}

function renderPill() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <DeskPill />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("DeskPill", () => {
  it("renders nothing while loading (never a flashing 0)", () => {
    vi.mocked(fetchDesk).mockReturnValue(new Promise(() => {}));
    const { container } = renderPill();
    expect(container.firstChild).toBeNull();
    expect(screen.queryByTestId("header-desk")).toBeNull();
  });

  it("renders nothing when attention is 0", async () => {
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload({ items: [], attention: 0 }));
    const { container } = renderPill();
    await waitFor(() => expect(fetchDesk).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
    expect(screen.queryByTestId("header-desk")).toBeNull();
  });

  it("renders nothing on fetch error", async () => {
    vi.mocked(fetchDesk).mockRejectedValueOnce(new Error("down"));
    const { container } = renderPill();
    await waitFor(() => expect(fetchDesk).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it("shows the attention count and an aria-label that includes it", async () => {
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload({ attention: 3 }));
    renderPill();
    await waitFor(() => expect(screen.getByTestId("header-desk")).toBeTruthy());
    const chip = screen.getByTestId("header-desk");
    expect(chip.textContent).toContain("3");
    expect(chip.getAttribute("aria-label")).toContain("3");
  });

  it("opens a popover of desk rows whose hrefs go through artifactHref", async () => {
    const row = item();
    vi.mocked(fetchDesk).mockResolvedValueOnce(
      payload({ items: [row], attention: 1 }),
    );
    renderPill();
    await waitFor(() => expect(screen.getByTestId("header-desk")).toBeTruthy());
    fireEvent.click(screen.getByTestId("header-desk"));
    const link = screen.getByTestId(`desk-row-${row.id}`);
    expect(link.getAttribute("href")).toBe(
      artifactHref(row.kb, row.source_relative),
    );
    expect(link.getAttribute("href")).toBe("/a/docs/handoff/ticket-123.md");
    expect(link.textContent).toContain("Ticket 123");
    expect(link.textContent).toContain("docs");
    expect(link.textContent).toContain("unread");
    expect(link.textContent).toContain("changed");
  });

  it("dismisses the popover on Escape", async () => {
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload());
    renderPill();
    await waitFor(() => expect(screen.getByTestId("header-desk")).toBeTruthy());
    fireEvent.click(screen.getByTestId("header-desk"));
    expect(screen.getByRole("dialog", { name: "desk" })).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "desk" })).toBeNull();
  });

  it("dismisses the popover on outside click", async () => {
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload());
    renderPill();
    await waitFor(() => expect(screen.getByTestId("header-desk")).toBeTruthy());
    fireEvent.click(screen.getByTestId("header-desk"));
    expect(screen.getByRole("dialog", { name: "desk" })).toBeTruthy();
    fireEvent.mouseDown(document.body);
    expect(screen.queryByRole("dialog", { name: "desk" })).toBeNull();
  });

});
