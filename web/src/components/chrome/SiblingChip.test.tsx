// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router-dom";
import SiblingChip from "./SiblingChip";
import { useSiblingChip } from "../../hooks/useSiblingChip";
import { useDocLens, useDocLensScorecard } from "../../hooks/useDocLens";
import { artifactOrigin } from "../../lib/artifactHost";

const SUFFIX = ".artifacts.localhost";
const CODE = "https://code.example.test";
const DOC_ID = "aaaaaaaaaaaa";

vi.mock("../../hooks/useArtifactHost", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../hooks/useArtifactHost")>();
  return { ...actual, useArtifactHostSuffix: () => ".artifacts.localhost" };
});

const client = vi.hoisted(() => ({
  fetchDocByPath: vi.fn(),
  fetchCodeRefs: vi.fn(),
  fetchKbs: vi.fn(),
}));
vi.mock("../../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/client")>();
  return { ...actual, ...client };
});

const doclens = vi.hoisted(() => ({
  fetchDocLens: vi.fn(),
  fetchDocLensScorecard: vi.fn(),
}));
vi.mock("../../api/doclens", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/doclens")>();
  return { ...actual, ...doclens };
});

function codeRefs(refs: object[], groups: object[] = []) {
  return {
    doc_id: DOC_ID,
    never_scanned: false,
    extracted_at: 1_800_000_000,
    code_rev: null,
    ref_count: refs.length,
    ungrouped_count: 0,
    truncated: false,
    groups,
    refs,
  };
}
const pathRef = (ordinal: number, group: string | null, path: string, line: number) => ({
  ordinal,
  group,
  kind: "path_line",
  raw: `${path}:${line}`,
  path_hint: path,
  line_start: line,
  line_end: null,
  line_spans: null,
  symbol_container: null,
  symbol_member: null,
  context: "",
  context_tokens: [],
  declared: false,
});
const REFS = codeRefs(
  [
    pathRef(0, "intro", "src/a.rs", 10),
    pathRef(1, "design", "crates/kb-core/src/coderefs.rs", 120),
  ],
  [
    { ordinal: 0, key: "intro", label: "Intro", anchor: "intro" },
    { ordinal: 1, key: "design", label: "Design", anchor: "design" },
  ],
);
const SCORECARD = {
  doc_id: DOC_ID,
  never_scanned: false,
  pinned_repo: null,
  repos: [],
};
function lensOut() {
  return {
    counts: { drifted: 0, total: 2 },
    resolved_unix: 1_800_000_000,
    refs: [
      {
        ordinal: 1,
        path_state: "present",
        reader: { repo: "app", path: "crates/kb-core/src/coderefs.rs", line: 130 },
        search: null,
      },
    ],
  };
}

function Harness() {
  const model = useSiblingChip();
  return model ? <SiblingChip model={model} /> : <span data-testid="no-chip" />;
}

