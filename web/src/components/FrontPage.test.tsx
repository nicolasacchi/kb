// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import FrontPage from "./FrontPage";
import {
  fetchEchoes,
  fetchResurface,
  type DocSummary,
  type KbSummary,
} from "../api/client";
import { fetchInbox } from "../api/inbox";
import { fetchDesk } from "../api/desk";
import { fetchSlates } from "../api/slates";
import { censusRead, censusReset } from "../lib/census";

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return { ...actual, fetchEchoes: vi.fn(), fetchResurface: vi.fn(), fetchDocsPage: vi.fn() };
});
vi.mock("../api/inbox", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/inbox")>();
  return { ...actual, fetchInbox: vi.fn() };
});
vi.mock("../api/desk", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/desk")>();
  return { ...actual, fetchDesk: vi.fn() };
});
vi.mock("../api/slates", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/slates")>();
  return { ...actual, fetchSlates: vi.fn(), fetchSlateBoard: vi.fn() };
});

const NOW_S = Math.floor(Date.now() / 1000);

function doc(id: string, extra: Partial<DocSummary> = {}): DocSummary {
  return {
    id,
    title: `Doc ${id}`,
    path: `${id}.html`,
    folder: "",
    source_relative: `${id}.html`,
    word_count: 500,
    mtime_unix: NOW_S - 3600,
    ...extra,
  };
}

const KBS = [{ name: "docs", doc_count: 2, memory_scope: null }] as unknown as KbSummary[];

function mount(rows: DocSummary[] = [doc("a"), doc("b", { summary: "dek of b" })]) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <FrontPage kb="docs" kbs={KBS} rows={rows} total={2} hasMore={false} loading={false} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
  censusReset();
  vi.mocked(fetchEchoes).mockResolvedValue({ items: [], today: "", beliefs: [] });
  vi.mocked(fetchResurface).mockResolvedValue({
    items: [
      {
        id: "b",
        title: "Doc b",
        source_relative: "b.html",
        score: 2,
        comment_term: 1,
        read_term: 1,
        open_comments: 2,
        reasons: ["2 open comments · oldest 4d"],
      },
    ],
    now_unix: NOW_S,
  } as never);
  vi.mocked(fetchInbox).mockResolvedValue({ items: [], total_open: 0 });
  vi.mocked(fetchDesk).mockResolvedValue({ items: [], attention: 0 });
  vi.mocked(fetchSlates).mockResolvedValue([]);
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("FrontPage", () => {
  it("renders the masthead, the date, and an honest edition line", async () => {
    mount();
    expect(screen.getByRole("heading", { name: "docs" })).toBeTruthy();
    const edition = screen.getByTestId("frontpage-edition").textContent ?? "";
    expect(edition).toContain("2 artifacts");
    expect(edition).toContain("1,000 words");
    // First visit in this browser: it says so instead of inventing a count.
    expect(edition).toContain("no earlier visit on record here");
    expect(document.querySelector("time")?.textContent).toMatch(/\d{4}$/);
  });

  it("leads with the resurface item and its reason as the dateline", async () => {
    mount();
    const lead = await screen.findByTestId("frontpage-lead");
    await waitFor(() => expect(lead.textContent).toContain("2 open comments · oldest 4d"));
    expect(lead.textContent).toContain("Doc b");
    expect(lead.textContent).toContain("dek of b");
    fireEvent.click(lead.querySelector("a") as HTMLElement);
    expect(censusRead()["frontpage.lead.open"]).toBe(1);
    expect(censusRead()["frontpage.view"]).toBeGreaterThanOrEqual(1);
  });

  it("collapses to the masthead line, remembers it per kb, and counts it", async () => {
    const first = mount();
    await screen.findByTestId("frontpage-lead");
    fireEvent.click(screen.getByTestId("frontpage-collapse"));
    expect(screen.queryByTestId("frontpage-lead")).toBeNull();
    expect(screen.getByRole("heading", { name: "docs" })).toBeTruthy();
    expect(localStorage.getItem("kb:frontpage:collapsed:docs")).toBe("1");
    expect(censusRead()["frontpage.collapse"]).toBe(1);
    first.unmount();

    mount();
    expect(screen.queryByTestId("frontpage-lead")).toBeNull();
    fireEvent.click(screen.getByTestId("frontpage-collapse"));
    expect(await screen.findByTestId("frontpage-lead")).toBeTruthy();
    expect(localStorage.getItem("kb:frontpage:collapsed:docs")).toBe("0");
  });

  it("shows open comments in the coverage rail with a link", async () => {
    vi.mocked(fetchInbox).mockResolvedValue({
      items: [
        {
          kb: "docs",
          artifact_id: "a",
          source_relative: "a.html",
          title: "Doc a",
          comment_id: "c1",
          excerpt: "please check this",
          author: "you",
          reply_count: 0,
          anchor: "file",
          stale: false,
          created_at: 1,
          updated_at: 2,
        },
      ],
      total_open: 1,
    });
    mount();
    const rail = await screen.findByLabelText("continuing coverage");
    await waitFor(() => expect(rail.textContent).toContain("1 open comment"));
    expect(rail.querySelector("a")?.getAttribute("href")).toContain("/a/docs/a.html");
  });
});
