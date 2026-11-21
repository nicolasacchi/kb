// @vitest-environment jsdom
// FUTERMATTN - the blocked count reaches the tab title and the app badge from
// the EXISTING live-status query (no new poll, no new stream).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import WaitingChip from "../components/chrome/WaitingChip";
import { LIVE_SESSIONS_KEY } from "./useLiveSessions";
import { useDocumentTitle } from "./useDocumentTitle";
import { fetchLiveStatus, type LiveStatusRow } from "../api/sessions";

vi.mock("../api/sessions", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/sessions")>();
  return { ...actual, fetchLiveStatus: vi.fn() };
});

function row(o: Partial<LiveStatusRow> & Pick<LiveStatusRow, "session_id">): LiveStatusRow {
  return {
    harness: "claude",
    holder: "human",
    state: "working",
    source: "hook",
    confidence: "observed",
    since_unix: 1_800_000_000,
    since_secs: 0,
    resume: `claude -r ${o.session_id}`,
    blocked: true,
    ...o,
  };
}

function Page() {
  useDocumentTitle("Sessions");
  return null;
}

let qc: QueryClient;
const setAppBadge = vi.fn(() => Promise.resolve());
const clearAppBadge = vi.fn(() => Promise.resolve());

beforeEach(() => {
  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  Object.defineProperty(navigator, "setAppBadge", { configurable: true, value: setAppBadge });
  Object.defineProperty(navigator, "clearAppBadge", { configurable: true, value: clearAppBadge });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  delete (navigator as unknown as Record<string, unknown>).setAppBadge;
  delete (navigator as unknown as Record<string, unknown>).clearAppBadge;
  document.title = "kb";
});

function mount(withPage: boolean) {
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        {withPage ? <Page /> : null}
        <WaitingChip />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("attention badge", () => {
  it("the title shows (1) for one blocked session, the app badge follows, and answering clears both", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValue([row({ session_id: "a" })]);
    mount(false);
    await waitFor(() => expect(document.title).toBe("(1) kb"));
    expect(setAppBadge).toHaveBeenLastCalledWith(1);
    act(() => {
      qc.setQueryData(LIVE_SESSIONS_KEY, [row({ session_id: "a", blocked: false, state: "working" })]);
    });
    await waitFor(() => expect(document.title).toBe("kb"));
    expect(clearAppBadge).toHaveBeenCalled();
  });

  it("composes with a route title and survives its re-render", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValue([row({ session_id: "a" }), row({ session_id: "b" })]);
    mount(true);
    await waitFor(() => expect(document.title).toBe("(2) Sessions · kb"));
  });

  it("a quiet fleet leaves the title alone and never calls the badge API with 0", async () => {
    vi.mocked(fetchLiveStatus).mockResolvedValue([row({ session_id: "a", blocked: false })]);
    mount(false);
    await waitFor(() => expect(fetchLiveStatus).toHaveBeenCalled());
    expect(document.title).toBe("kb");
    expect(setAppBadge).not.toHaveBeenCalledWith(0);
  });
});
