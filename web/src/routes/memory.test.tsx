// @vitest-environment jsdom
//
// MI-W3.R — the SalienceEdit control (click-to-edit number input → PATCH
// …/memories/{id}/salience) had zero automated coverage: neither the
// click-to-edit UI nor the optimistic-overlay-with-rollback wiring around
// it (`updateSalience`, this file's `Memory` component) was exercised.
// Renders the REAL `Memory` route (not a re-implemented harness) with
// `useMemories` + the API client mocked, so the actual rollback code path
// is what's under test — the rollback is the risky half: a rejected PATCH
// must not leave the optimistic value stuck.
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import Memory from "./memory";
import { useMemories } from "../hooks/useMemories";
import {
  fetchMemoryLineage,
  patchMemorySalience,
  setMemoryPolicy,
  type MemoryLineageResponse,
  type RecallHit,
} from "../api/client";

// Partial mock: only `useMemories` is stubbed — CT-E4's widened provenance
// gate opens the REAL ProvenanceThread from this route, which reads the
// module's other hooks (useMemoryRecalledBy/useMemoryLineage); they run for
// real against the mocked api/client fetches below.
vi.mock("../hooks/useMemories", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../hooks/useMemories")>();
  return {
    ...actual,
    useMemories: vi.fn(),
  };
});

vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return {
    ...actual,
    patchMemorySalience: vi.fn(),
    // Mocked so `DecayRail`/`MemoryRow`'s shared `useMemoryPolicy` fetch
    // resolves quietly instead of hitting a real daemon — unrelated to the
    // control under test.
    fetchMemoryPolicy: vi.fn(async () => ({ policy: "balanced" as const, drop_threshold: 0.15 })),
    setMemoryPolicy: vi.fn(),
    // MI-W4.4 — `HygieneQueue` renders unconditionally on /memory now;
    // mocked quiet-empty so it never surfaces an unrelated `role="alert"`
    // in tests asserting "no alert" for a DIFFERENT control. FIX2 —
    // `high_salience_threshold`/`dormant_days` are now REQUIRED wire
    // fields (also gates whether the quadrant scatter renders at all).
    fetchMemoryTriage: vi.fn(async () => ({
      items: [],
      scanned: 0,
      high_salience_threshold: 0.7,
      dormant_days: 60,
    })),
    // MI-W4.3 — the lineage viewer's fetch; mocked per-test where needed.
    fetchMemoryLineage: vi.fn(),
    // CT-E4 — the provenance dossier now opens from this route's rows even
    // without an origin session, so its own data lanes need quiet defaults:
    // the CT-B2 recall-history fetch and the CT-B6 coderef-hints fetch.
    fetchMemoryRecalledBy: vi.fn(async () => ({ rows: [] })),
    fetchCodeRefs: vi.fn(async () => ({ ref_count: 0, never_scanned: true, refs: [] })),
  };
});

