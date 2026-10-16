// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { useDocs } from "./useDocs";
import { fetchDocsPage, type DocSummary } from "../api/client";

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return { ...actual, fetchDocsPage: vi.fn() };
});

function doc(id: string): DocSummary {
  return {
    id,
    title: `Doc ${id}`,
    path: `${id}.html`,
    folder: "",
    source_relative: `${id}.html`,
    word_count: 1,
    mtime_unix: 1,
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("useDocs", () => {
  it("never returns one artifact twice when a duplicate spans a page boundary", async () => {
    // Page 1 ends with "b"; page 2 starts with the same artifact again (two
    // physical rows) — the merged list must carry it once, order preserved.
    vi.mocked(fetchDocsPage)
      .mockResolvedValueOnce({
        docs: [doc("a"), doc("b")],
        offset: 0,
        total: 4,
        has_more: true,
      } as never)
      .mockResolvedValueOnce({
        docs: [doc("b"), doc("c")],
        offset: 2,
        total: 4,
        has_more: false,
      } as never);
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={qc}>{children}</QueryClientProvider>
    );
    const { result } = renderHook(() => useDocs("canon", {}, 2), { wrapper });
    await waitFor(() => expect(result.current.rows.length).toBe(2));
    result.current.loadMore();
    await waitFor(() => expect(result.current.rows.length).toBe(3));
    expect(result.current.rows.map((r) => r.id)).toEqual(["a", "b", "c"]);
    // The server total counted 4 physical rows; the badge matches the cards.
    expect(result.current.total).toBe(3);
  });
});
