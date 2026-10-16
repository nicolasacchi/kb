// @vitest-environment jsdom
//
// OL P4 — Lists, Notes and Settings share ONE page frame (`.kb-page`: same
// max-width, centring and top padding), so the Lists/Notes tab bar does not
// shift between the two. Renders the REAL routes and checks the root element.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import ListsRoute from "./lists";
import NotesRoute from "./notes";
import Settings from "./settings";
import ConfirmProvider from "../components/ConfirmProvider";

beforeEach(() => {
  // No daemon in jsdom: every fetch fails fast; only the first paint matters.
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

function mount(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <ConfirmProvider>
        <MemoryRouter>{ui}</MemoryRouter>
      </ConfirmProvider>
    </QueryClientProvider>,
  );
}

describe("page frame", () => {
  it("Lists root carries kb-page", () => {
    const { getByTestId } = mount(<ListsRoute />);
    expect(getByTestId("lists-view").classList.contains("kb-page")).toBe(true);
  });
  it("Notes root carries kb-page", () => {
    const { getByTestId } = mount(<NotesRoute />);
    expect(getByTestId("notes-view").classList.contains("kb-page")).toBe(true);
  });
  it("Settings root carries kb-page", () => {
    const { container } = mount(<Settings />);
    expect(container.querySelector(".settings")?.classList.contains("kb-page")).toBe(true);
  });
});
