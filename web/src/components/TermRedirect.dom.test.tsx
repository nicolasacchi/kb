// @vitest-environment jsdom
//
// HUBDOOR — /term/:id redirects to /terminals/:id, replacing the entry and
// keeping the query, hash and navigation state; a malformed id lands on the
// terminal page's own not-found card (through the real route table).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation, useNavigationType } from "react-router-dom";

vi.mock("../hooks/useArtifactHost", async (orig) => ({
  ...(await orig<typeof import("../hooks/useArtifactHost")>()),
  useIdentity: () => ({ terminal_admitted: true }) as never,
}));
vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: () => <div data-testid="mock-panel" />,
}));

import TermRedirect from "./TermRedirect";
import TermPageRoute from "../routes/termPage";

function Where() {
  const l = useLocation();
  const t = useNavigationType();
  return (
    <div>
      <span data-testid="at">{l.pathname + l.search + l.hash}</span>
      <span data-testid="state">{JSON.stringify(l.state)}</span>
      <span data-testid="nav">{t}</span>
    </div>
  );
}

function mount(entry: Parameters<typeof MemoryRouter>[0]["initialEntries"]) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={entry}>
        <Routes>
          <Route path="/term/:id" element={<TermRedirect />} />
          <Route path="/terminals/:id" element={<><Where /><TermPageRoute /></>} />
          <Route path="*" element={<Where />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal("matchMedia", (q: string) =>
    ({ matches: false, media: q, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }) as unknown as MediaQueryList,
  );
  vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ terms: [] }), { status: 200 })));
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("TermRedirect", () => {
  it("/term/t1?x=1#h -> /terminals/t1?x=1#h, replace, state preserved", () => {
    mount([{ pathname: "/term/t1", search: "?x=1", hash: "#h", state: { launched: { id: "t1", harness: "omp" } } }]);
    expect(screen.getByTestId("at").textContent).toBe("/terminals/t1?x=1#h");
    expect(screen.getByTestId("state").textContent).toBe('{"launched":{"id":"t1","harness":"omp"}}');
    expect(screen.getByTestId("nav").textContent).toBe("REPLACE");
  });

  it("a malformed id still lands on the terminal page's not-found card", async () => {
    mount(["/term/BAD.."]);
    expect(screen.getByTestId("at").textContent).toBe("/terminals/BAD..");
    expect(await screen.findByTestId("term-not-found")).toBeTruthy();
  });
});