// jsdom's <dialog> doesn't implement showModal() in every version this repo
// might run under (the lineage viewer opens a native <dialog>).
beforeAll(() => {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
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

const HIT: RecallHit = {
  id: "abc123abc123",
  kb: "mem",
  title: "Deploy pipeline",
  path: "/srv/mem/deploy.html",
  source_relative: "deploy.html",
  score: 0.5,
  salience: 0.42,
  pinned: false,
  global: true,
  linked_kbs: [],
  recall_count: 0,
  recall_used_count: 0,
  recall_weekly: [],
};

function renderMemory(initialEntries: string[] = ["/memory"]) {
  // retry: false — the CT-E4 dossier tests open ProvenanceThread inside
  // this route, whose UNMOCKED query lanes fail fast in jsdom; retrying
  // them would only leave timers running past cleanup.
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={initialEntries}>
        <Memory />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

/// Click the salience button into edit mode, replace the draft value, and
/// blur (the control commits on blur/Enter — see `SalienceEdit`).
function editSalience(next: string) {
  fireEvent.click(screen.getByTestId("memory-salience-value"));
  const input = screen.getByTestId("memory-salience-input") as HTMLInputElement;
  fireEvent.change(input, { target: { value: next } });
  fireEvent.blur(input);
}

describe("Memory route — SalienceEdit control + optimistic rollback", () => {
  beforeEach(() => {
    vi.mocked(useMemories).mockReturnValue({
      hits: [HIT],
      loading: false,
      error: null,
      refresh: vi.fn(),
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders the control, edits the value, and PATCHes the client wrapper with (kb, id, value)", async () => {
    vi.mocked(patchMemorySalience).mockResolvedValueOnce({
      id: HIT.id,
      salience: 0.75,
    });

    renderMemory();
    expect(screen.getByTestId("memory-salience-value").textContent).toBe("0.42");

    editSalience("0.75");

    await waitFor(() => {
      expect(patchMemorySalience).toHaveBeenCalledWith("mem", "abc123abc123", 0.75);
    });
    // Optimistic value sticks once the PATCH resolves.
    await waitFor(() => {
      expect(screen.getByTestId("memory-salience-value").textContent).toBe("0.75");
    });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("rolls back the optimistic value when the PATCH rejects", async () => {
    vi.mocked(patchMemorySalience).mockRejectedValueOnce(new Error("network down"));

    renderMemory();
    editSalience("0.99");

    await waitFor(() => {
      expect(patchMemorySalience).toHaveBeenCalledWith("mem", "abc123abc123", 0.99);
    });
    // The risky path: a rejected PATCH must roll the row back to the
    // pre-edit value, not leave the optimistic (never-persisted) one shown.
    await waitFor(() => {
      expect(screen.getByTestId("memory-salience-value").textContent).toBe("0.42");
    });
    expect(screen.getByRole("alert").textContent).toContain("network down");
  });
});

describe("Memory route — MI-W4.1 decay-policy control (hoisted to useMemoryPolicy)", () => {
  beforeEach(() => {
    vi.mocked(useMemories).mockReturnValue({
      hits: [HIT],
      loading: false,
      error: null,
      refresh: vi.fn(),
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("flips the policy optimistically and keeps it once the PUT resolves", async () => {
    vi.mocked(setMemoryPolicy).mockResolvedValueOnce({
      policy: "strict",
      drop_threshold: 0.2,
    });
    renderMemory();
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "balanced" }).getAttribute("aria-pressed")).toBe(
        "true",
      );
    });

    fireEvent.click(screen.getByRole("button", { name: "strict" }));

    await waitFor(() => {
      expect(setMemoryPolicy).toHaveBeenCalledWith("strict");
    });
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "strict" }).getAttribute("aria-pressed")).toBe(
        "true",
      );
    });
  });

  it("rolls back the optimistic policy flip when the PUT rejects", async () => {
    vi.mocked(setMemoryPolicy).mockRejectedValueOnce(new Error("daemon unreachable"));
    renderMemory();
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "balanced" }).getAttribute("aria-pressed")).toBe(
        "true",
      );
    });

    fireEvent.click(screen.getByRole("button", { name: "loose" }));

    await waitFor(() => {
      expect(screen.getByRole("alert").textContent).toContain("daemon unreachable");
    });
    // Rolled back to the pre-flip policy, not left on the failed "loose".
    expect(screen.getByRole("button", { name: "balanced" }).getAttribute("aria-pressed")).toBe(
      "true",
    );
    expect(screen.getByRole("button", { name: "loose" }).getAttribute("aria-pressed")).toBe(
      "false",
    );
  });
});

