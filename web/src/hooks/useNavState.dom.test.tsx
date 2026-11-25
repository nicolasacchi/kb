// @vitest-environment jsdom
//
// HUBDOOR — useNavState: the blocked-terminal count behind the Terminals door.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { TermRow } from "../api/terms";

let admitted = true;
vi.mock("./useArtifactHost", () => ({
  useIdentity: () => ({ terminal_admitted: admitted }) as never,
}));

import { blockedTermCount, useNavItems, useNavState } from "./useNavState";

const row = (id: string, o: Partial<TermRow> = {}): TermRow => ({
  id,
  harness: "claude",
  cwd: "/w/a",
  started_unix: 1,
  resume_sid: null,
  pane_pid: 1,
  cols: 80,
  rows: 24,
  bracket_paste_flag: false,
  pane_dead: false,
  exit_code: null,
  grid_owner: null,
  conf_version: 1,
  anchor_pid: null,
  stale: false,
  ...o,
});

function Probe() {
  const s = useNavState();
  const items = useNavItems();
  return (
    <div>
      <span data-testid="n">{s.blockedTerms}</span>
      <span data-testid="adm">{String(s.terminalAdmitted)}</span>
      <span data-testid="ids">{items.map((i) => i.id).join(",")}</span>
    </div>
  );
}

const fetchSpy = vi.fn();
beforeEach(() => {
  admitted = true;
  fetchSpy.mockReset();
  fetchSpy.mockImplementation(async () => new Response(JSON.stringify({ terms: [] }), { status: 200 }));
  vi.stubGlobal("fetch", fetchSpy);
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("blockedTermCount", () => {
  it("counts blocked rows that are still alive", () => {
    expect(blockedTermCount(undefined)).toBe(0);
    expect(blockedTermCount([])).toBe(0);
    expect(
      blockedTermCount([
        row("a", { blocked: true }),
        row("b", { blocked: true }),
        row("c", { blocked: true, pane_dead: true }),
        row("d", { blocked: false }),
        row("e"),
      ]),
    ).toBe(2);
  });
});

describe("useNavState", () => {
  it("renders with NO QueryClientProvider: count 0, no throw, no fetch", async () => {
    render(<Probe />);
    expect(screen.getByTestId("n").textContent).toBe("0");
    expect(screen.getByTestId("adm").textContent).toBe("true");
    expect(screen.getByTestId("ids").textContent).toContain("terminals");
    await new Promise((r) => setTimeout(r, 20));
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("with a provider, counts blocked live rows of the shared [\"terms\"] query", () => {
    const qc = new QueryClient();
    qc.setQueryData(["terms"], [
      row("a", { blocked: true }),
      row("b", { blocked: true }),
      row("c", { blocked: true, pane_dead: true }),
      row("d"),
    ]);
    render(
      <QueryClientProvider client={qc}>
        <Probe />
      </QueryClientProvider>,
    );
    expect(screen.getByTestId("n").textContent).toBe("2");
  });

  it("follows the cache when the terms query changes", async () => {
    const qc = new QueryClient();
    qc.setQueryData(["terms"], [row("a", { blocked: true })]);
    render(
      <QueryClientProvider client={qc}>
        <Probe />
      </QueryClientProvider>,
    );
    expect(screen.getByTestId("n").textContent).toBe("1");
    qc.setQueryData(["terms"], [row("a")]);
    await waitFor(() => expect(screen.getByTestId("n").textContent).toBe("0"));
  });

  it("not admitted: count 0 even with blocked rows cached, the query never fetches, no Terminals door", async () => {
    admitted = false;
    const qc = new QueryClient();
    qc.setQueryData(["terms"], [row("a", { blocked: true })]);
    render(
      <QueryClientProvider client={qc}>
        <Probe />
      </QueryClientProvider>,
    );
    expect(screen.getByTestId("n").textContent).toBe("0");
    expect(screen.getByTestId("ids").textContent).not.toContain("terminals");
    await new Promise((r) => setTimeout(r, 20));
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("admitted with a provider and an empty cache: reads /api/terms once", async () => {
    const qc = new QueryClient();
    render(
      <QueryClientProvider client={qc}>
        <Probe />
      </QueryClientProvider>,
    );
    await waitFor(() => expect(fetchSpy).toHaveBeenCalledTimes(1));
    expect(String(fetchSpy.mock.calls[0][0])).toContain("/api/terms");
  });
});