function renderAt(url: string, extra?: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[url]}>
        <Harness />
        {extra}
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  // Rail collapsed: the chip is the only home for the door in these tests.
  localStorage.setItem("kb:inspector-collapsed.detail", "true");
  client.fetchDocByPath.mockResolvedValue({ id: DOC_ID });
  client.fetchCodeRefs.mockResolvedValue(REFS);
  client.fetchKbs.mockResolvedValue([{ name: "k", code_url: CODE }]);
  doclens.fetchDocLensScorecard.mockResolvedValue(SCORECARD);
  doclens.fetchDocLens.mockResolvedValue(lensOut());
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Header Code chip", () => {
  it("paints hints first, then upgrades to verified from the lens", async () => {
    let release: (v: unknown) => void = () => {};
    doclens.fetchDocLens.mockReturnValue(new Promise((r) => (release = r)));
    renderAt("/a/k/notes/doc.html?repo=app");
    const chip = await screen.findByTestId("sibling-code-chip");
    expect(chip.parentElement?.getAttribute("data-kb-sibling-kind")).toBe("hints");
    await act(async () => release(lensOut()));
    await waitFor(() =>
      expect(screen.getByTestId("sibling-code-chip").parentElement?.getAttribute("data-kb-sibling-kind")).toBe(
        "verified",
      ),
    );
    const a = screen.getByTestId("sibling-code-chip");
    expect(a.tagName).toBe("A");
    expect(a.getAttribute("href")).toBe(
      `${CODE}/r/app/crates/kb-core/src/coderefs.rs?line=130`,
    );
    expect(a.textContent).toContain("coderefs.rs:130");
  });

  it("a CORS-shaped failure stays hint-worded and never reads as verified", async () => {
    doclens.fetchDocLensScorecard.mockRejectedValue(new TypeError("Failed to fetch"));
    doclens.fetchDocLens.mockRejectedValue(new TypeError("Failed to fetch"));
    renderAt("/a/k/notes/doc.html?repo=app");
    await waitFor(() =>
      expect(screen.getByTestId("sibling-code-chip").parentElement?.getAttribute("data-kb-sibling-kind")).toBe(
        "degraded",
      ),
    );
    expect(screen.getByTestId("sibling-code-chip").getAttribute("title")).toContain(
      "cannot check them against the code",
    );
  });

  it("shares the Links tab's query keys: one request each, not two", async () => {
    function LinksTabStandIn() {
      useDocLensScorecard(CODE, "k", DOC_ID, true);
      useDocLens(CODE, "k", DOC_ID, "app");
      return null;
    }
    renderAt("/a/k/notes/doc.html?repo=app", <LinksTabStandIn />);
    await screen.findByTestId("sibling-code-chip");
    await waitFor(() => expect(doclens.fetchDocLens).toHaveBeenCalled());
    expect(doclens.fetchDocLensScorecard).toHaveBeenCalledTimes(1);
    expect(doclens.fetchDocLens).toHaveBeenCalledTimes(1);
  });

  it("the live kb:section re-targets the chip to the nearest ref", async () => {
    doclens.fetchDocLens.mockReturnValue(new Promise(() => {}));
    renderAt("/a/k/notes/doc.html?repo=app");
    const before = await screen.findByTestId("sibling-code-chip");
    expect(before.getAttribute("href")).toBe(`${CODE}/r/app/src/a.rs?line=10`);
    await act(async () => {
      window.dispatchEvent(
        new MessageEvent("message", {
          origin: artifactOrigin(DOC_ID, "k", SUFFIX),
          data: { kind: "kb:section", id: "design" },
        }),
      );
    });
    await waitFor(() =>
      expect(screen.getByTestId("sibling-code-chip").getAttribute("href")).toBe(
        `${CODE}/r/app/crates/kb-core/src/coderefs.rs?line=120`,
      ),
    );
  });

  it("ignores a kb:section from another artifact's origin", async () => {
    doclens.fetchDocLens.mockReturnValue(new Promise(() => {}));
    renderAt("/a/k/notes/doc.html?repo=app");
    await screen.findByTestId("sibling-code-chip");
    await act(async () => {
      window.dispatchEvent(
        new MessageEvent("message", {
          origin: artifactOrigin("bbbbbbbbbbbb", "k", SUFFIX),
          data: { kind: "kb:section", id: "design" },
        }),
      );
    });
    expect(screen.getByTestId("sibling-code-chip").getAttribute("href")).toBe(
      `${CODE}/r/app/src/a.rs?line=10`,
    );
  });

  it("is absent on the gallery and on /search", async () => {
    for (const url of ["/", "/search?q=x"]) {
      const { unmount } = renderAt(url);
      expect(screen.getByTestId("no-chip")).toBeTruthy();
      unmount();
    }
    expect(client.fetchCodeRefs).not.toHaveBeenCalled();
  });

  it("renders no chip while the rail shows the Code section", async () => {
    localStorage.setItem("kb:inspector-collapsed.detail", "false");
    renderAt("/a/k/notes/doc.html?repo=app");
    await waitFor(() => expect(client.fetchCodeRefs).toHaveBeenCalled());
    expect(screen.getByTestId("no-chip")).toBeTruthy();
  });

  it("renders no chip at or below 860px (mobile is a reader)", async () => {
    const real = window.matchMedia;
    window.matchMedia = ((q: string) => ({
      matches: q === "(max-width: 860px)",
      media: q,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      onchange: null,
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
    try {
      renderAt("/a/k/notes/doc.html?repo=app");
      await waitFor(() => expect(client.fetchCodeRefs).toHaveBeenCalled());
      expect(screen.getByTestId("no-chip")).toBeTruthy();
    } finally {
      window.matchMedia = real;
    }
  });

  it("renders no chip on the Links tab with the rail open, and appears once the rail collapses", async () => {
    localStorage.setItem("kb:inspector-collapsed.detail", "false");
    localStorage.setItem("kb:inspector-tab.detail", "links");
    renderAt("/a/k/notes/doc.html?repo=app");
    await waitFor(() => expect(client.fetchCodeRefs).toHaveBeenCalled());
    expect(screen.getByTestId("no-chip")).toBeTruthy();
    await act(async () => {
      localStorage.setItem("kb:inspector-collapsed.detail", "true");
      window.dispatchEvent(new CustomEvent("kb:inspector-collapsed.detail"));
    });
    expect(await screen.findByTestId("sibling-code-chip")).toBeTruthy();
  });

  it("a pinned repo with no ?repo= still verifies (the lens runs on the pin)", async () => {
    doclens.fetchDocLensScorecard.mockResolvedValue({ ...SCORECARD, pinned_repo: "app" });
    renderAt("/a/k/notes/doc.html");
    await waitFor(() =>
      expect(screen.getByTestId("sibling-code-chip").parentElement?.getAttribute("data-kb-sibling-kind")).toBe(
        "verified",
      ),
    );
    expect(doclens.fetchDocLens).toHaveBeenCalledTimes(1);
    expect(doclens.fetchDocLens.mock.calls[0]).toContain("app");
  });

  it("a doc with no usable path is a button that opens the Links tab", async () => {
    client.fetchCodeRefs.mockResolvedValue(
      codeRefs([{ ...pathRef(0, null, "o/r", 4), kind: "issue" }]),
    );
    renderAt("/a/k/notes/doc.html");
    const chip = await screen.findByTestId("sibling-code-chip");
    expect(chip.tagName).toBe("BUTTON");
    fireEvent.click(chip);
    expect(localStorage.getItem("kb:inspector-tab.detail")).toBe("links");
    expect(localStorage.getItem("kb:inspector-collapsed.detail")).toBe("false");
  });

  it("is absent when the kb has no code_url", async () => {
    client.fetchKbs.mockResolvedValue([{ name: "k", code_url: null }]);
    renderAt("/a/k/notes/doc.html");
    await waitFor(() => expect(client.fetchCodeRefs).toHaveBeenCalled());
    expect(screen.getByTestId("no-chip")).toBeTruthy();
    expect(doclens.fetchDocLensScorecard).not.toHaveBeenCalled();
  });
});
