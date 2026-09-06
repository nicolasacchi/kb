// @vitest-environment jsdom
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import LineageViewer from "./LineageViewer";
import { fetchMemoryLineage, type MemoryLineageResponse } from "../api/client";

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return {
    ...actual,
    fetchMemoryLineage: vi.fn(),
    fetchMemoryPolicy: vi.fn(async () => ({ policy: "balanced" as const, drop_threshold: 0.15 })),
  };
});

// jsdom's <dialog> support doesn't implement showModal() in every version
// this repo might run under — stub it so mounting never throws, matching
// every other native-<dialog> component in this codebase (none of which
// carry their own test file to have hit this yet).
beforeAll(() => {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function () {
      this.open = true;
    };
  } else {
    vi.spyOn(HTMLDialogElement.prototype, "showModal").mockImplementation(function (
      this: HTMLDialogElement,
    ) {
      this.open = true;
    });
  }
});

function renderViewer(kb = "globalmem", id = "aaaaaaaaaaaa") {
  // retry:false — the failure-path test below needs an immediate `isError`
  // transition, not TanStack's default 3-retry exponential backoff.
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <LineageViewer kb={kb} id={id} onClose={vi.fn()} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function node(overrides: Partial<MemoryLineageResponse["start"]> = {}) {
  return {
    id: "id0000000000",
    title: "A Fact",
    created_unix: 1_700_000_000,
    forgotten: false,
    salience: 0.5,
    decay_k: 0.01,
    age_days: 5,
    pinned: false,
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("LineageViewer", () => {
  it("shows a loading state, then the start node once fetched", async () => {
    vi.mocked(fetchMemoryLineage).mockResolvedValueOnce({
      id: "aaaaaaaaaaaa",
      start: node({ id: "aaaaaaaaaaaa", title: "Current Fact" }),
      supersedes_chain: [],
      superseded_by_chain: [],
    } as MemoryLineageResponse);
    renderViewer();
    await waitFor(() => {
      expect(screen.getByTestId("lineage-start").textContent).toContain("Current Fact");
    });
    expect(screen.getByText("(this memory supersedes nothing)")).toBeTruthy();
    expect(screen.getByText("(nothing supersedes this memory)")).toBeTruthy();
  });

  it("surfaces a fetch failure as an alert", async () => {
    vi.mocked(fetchMemoryLineage).mockRejectedValueOnce(new Error("404 not found"));
    renderViewer();
    await waitFor(() => {
      expect(screen.getByRole("alert").textContent).toContain("404 not found");
    });
  });

  it("renders both chains and their overlap badges", async () => {
    vi.mocked(fetchMemoryLineage).mockResolvedValueOnce({
      id: "mid00000000",
      start: node({ id: "mid00000000", title: "Middle Fact" }),
      supersedes_chain: [node({ id: "old000000000", title: "Old Fact", forgotten: true })],
      superseded_by_chain: [node({ id: "new000000000", title: "New Fact" })],
    } as MemoryLineageResponse);
    renderViewer();
    await waitFor(() => {
      expect(screen.getByText("Middle Fact")).toBeTruthy();
    });
    expect(screen.getByText("Old Fact")).toBeTruthy();
    expect(screen.getByText("New Fact")).toBeTruthy();
    expect(screen.getAllByTestId("lineage-overlap").length).toBeGreaterThan(0);
    // The forgotten hop renders distinctly.
    const nodes = screen.getAllByTestId("lineage-node");
    const oldNode = nodes.find((n) => n.textContent?.includes("Old Fact"));
    expect(oldNode?.className).toContain("kb-lineage__node--forgotten");
  });

  it("truncates a long chain with a count, never rendering a hairball", async () => {
    const longChain = Array.from({ length: 10 }, (_, i) =>
      node({ id: `older${i}0000`, title: `Older ${i}` }),
    );
    vi.mocked(fetchMemoryLineage).mockResolvedValueOnce({
      id: "start0000000",
      start: node({ id: "start0000000", title: "Start" }),
      supersedes_chain: longChain,
      superseded_by_chain: [],
    } as MemoryLineageResponse);
    renderViewer();
    await waitFor(() => {
      expect(screen.getByText("Start")).toBeTruthy();
    });
    expect(screen.getByTestId("lineage-truncated-older").textContent).toContain("+8 more");
    // At most DEFAULT_MAX_PER_SIDE (2) older nodes actually rendered.
    const nodes = screen.getAllByTestId("lineage-node");
    expect(nodes.length).toBe(3); // start + 2 shown older
  });

  // CT-B3 — "view family in gallery" pivots over the WHOLE chain (start +
  // both chains), not just the display-truncated slice `LineageChain`
  // actually renders.
  it("gallery pivot links over the whole chain, including truncated hops", async () => {
    const longChain = Array.from({ length: 10 }, (_, i) =>
      node({ id: `older${i}0000`, title: `Older ${i}` }),
    );
    vi.mocked(fetchMemoryLineage).mockResolvedValueOnce({
      id: "start0000000",
      start: node({ id: "start0000000", title: "Start" }),
      supersedes_chain: longChain,
      superseded_by_chain: [node({ id: "new000000000", title: "New Fact" })],
    } as MemoryLineageResponse);
    renderViewer("globalmem");
    await waitFor(() => {
      expect(screen.getByText("Start")).toBeTruthy();
    });
    const pivot = screen.getByTestId("lineage-gallery-pivot");
    expect(pivot.textContent).toContain("12"); // start + 10 older + 1 newer
    const expectedIds = ["start0000000", ...longChain.map((n) => n.id), "new000000000"];
    expect(pivot.getAttribute("href")).toBe(
      `/?kb=globalmem&ids=${expectedIds.map(encodeURIComponent).join("%2C")}`,
    );
  });

  // CT-B3 — invariant #35's degrade-LOUDLY rule: a chain over the gallery's
  // 500-id cap renders a disabled chip (no href), never a link the gallery
  // would 400 on.
  it("degrades to a disabled pivot chip when the chain exceeds the ids= cap", async () => {
    const hugeChain = Array.from({ length: 501 }, (_, i) => node({ id: `h${i}0000000` }));
    vi.mocked(fetchMemoryLineage).mockResolvedValueOnce({
      id: "start0000000",
      start: node({ id: "start0000000", title: "Start" }),
      supersedes_chain: hugeChain,
      superseded_by_chain: [],
    } as MemoryLineageResponse);
    renderViewer();
    await waitFor(() => {
      expect(screen.getByText("Start")).toBeTruthy();
    });
    const pivot = screen.getByTestId("lineage-gallery-pivot");
    expect(pivot.tagName).toBe("SPAN");
    expect(pivot.getAttribute("aria-disabled")).toBe("true");
    expect(pivot.getAttribute("href")).toBeNull();
  });
});