describe("Memory route — MI-W4.1/W4.3 row health column + lineage action", () => {
  beforeEach(() => {
    vi.mocked(useMemories).mockReturnValue({
      hits: [{ ...HIT, salience: 0.9, age_days: 0, decay_k: 0.01 }],
      loading: false,
      error: null,
      refresh: vi.fn(),
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders a decay sparkline in the health column for each row", () => {
    renderMemory();
    expect(screen.getByTestId("decay-sparkline")).toBeTruthy();
  });

  it("opens the lineage viewer when the lineage action is clicked", async () => {
    vi.mocked(fetchMemoryLineage).mockResolvedValueOnce({
      id: HIT.id,
      start: {
        id: HIT.id,
        title: HIT.title,
        created_unix: 1_700_000_000,
        forgotten: false,
        pinned: false,
      },
      supersedes_chain: [],
      superseded_by_chain: [],
    } as MemoryLineageResponse);

    renderMemory();
    expect(screen.queryByTestId("lineage-viewer")).toBeNull();

    fireEvent.click(screen.getByTitle("view supersede lineage"));

    await waitFor(() => {
      expect(screen.getByTestId("lineage-viewer")).toBeTruthy();
    });
    await waitFor(() => {
      expect(fetchMemoryLineage).toHaveBeenCalledWith("mem", "abc123abc123", expect.anything());
    });
  });
});

const PROVENANCE_TITLE =
  "open this memory's provenance dossier — origin, currency, attention, and the session chain when one exists";

describe("Memory route — MI-W4.6 → CT-E4 provenance action (widened gate)", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("CT-E4 — the dossier opens for a memory with NO origin session: hand-written origin + honest chain absence", async () => {
    vi.mocked(useMemories).mockReturnValue({
      hits: [HIT], // HIT carries no session_id
      loading: false,
      error: null,
      refresh: vi.fn(),
    });
    renderMemory();
    const btn = screen.getByTitle(PROVENANCE_TITLE) as HTMLButtonElement;
    expect(btn.disabled).toBe(false);

    fireEvent.click(btn);

    await waitFor(() => {
      expect(screen.getByTestId("provenance-thread")).toBeTruthy();
    });
    expect(screen.getByTestId("provenance-origin").textContent).toContain("hand-written");
    // The why-chain renders its honest absence — never the misleading
    // "capture no longer indexed" line, and never a session hop.
    expect(screen.getByTestId("provenance-no-origin-session")).toBeTruthy();
    expect(screen.queryByTestId("provenance-no-session")).toBeNull();
    expect(screen.queryByTestId("provenance-session")).toBeNull();
  });

  it("the dossier still opens for a memory WITH an origin session", async () => {
    vi.mocked(useMemories).mockReturnValue({
      hits: [{ ...HIT, session_id: "sid-abc" }],
      loading: false,
      error: null,
      refresh: vi.fn(),
    });
    renderMemory();
    const btn = screen.getByTitle(PROVENANCE_TITLE) as HTMLButtonElement;
    expect(btn.disabled).toBe(false);

    fireEvent.click(btn);

    await waitFor(() => {
      expect(screen.getByTestId("provenance-thread")).toBeTruthy();
    });
    expect(screen.getByTestId("provenance-origin").textContent).toContain("session-born");
  });
});

describe("Memory route — CT-E4 tension badge + ?sort=unverified", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  // Three rows in recall-score order: A never recalled, B agent-hot-human-
  // cold (5 recalls, never opened), C recalled twice but READ.
  const A = { ...HIT, id: "aaaaaaaaaaaa", title: "A Fact", recall_count: 0 };
  const B = { ...HIT, id: "bbbbbbbbbbbb", title: "B Fact", recall_count: 5 };
  const C = { ...HIT, id: "cccccccccccc", title: "C Fact", recall_count: 2, read_pct: 90 };

  function mockRows(hits = [A, B, C]) {
    vi.mocked(useMemories).mockReturnValue({
      hits,
      loading: false,
      error: null,
      refresh: vi.fn(),
    });
  }

  function rowIds(): (string | null)[] {
    return screen.getAllByTestId("memory-item").map((el) => el.getAttribute("data-id"));
  }

  it("the badge renders the exact CT-B6 wording on an agent-hot-human-cold row only", () => {
    mockRows();
    renderMemory();
    const badges = screen.getAllByTestId("memory-tension");
    expect(badges).toHaveLength(1); // B only — A never recalled, C was read
    expect(badges[0].textContent).toBe("Recalled 5× by agents · never opened by you");
  });

  it("no badge once the human has opened it, or when never recalled", () => {
    mockRows([A, C]);
    renderMemory();
    expect(screen.queryByTestId("memory-tension")).toBeNull();
  });

  it("?sort=unverified orders the bucket first, then recall_count DESC over the cold rest", () => {
    mockRows();
    renderMemory(["/memory?sort=unverified"]);
    // B (hot) → C (cold, 2 recalls) → A (cold, never recalled).
    expect(rowIds()).toEqual([B.id, C.id, A.id]);
    expect(
      screen.getByTestId("memory-sort-unverified").getAttribute("aria-pressed"),
    ).toBe("true");
  });

  it("the control toggles the URL-carried sort: default order is the recall ranking, byte-identical", () => {
    mockRows();
    renderMemory();
    // Absent ?sort= — the recall endpoint's score order, untouched.
    expect(rowIds()).toEqual([A.id, B.id, C.id]);
    const ctl = screen.getByTestId("memory-sort-unverified");
    expect(ctl.getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(ctl);
    expect(ctl.getAttribute("aria-pressed")).toBe("true");
    expect(rowIds()).toEqual([B.id, C.id, A.id]);

    fireEvent.click(ctl);
    expect(ctl.getAttribute("aria-pressed")).toBe("false");
    expect(rowIds()).toEqual([A.id, B.id, C.id]);
  });
});

describe("Memory route — MI-W4.7 scope overlap", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders the scope-overlap summary over the currently-loaded hits", () => {
    vi.mocked(useMemories).mockReturnValue({
      hits: [
        { ...HIT, id: "a", kb: "alpha", global: false, linked_kbs: [] },
        { ...HIT, id: "b", kb: "beta", global: false, linked_kbs: [] },
      ],
      loading: false,
      error: null,
      refresh: vi.fn(),
    });
    renderMemory();
    const rows = screen.getAllByTestId("scope-overlap-row");
    expect(rows).toHaveLength(2);
  });

  it("failure path: renders the scope-overlap empty state when there are no memories", () => {
    vi.mocked(useMemories).mockReturnValue({
      hits: [],
      loading: false,
      error: null,
      refresh: vi.fn(),
    });
    renderMemory();
    expect(screen.getByTestId("scope-overlap-empty")).toBeTruthy();
  });
});
