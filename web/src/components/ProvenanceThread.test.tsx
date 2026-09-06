// @vitest-environment jsdom
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import ProvenanceThread from "./ProvenanceThread";
import { useCommitFiles, useSessionDetail } from "../hooks/useSessions";
import {
  useMemoryCommittedIn,
  useMemoryLineage,
  useMemoryRecalledBy,
} from "../hooks/useMemories";
import { useCodeRefs } from "../hooks/useCodeRefs";
import { useDocLens, useDocLensScorecard } from "../hooks/useDocLens";
import { useCodeUrlForKb } from "../hooks/useCodeUrlForKb";
import type { CommitOut } from "../api/sessions";
import type { RecallHit } from "../api/client";

vi.mock("../hooks/useSessions", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../hooks/useSessions")>();
  return {
    ...actual,
    useSessionDetail: vi.fn(),
    useCommitFiles: vi.fn(),
  };
});

// CT-B2 — the provenance modal now ALSO calls `useMemoryRecalledBy`
// unconditionally (it decides internally whether `kb`/`id` are known), so
// every existing test needs a default mock return, same as the two above.
// CT-B6 adds `useMemoryLineage` (the dossier's Currency section).
vi.mock("../hooks/useMemories", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../hooks/useMemories")>();
  return {
    ...actual,
    useMemoryRecalledBy: vi.fn(),
    useMemoryLineage: vi.fn(),
    // CT-F1 — the exact-id citation lane, called unconditionally for the
    // same reason `useMemoryRecalledBy` is (the component decides
    // internally whether kb/id are known).
    useMemoryCommittedIn: vi.fn(),
  };
});

// CT-B6 — the code-citation freshness lane (same-origin coderef/1 hints +
// the cross-daemon doclens pair) and the code_url resolve. All mocked at
// the hook seam so no test needs a QueryClientProvider or network.
vi.mock("../hooks/useCodeRefs", () => ({ useCodeRefs: vi.fn() }));
vi.mock("../hooks/useDocLens", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../hooks/useDocLens")>();
  return {
    ...actual,
    useDocLens: vi.fn(),
    useDocLensScorecard: vi.fn(),
  };
});
vi.mock("../hooks/useCodeUrlForKb", () => ({ useCodeUrlForKb: vi.fn() }));

// CT-B6 — ProvenanceChip owns its own TanStack query (the U3 origin
// resolve); stubbed so the highlight-born header renders without a
// QueryClientProvider.
vi.mock("./ProvenanceChip", () => ({
  default: () => <span data-testid="stub-provenance-chip" />,
}));

/// A loose UseQueryResult stand-in: only the fields ProvenanceThread reads
/// (`data`/`isError`/`error`/`refetch`), cast wide so one helper serves
/// every mocked query hook.
function q(partial: Record<string, unknown> = {}) {
  return {
    data: undefined,
    isError: false,
    error: null,
    refetch: vi.fn(),
    ...partial,
  } as never;
}

// jsdom's <dialog> support doesn't implement showModal() in every version
// this repo might run under — stub it so mounting never throws, matching
// LineageViewer's precedent.
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

