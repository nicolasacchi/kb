// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("../api/versions", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/versions")>();
  return { ...actual, fetchSinceVisit: vi.fn() };
});

import { fetchSinceVisit } from "../api/versions";
import { sinceVisitKey, useSinceVisit } from "./useSinceVisit";

function Probe({ kb, id }: { kb: string; id: string }) {
  const d = useSinceVisit(kb, id, true);
  return <output data-testid="s">{d?.state ?? "-"}</output>;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("useSinceVisit", () => {
  it("fetches once per mount and a remount refetches (on-open answer)", async () => {
    vi.mocked(fetchSinceVisit).mockResolvedValue({ state: "no_visit", mode: "index" });
    const qc = new QueryClient();
    const tree = (
      <QueryClientProvider client={qc}>
        <Probe kb="k" id="aaaaaaaaaaaa" />
      </QueryClientProvider>
    );
    const first = render(tree);
    await waitFor(() => expect(first.getByTestId("s").textContent).toBe("no_visit"));
    expect(fetchSinceVisit).toHaveBeenCalledTimes(1);
    first.unmount();
    render(tree);
    await waitFor(() => expect(fetchSinceVisit).toHaveBeenCalledTimes(2));
  });

  it("is not bridged: artifact.indexed does not refetch it", async () => {
    // The key is outside every bridge prefix: invalidating docs/doc/versions
    // churn prefixes never matches ["sinceVisit", kb, id].
    const key = sinceVisitKey("k", "aaaaaaaaaaaa");
    expect(key[0]).toBe("sinceVisit");
    const qc = new QueryClient();
    vi.mocked(fetchSinceVisit).mockResolvedValue({ state: "no_visit", mode: "index" });
    const r = render(
      <QueryClientProvider client={qc}>
        <Probe kb="k" id="aaaaaaaaaaaa" />
      </QueryClientProvider>,
    );
    await waitFor(() => expect(r.getByTestId("s").textContent).toBe("no_visit"));
    for (const prefix of [["docs", "k"], ["doc", "k"], ["versions", "k"], ["search"], ["memories"]]) {
      await qc.invalidateQueries({ queryKey: prefix });
    }
    expect(fetchSinceVisit).toHaveBeenCalledTimes(1);
  });
});
