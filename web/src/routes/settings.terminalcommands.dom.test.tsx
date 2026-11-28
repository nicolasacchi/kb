// @vitest-environment jsdom
//
// CMDSETTINGS - the "Terminal commands" Settings tab exists only for an
// admitted caller, and `/settings#terminal-commands` resolves to it even when
// the identity arrives after the first paint.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import Settings from "./settings";
import ConfirmProvider from "../components/ConfirmProvider";

let admitted: boolean | null;
vi.mock("../hooks/useArtifactHost", async (orig) => {
  const real = await orig<typeof import("../hooks/useArtifactHost")>();
  return {
    ...real,
    useIdentity: () => (admitted === null ? null : ({ terminal_admitted: admitted } as never)),
  };
});

beforeEach(() => {
  localStorage.clear();
  window.location.hash = "";
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => {
      throw new Error("no daemon");
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function ui() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return (
    <QueryClientProvider client={qc}>
      <ConfirmProvider>
        <MemoryRouter>
          <Settings />
        </MemoryRouter>
      </ConfirmProvider>
    </QueryClientProvider>
  );
}

describe("Settings > Terminal commands tab", () => {
  it("is absent when the caller is not admitted, or the identity is unknown", () => {
    admitted = false;
    render(ui());
    expect(screen.queryByRole("tab", { name: "Terminal commands" })).toBeNull();
    cleanup();
    admitted = null;
    render(ui());
    expect(screen.queryByRole("tab", { name: "Terminal commands" })).toBeNull();
  });

  it("is present after Census for an admitted caller", () => {
    admitted = true;
    render(ui());
    const names = screen.getAllByRole("tab").map((t) => t.textContent);
    expect(names.indexOf("Terminal commands")).toBe(names.indexOf("Census") + 1);
  });

  it("the hash selects it, including when the identity arrives later", () => {
    window.location.hash = "#terminal-commands";
    admitted = null;
    const view = render(ui());
    expect(screen.getByRole("tab", { name: "Overview" }).getAttribute("aria-selected")).toBe("true");
    admitted = true;
    view.rerender(ui());
    expect(screen.getByRole("tab", { name: "Terminal commands" }).getAttribute("aria-selected")).toBe("true");
  });
});