beforeEach(() => {
  vi.mocked(useMemoryRecalledBy).mockReturnValue({ rows: [], loading: false, error: null });
  vi.mocked(useMemoryCommittedIn).mockReturnValue({ rows: [], loading: false, error: null });
  vi.mocked(useMemoryLineage).mockReturnValue(q());
  vi.mocked(useCodeRefs).mockReturnValue(q());
  vi.mocked(useDocLensScorecard).mockReturnValue(q());
  vi.mocked(useDocLens).mockReturnValue(q());
  vi.mocked(useCodeUrlForKb).mockReturnValue(null);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function commit(overrides: Partial<CommitOut> = {}): CommitOut {
  return {
    kind: "commit",
    sha: "abc12345",
    sha_full: "abc12345def67890abc12345def67890abc1234",
    subject: "fix: thing",
    resolved: true,
    trailers: [],
    ...overrides,
  };
}

const DETAIL = {
  memory_ids: [],
  id: "row1",
  kb: "sessions",
  artifact_id: "art1",
  session_id: "sid-123",
  started_at: 1_700_000_000,
  ended_at: 1_700_003_600,
  duration_ms: 3_600_000,
  message_count: 20,
  memory_count: 1,
  source_relative: "sessions/sid-123.html",
  display_name: "Fix the thing",
  files_read_count: 3,
  files_edited_count: 2,
  token_total: 5000,
  tool_calls: 10,
  error_count: 0,
  subagent_count: 0,
  subagent_tokens: 0,
  subagent_tool_calls: 0,
  subagent_files_edited: 0,
  subagent_launched_unstatted: 0,
  harness: "claude",
  active_secs: 1800,
  user_turns: 5,
  commit_count: 1,
} as unknown as ReturnType<typeof useSessionDetail>["detail"];

function renderThread(
  sessionId: string | null = "sid-123",
  memory?: { kb: string; id: string },
  hit?: RecallHit,
) {
  return render(
    <MemoryRouter>
      <ProvenanceThread
        sessionId={sessionId}
        memoryKb={memory?.kb}
        memoryId={memory?.id}
        hit={hit}
        onClose={vi.fn()}
      />
    </MemoryRouter>,
  );
}

/// CT-B6 — a minimal recall row for dossier-mode tests.
const HIT: RecallHit = {
  id: "aaaaaaaaaaaa",
  kb: "notes",
  title: "Deploy pipeline",
  path: "/srv/notes/mem.html",
  source_relative: "mem.html",
  score: 0.5,
  salience: 0.42,
  pinned: false,
  global: false,
  linked_kbs: [],
  recall_count: 0,
  recall_used_count: 0,
  recall_weekly: [],
};

/// CT-B6 — most dossier tests want a quiet, loaded session chain below the
/// new sections; one helper instead of the full literal per test.
function mockSessionLoaded(commits: CommitOut[] = []) {
  vi.mocked(useSessionDetail).mockReturnValue({
    detail: DETAIL,
    memories: [],
    recalls: [],
    touches: null,
    readings: [],
    files: [],
    decisions: [],
    commits,
    research: [],
    comments: { artifacts: [], total: 0, raised: [] },
    loading: false,
  });
}

describe("ProvenanceThread", () => {
  it("shows a loading state while the session detail is pending", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: null,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: true,
    });
    renderThread();
    expect(screen.getByText("loading…")).toBeTruthy();
  });

  it("failure path: renders an honest empty state when the session is no longer indexed", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: null,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    renderThread();
    expect(screen.getByTestId("provenance-no-session")).toBeTruthy();
    expect(screen.queryByTestId("provenance-session")).toBeNull();
  });

  it("renders the session hop and the commit list", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [commit(), commit({ sha: "def45678", subject: "docs: update" })],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    renderThread();
    expect(screen.getByTestId("provenance-session").textContent).toContain("Fix the thing");
    const commitRows = screen.getAllByTestId("provenance-commit");
    expect(commitRows).toHaveLength(2);
    expect(commitRows[0].textContent).toContain("fix: thing");
    expect(commitRows[1].textContent).toContain("docs: update");
  });

  it("an unresolved commit's toggle is disabled (no sha to look up files for)", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [commit({ resolved: false, sha_full: undefined })],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    renderThread();
    const toggle = screen.getByTestId("provenance-commit").querySelector("button")!;
    expect((toggle as HTMLButtonElement).disabled).toBe(true);
  });

  it("expanding a resolved commit fetches and shows its touched files with staleness", async () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [commit()],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    vi.mocked(useCommitFiles).mockReturnValue({
      data: {
        available: true,
        truncated: false,
        files: [
          { path: "src/a.rs", last_touched_unix: Math.floor(Date.now() / 1000) - 86400, changed_since: true },
          { path: "src/b.rs", last_touched_unix: Math.floor(Date.now() / 1000) - 86400 * 10, changed_since: false },
        ],
      },
      loading: false,
      error: null,
    });
    renderThread();
    fireEvent.click(screen.getByTestId("provenance-commit").querySelector("button")!);
    let filesList!: HTMLElement;
    await waitFor(() => {
      filesList = screen.getByTestId("provenance-commit-files");
      expect(filesList).toBeTruthy();
    });
    expect(within(filesList).getByText("src/a.rs")).toBeTruthy();
    expect(within(filesList).getByText(/changed again/)).toBeTruthy();
    expect(within(filesList).getByText(/unchanged since this commit/)).toBeTruthy();
  });

  it("failure path: a commit-files lookup error surfaces as an alert", async () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [commit()],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    vi.mocked(useCommitFiles).mockReturnValue({
      data: null,
      loading: false,
      error: "network down",
    });
    renderThread();
    fireEvent.click(screen.getByTestId("provenance-commit").querySelector("button")!);
    await waitFor(() => {
      expect(screen.getByRole("alert").textContent).toContain("network down");
    });
  });

  // CT-B2 — "Recall history" section.

  it("recall history is absent when no memory identity is known", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    renderThread();
    expect(screen.queryByTestId("provenance-recalled-by")).toBeNull();
    expect(useMemoryRecalledBy).toHaveBeenCalledWith(null, null);
  });

  it("recall history: an honest empty state when nothing was ever recalled", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    renderThread("sid-123", { kb: "notes", id: "aaaaaaaaaaaa" });
    expect(useMemoryRecalledBy).toHaveBeenCalledWith("notes", "aaaaaaaaaaaa");
    const section = screen.getByTestId("provenance-recalled-by");
    expect(within(section).getByText(/no recalls the capture pipeline saw/)).toBeTruthy();
  });

  it("recall history: renders a row with title, short id, and turn marker", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    vi.mocked(useMemoryRecalledBy).mockReturnValue({
      rows: [
        {
          session_kb: "sessions",
          session_id: "sid-999-longer-than-eight",
          session_title: "Fix the reconcile bug",
          turn_id: "t-0123456789ab",
          recalled_at: 1_700_000_000,
        },
      ],
      loading: false,
      error: null,
    });
    renderThread("sid-123", { kb: "notes", id: "aaaaaaaaaaaa" });
    const rows = screen.getAllByTestId("provenance-recall-row");
    expect(rows).toHaveLength(1);
    expect(rows[0].textContent).toContain("Fix the reconcile bug");
    expect(rows[0].textContent).toContain("sid-999-");
    expect(rows[0].textContent).toContain("t-0123456789ab");
  });

  it("recall history: a lookup error surfaces as an alert", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    vi.mocked(useMemoryRecalledBy).mockReturnValue({
      rows: [],
      loading: false,
      error: "network down",
    });
    renderThread("sid-123", { kb: "notes", id: "aaaaaaaaaaaa" });
    expect(screen.getByRole("alert").textContent).toContain("network down");
  });
});

