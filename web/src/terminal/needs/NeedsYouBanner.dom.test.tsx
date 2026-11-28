// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

const liveMock = vi.hoisted(() => vi.fn());
const convMock = vi.hoisted(() => vi.fn());
vi.mock("../../hooks/useLiveSessions", () => ({ useLiveSessions: liveMock }));
vi.mock("../../hooks/useConversation", () => ({ useConversation: convMock }));

import { NeedsYouBanner } from "./NeedsYouBanner";
import { TermRailContext } from "../rail/context";

const SINCE = 1_700_000_000;
const blockedRow = (over: Record<string, unknown> = {}) => ({ session_id: "sid-1", blocked: true, blocked_since_unix: SINCE, ...over });
const okConv = (q: string, gen = `${SINCE + 1}-1`) => ({
  data: {
    kind: "ok",
    kb: "demo",
    conversation: {
      schema: "conversation/1", total: 1, from: 1, next_cursor: null, capture_gen: gen, unchanged: false,
      rows: [{ n: 1, id: "t-1", ts: null, prompt: "p", answer: null, asks: [{ kind: "ask", q, options: ["a", "b"], answer: null, pending: true }], commits: [], files_touched: 0, errors: 0, subagents: { n: 0, errors: 0 } }],
    },
  },
});

function mount(sid: string | undefined, onJump = vi.fn()) {
  const ui = (
    <TermRailContext.Provider value={{ termId: "t1", harness: "claude", sid }}>
      <NeedsYouBanner onJump={onJump} />
    </TermRailContext.Provider>
  );
  const r = render(ui);
  return { ...r, onJump, again: () => r.rerender(ui) };
}

beforeEach(() => {
  convMock.mockReturnValue({ data: undefined });
});
afterEach(() => {
  cleanup();
  liveMock.mockReset();
  convMock.mockReset();
});

describe("NeedsYouBanner", () => {
  it("renders the capture question as a text node with options and the capture caption", () => {
    liveMock.mockReturnValue({ rows: [blockedRow()], loading: false });
    const evil = "<img src=x onerror=alert(1)>";
    convMock.mockReturnValue(okConv(evil));
    mount("sid-1");
    const banner = screen.getByTestId("needs-you");
    expect(banner.getAttribute("data-source")).toBe("capture");
    expect(banner.getAttribute("data-kind")).toBe("question");
    expect(screen.getByTestId("needs-you-text").textContent).toBe(evil);
    expect(banner.querySelector("img")).toBeNull();
    expect(screen.getByTestId("needs-you-options").textContent).toBe("a / b");
    expect(screen.getByTestId("needs-you-fresh").textContent).toBe("From the last capture");
    expect(banner.getAttribute("role")).toBeNull();
  });

  it("beat text with a tool chip and the pending-capture caption", () => {
    liveMock.mockReturnValue({ rows: [blockedRow({ detail_reason: "needs your permission", blocked_tool: "Bash", last_line: "ran ls" })], loading: false });
    mount("sid-1");
    expect(screen.getByTestId("needs-you").getAttribute("data-source")).toBe("beat");
    expect(screen.getByTestId("needs-you-text").textContent).toBe("needs your permission");
    expect(screen.getByTestId("needs-you-tool").textContent).toBe("Bash");
    expect(screen.getByTestId("needs-you-last").textContent).toBe("ran ls");
    expect(screen.getByTestId("needs-you-fresh").textContent).toBe("Question text appears after the next capture");
  });

  it("is absent with no sid, no row, or an unblocked row", () => {
    liveMock.mockReturnValue({ rows: [blockedRow()], loading: false });
    mount(undefined);
    expect(screen.queryByTestId("needs-you")).toBeNull();
    cleanup();
    liveMock.mockReturnValue({ rows: [blockedRow({ session_id: "other" })], loading: false });
    mount("sid-1");
    expect(screen.queryByTestId("needs-you")).toBeNull();
    cleanup();
    liveMock.mockReturnValue({ rows: [blockedRow({ blocked: false })], loading: false });
    mount("sid-1");
    expect(screen.queryByTestId("needs-you")).toBeNull();
  });

  it("unmounts when the row flips unblocked", () => {
    liveMock.mockReturnValue({ rows: [blockedRow()], loading: false });
    const { again } = mount("sid-1");
    expect(screen.getByTestId("needs-you")).toBeTruthy();
    liveMock.mockReturnValue({ rows: [blockedRow({ blocked: false })], loading: false });
    again();
    expect(screen.queryByTestId("needs-you")).toBeNull();
  });

  it("the jump button calls onJump", () => {
    liveMock.mockReturnValue({ rows: [blockedRow()], loading: false });
    const { onJump } = mount("sid-1");
    fireEvent.click(screen.getByTestId("needs-you-jump"));
    expect(onJump).toHaveBeenCalledTimes(1);
  });

  it("the conversation query is enabled only while blocked", () => {
    liveMock.mockReturnValue({ rows: [blockedRow({ blocked: false })], loading: false });
    mount("sid-1");
    expect(convMock).toHaveBeenLastCalledWith(undefined);
    cleanup();
    liveMock.mockReturnValue({ rows: [blockedRow()], loading: false });
    mount("sid-1");
    expect(convMock).toHaveBeenLastCalledWith("sid-1");
  });
});
