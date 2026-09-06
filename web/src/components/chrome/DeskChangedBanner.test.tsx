// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import DeskChangedBanner, {
  deskBannerDismissKey,
  deskBannerVisible,
  findDeskItem,
  isHandoffPath,
} from "./DeskChangedBanner";
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

function payload(items: DeskItem[]): DeskResponse {
  return { items, attention: items.filter((i) => i.changed_since_read).length };
}

function renderBanner(kb = "docs", id = "abc123def456") {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <DeskChangedBanner kb={kb} id={id} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  sessionStorage.clear();
});

describe("desk banner gating (pure)", () => {
  it("isHandoffPath is the handoff/ prefix", () => {
    expect(isHandoffPath("handoff/ticket-123.md")).toBe(true);
    expect(isHandoffPath("notes/ticket-123.md")).toBe(false);
    expect(isHandoffPath("handoff")).toBe(false);
  });

  it("deskBannerDismissKey is per kb/id/updated_unix", () => {
    expect(deskBannerDismissKey("docs", "abc", 10)).toBe(
      "kb:desk-banner-dismissed:docs:abc:10",
    );
    expect(deskBannerDismissKey("docs", "abc", 11)).not.toBe(
      deskBannerDismissKey("docs", "abc", 10),
    );
  });

  it("findDeskItem matches kb + id", () => {
    const a = item({ id: "aaa", kb: "docs" });
    const b = item({ id: "aaa", kb: "other" });
    expect(findDeskItem([a, b], "other", "aaa")).toBe(b);
    expect(findDeskItem([a], "docs", "missing")).toBeUndefined();
  });

  it("visible only for a handoff with changed_since_read and not dismissed", () => {
    expect(deskBannerVisible(undefined, false)).toBe(false);
    expect(deskBannerVisible(item({ source_relative: "notes/x.md" }), false)).toBe(
      false,
    );
    expect(deskBannerVisible(item({ changed_since_read: false }), false)).toBe(
      false,
    );
    expect(deskBannerVisible(item(), true)).toBe(false);
    expect(deskBannerVisible(item(), false)).toBe(true);
  });
});

describe("DeskChangedBanner", () => {
  it("renders nothing when the open artifact is not on the desk", async () => {
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload([]));
    const { container } = renderBanner();
    await waitFor(() => expect(fetchDesk).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when changed_since_read is false", async () => {
    vi.mocked(fetchDesk).mockResolvedValueOnce(
      payload([item({ changed_since_read: false })]),
    );
    const { container } = renderBanner();
    await waitFor(() => expect(fetchDesk).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when source_relative is not a handoff path", async () => {
    vi.mocked(fetchDesk).mockResolvedValueOnce(
      payload([item({ source_relative: "notes/ticket-123.md" })]),
    );
    const { container } = renderBanner();
    await waitFor(() => expect(fetchDesk).toHaveBeenCalled());
    expect(container.firstChild).toBeNull();
  });

  it("renders the banner with a ?at= previous-version link", async () => {
    const row = item();
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload([row]));
    renderBanner(row.kb, row.id);
    await waitFor(() =>
      expect(screen.getByTestId("desk-changed-banner")).toBeTruthy(),
    );
    expect(screen.getByText("Changed since you last read")).toBeTruthy();
    const prev = screen.getByTestId("desk-banner-prev");
    expect(prev.getAttribute("href")).toBe(
      artifactHref(row.kb, row.source_relative, { at: row.last_opened_unix }),
    );
    expect(prev.getAttribute("href")).toBe(
      "/a/docs/handoff/ticket-123.md?at=1787764000",
    );
  });

  it("dismiss writes the sessionStorage key and hides; a newer updated_unix re-shows", async () => {
    const row = item({ updated_unix: 100 });
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload([row]));
    renderBanner(row.kb, row.id);
    await waitFor(() =>
      expect(screen.getByTestId("desk-changed-banner")).toBeTruthy(),
    );
    fireEvent.click(screen.getByRole("button", { name: "dismiss" }));
    expect(screen.queryByTestId("desk-changed-banner")).toBeNull();
    expect(
      sessionStorage.getItem(deskBannerDismissKey(row.kb, row.id, 100)),
    ).toBe("1");

    // Same generation stays hidden even if the component remounts.
    cleanup();
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload([row]));
    renderBanner(row.kb, row.id);
    await waitFor(() => expect(fetchDesk).toHaveBeenCalled());
    expect(screen.queryByTestId("desk-changed-banner")).toBeNull();

    // Newer updated_unix is a different key → banner returns.
    cleanup();
    const newer = item({ updated_unix: 200 });
    vi.mocked(fetchDesk).mockResolvedValueOnce(payload([newer]));

    renderBanner(newer.kb, newer.id);
    await waitFor(() =>
      expect(screen.getByTestId("desk-changed-banner")).toBeTruthy(),
    );
  });
});