// CT-F1 — the exact-id citation section ("Cited in commits").
describe("ProvenanceThread — CT-F1 exact-id citations", () => {
  beforeEach(() => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: DETAIL,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
  });

  it("renders one row per citing commit, with sha, subject and repo", () => {
    vi.mocked(useMemoryCommittedIn).mockReturnValue({
      rows: [
        {
          session_kb: "sessions",
          session_id: "sid-999",
          sha_full: "deadbeef00112233445566778899aabbccddeeff",
          sha: "deadbeef",
          subject: "feat: use the remembered cap",
          repo_root: "/home/user/project/kb",
          recorded_at: 1_700_000_000,
        },
      ],
      loading: false,
      error: null,
    });
    renderThread("sid-123", { kb: "notes", id: "aaaaaaaaaaaa" });
    expect(useMemoryCommittedIn).toHaveBeenCalledWith("notes", "aaaaaaaaaaaa");
    const section = screen.getByTestId("provenance-committed-in");
    const rows = within(section).getAllByTestId("provenance-exact-commit");
    expect(rows).toHaveLength(1);
    expect(rows[0].textContent).toContain("deadbeef");
    expect(rows[0].textContent).toContain("feat: use the remembered cap");
    expect(rows[0].textContent).toContain("/home/user/project/kb");
    // The honesty hint must ride the section wherever it renders.
    expect(section.textContent).toContain("opt-in per repo");
  });

  it("renders NOTHING when no commit cited it (opt-in gate is off by default)", () => {
    renderThread("sid-123", { kb: "notes", id: "aaaaaaaaaaaa" });
    expect(screen.queryByTestId("provenance-committed-in")).toBeNull();
  });

  it("is absent entirely when no memory identity is known", () => {
    renderThread();
    expect(screen.queryByTestId("provenance-committed-in")).toBeNull();
    expect(useMemoryCommittedIn).not.toHaveBeenCalled();
  });

  it("a lookup error surfaces as an alert rather than a silent empty", () => {
    vi.mocked(useMemoryCommittedIn).mockReturnValue({
      rows: [],
      loading: false,
      error: "network down",
    });
    renderThread("sid-123", { kb: "notes", id: "aaaaaaaaaaaa" });
    const section = screen.getByTestId("provenance-committed-in");
    expect(within(section).getByRole("alert").textContent).toContain("network down");
  });
});

// CT-B6 — the memory dossier (Origin / Currency / Attention).
describe("ProvenanceThread — CT-B6 dossier", () => {
  beforeEach(() => {
    mockSessionLoaded();
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  // --- Origin classes ----------------------------------------------------

  it("origin: highlight-born renders its class line + the origin-artifact chip", () => {
    renderThread("sid-123", undefined, {
      ...HIT,
      session_id: "sid-123",
      source_kb: "research",
      source_artifact: "bbbbbbbbbbbb",
      author: "you",
    });
    const origin = screen.getByTestId("provenance-origin");
    expect(origin.textContent).toContain("highlight-born");
    expect(origin.textContent).toContain("by you");
    // The U3 origin-artifact link (ProvenanceChip, stubbed) renders only
    // for this class.
    expect(within(origin).getByTestId("stub-provenance-chip")).toBeTruthy();
  });

  it("origin: session-born when only session provenance exists", () => {
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123" });
    const origin = screen.getByTestId("provenance-origin");
    expect(origin.textContent).toContain("session-born");
    expect(within(origin).queryByTestId("stub-provenance-chip")).toBeNull();
  });

  it("origin: hand-written when neither provenance set exists", () => {
    renderThread("sid-123", undefined, { ...HIT });
    const origin = screen.getByTestId("provenance-origin");
    expect(origin.textContent).toContain("hand-written");
    expect(within(origin).queryByTestId("stub-provenance-chip")).toBeNull();
  });

  // CT-E4 (owed CT-B6 follow-up) — the widened gate: sessionId is nullable
  // and the dossier renders for a session-less memory.

  it("CT-E4 — a null sessionId renders the dossier with an honest chain absence, no session fetch UI", () => {
    vi.mocked(useSessionDetail).mockReturnValue({
      detail: null,
      memories: [],
      recalls: [],
      touches: null,
      readings: [],
      files: [],
      decisions: [],
      commits: [],
      research: [],
      comments: { artifacts: [], total: 0, raised: [] },
      loading: false,
    });
    renderThread(null, { kb: "notes", id: "aaaaaaaaaaaa" }, { ...HIT, recall_count: 3 });
    expect(screen.getByTestId("provenance-origin").textContent).toContain("hand-written");
    // Attention still renders (the tension line: 3 recalls, never opened).
    expect(screen.getByTestId("provenance-tension").textContent).toBe(
      "Recalled 3× by agents · never opened by you",
    );
    // The chain's absence is its OWN message — never the misleading
    // "capture no longer indexed", never a session hop.
    expect(screen.getByTestId("provenance-no-origin-session")).toBeTruthy();
    expect(screen.queryByTestId("provenance-no-session")).toBeNull();
    expect(screen.queryByTestId("provenance-session")).toBeNull();
  });

  it("no dossier sections render when no hit is passed (pre-B6 callers)", () => {
    renderThread("sid-123");
    expect(screen.queryByTestId("provenance-origin")).toBeNull();
    expect(screen.queryByTestId("provenance-currency")).toBeNull();
    expect(screen.queryByTestId("provenance-attention")).toBeNull();
  });

  // --- Attention: agent-hot-human-cold -----------------------------------

  it("attention: the tension line appears exactly when recall_count>0 and read_pct is absent", () => {
    renderThread("sid-123", undefined, {
      ...HIT,
      session_id: "sid-123",
      recall_count: 14,
      recall_used_count: 3,
    });
    expect(screen.getByTestId("provenance-tension").textContent).toBe(
      "Recalled 14× by agents · never opened by you",
    );
    expect(screen.getByTestId("provenance-receipts").textContent).toContain(
      "14 recalls · 3 referenced",
    );
    expect(screen.queryByTestId("provenance-readstate")).toBeNull();
  });

  it("attention: no tension line once the human has opened it — read state shows instead", () => {
    renderThread("sid-123", undefined, {
      ...HIT,
      session_id: "sid-123",
      recall_count: 14,
      recall_used_count: 3,
      read_pct: 80,
      last_read_at: 1_700_000_000,
    });
    expect(screen.queryByTestId("provenance-tension")).toBeNull();
    const readstate = screen.getByTestId("provenance-readstate");
    expect(readstate.textContent).toContain("read 80% by you");
    expect(readstate.textContent).toContain("2023-11-14");
  });

  it("attention: no tension line when never recalled (nothing is agent-hot)", () => {
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123" });
    expect(screen.queryByTestId("provenance-tension")).toBeNull();
    expect(screen.getByTestId("provenance-readstate").textContent).toBe(
      "never opened by you",
    );
  });

  // --- Currency: lineage + flag ------------------------------------------

  it("currency: a superseded memory is called out", () => {
    vi.mocked(useMemoryLineage).mockReturnValue(
      q({
        data: {
          id: HIT.id,
          start: { id: HIT.id, title: HIT.title, forgotten: false, pinned: false },
          supersedes_chain: [],
          superseded_by_chain: [
            { id: "newer1", title: "Corrected fact", forgotten: false, pinned: false },
          ],
        },
      }),
    );
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123" });
    expect(screen.getByTestId("provenance-superseded").textContent).toContain(
      "superseded by Corrected fact",
    );
  });

  it("currency: an open flag renders its warning line", () => {
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123", flagged: true });
    expect(screen.getByTestId("provenance-flagged").textContent).toContain(
      "flagged as wrong",
    );
  });

  // --- Currency: code-citation freshness ---------------------------------

  it("currency: kb-code unreachable is its own state, distinct from drifted", () => {
    vi.mocked(useCodeUrlForKb).mockReturnValue("https://kbc.example");
    vi.mocked(useCodeRefs).mockReturnValue(
      q({ data: { ref_count: 3, never_scanned: false } }),
    );
    vi.mocked(useDocLensScorecard).mockReturnValue(
      q({ isError: true, error: new Error("fetch failed") }),
    );
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123" });
    const code = screen.getByTestId("provenance-code");
    expect(code.textContent).toContain("cites 3 code locations");
    expect(screen.getByTestId("provenance-code-unreachable").textContent).toContain(
      "kb-code unreachable",
    );
    expect(code.textContent).not.toContain("drifted");
    expect(screen.queryByTestId("provenance-code-drift")).toBeNull();
  });

  it("currency: drifted count renders from the pinned checkout's lens — no 'unreachable'", () => {
    vi.mocked(useCodeUrlForKb).mockReturnValue("https://kbc.example");
    vi.mocked(useCodeRefs).mockReturnValue(
      q({ data: { ref_count: 3, never_scanned: false } }),
    );
    vi.mocked(useDocLensScorecard).mockReturnValue(
      q({ data: { pinned_repo: "app" } }),
    );
    vi.mocked(useDocLens).mockReturnValue(
      q({
        data: {
          counts: { drifted: 2 },
          resolved_unix: Math.floor(Date.now() / 1000) - 60,
        },
      }),
    );
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123" });
    const code = screen.getByTestId("provenance-code");
    expect(code.textContent).toContain("cites 3 code locations");
    expect(screen.getByTestId("provenance-code-drift").textContent).toContain("2 drifted");
    expect(code.textContent).not.toContain("unreachable");
    expect(screen.queryByTestId("provenance-code-unreachable")).toBeNull();
  });

  it("currency: hints count only when the kb has no code_url (no cross-daemon call)", () => {
    vi.mocked(useCodeUrlForKb).mockReturnValue(null);
    vi.mocked(useCodeRefs).mockReturnValue(
      q({ data: { ref_count: 3, never_scanned: false } }),
    );
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123" });
    const code = screen.getByTestId("provenance-code");
    expect(code.textContent).toContain("cites 3 code locations");
    expect(screen.queryByTestId("provenance-code-unreachable")).toBeNull();
    expect(screen.queryByTestId("provenance-code-drift")).toBeNull();
    expect(screen.queryByTestId("provenance-code-unpinned")).toBeNull();
  });

  it("currency: freshness is honestly unchecked when no checkout is pinned", () => {
    vi.mocked(useCodeUrlForKb).mockReturnValue("https://kbc.example");
    vi.mocked(useCodeRefs).mockReturnValue(
      q({ data: { ref_count: 3, never_scanned: false } }),
    );
    vi.mocked(useDocLensScorecard).mockReturnValue(
      q({ data: { pinned_repo: null } }),
    );
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123" });
    expect(screen.getByTestId("provenance-code-unpinned").textContent).toContain(
      "no checkout pinned",
    );
    expect(screen.queryByTestId("provenance-code-drift")).toBeNull();
    expect(screen.queryByTestId("provenance-code-unreachable")).toBeNull();
  });

  it("currency: no code line at all when the memory cites no code", () => {
    vi.mocked(useCodeUrlForKb).mockReturnValue("https://kbc.example");
    vi.mocked(useCodeRefs).mockReturnValue(
      q({ data: { ref_count: 0, never_scanned: false } }),
    );
    renderThread("sid-123", undefined, { ...HIT, session_id: "sid-123" });
    expect(screen.queryByTestId("provenance-code")).toBeNull();
  });
});
