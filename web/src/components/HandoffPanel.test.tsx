// @vitest-environment jsdom
//
// HandoffPanel — injection happens only on explicit request: no fetch
// before the click, the click asks for the OTHER harness, the server's
// command + brief render, failures show the server's reason, and a harness
// outside the omp ⇄ Claude pair renders nothing.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { HandoffPanel, handoffTargets } from "./HandoffPanel";
import type { HandoffOut } from "../api/generated/HandoffOut";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function out(overrides: Partial<HandoffOut> = {}): HandoffOut {
  return {
    schema: "kb-handoff/1",
    session_id: "abc",
    source_harness: "omp",
    source_kind: "capture",
    target: "claude",
    brief: "[kb-handoff from omp session abc]\nresume the migration",
    chars: 52,
    truncated: [],
    notes: [],
    command: "kb sessions handoff abc --to claude --launch",
    scrubbed: false,
    ...overrides,
  } as HandoffOut;
}

function stubFetch(status: number, body: unknown) {
  const fn = vi.fn(
    async (_url: string | URL, _init?: RequestInit) =>
      new Response(JSON.stringify(body), {
        status,
        statusText: status === 200 ? "OK" : "Not Found",
        headers: { "Content-Type": "application/json" },
      }),
  );
  vi.stubGlobal("fetch", fn);
  return fn;
}

describe("handoffTargets", () => {
  it("maps each side of the pair to the other harness", () => {
    expect(handoffTargets("omp")).toEqual([{ to: "claude", label: "Continue in Claude Code" }]);
    expect(handoffTargets("claude")).toEqual([{ to: "omp", label: "Continue in omp" }]);
    expect(handoffTargets("codex")).toEqual([]);
  });
});

describe("HandoffPanel", () => {
  it("fetches nothing before the click, then asks for the other harness", async () => {
    const fetchFn = stubFetch(200, out());
    render(<HandoffPanel sessionId="abc" harness="omp" />);
    expect(fetchFn).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("session-handoff-claude"));
    await waitFor(() => expect(screen.getByTestId("session-handoff-result")).toBeTruthy());

    expect(fetchFn).toHaveBeenCalledTimes(1);
    const url = String(fetchFn.mock.calls[0][0]);
    expect(url).toContain("/api/sessions/abc/handoff");
    expect(url).toContain("to=claude");
  });

  it("renders the server command, brief, chars, truncated lanes, notes and scrub flag", async () => {
    stubFetch(
      200,
      out({
        truncated: ["tail", "files"],
        notes: ["no capture tail"],
        scrubbed: true,
      }),
    );
    render(<HandoffPanel sessionId="abc" harness="omp" />);
    fireEvent.click(screen.getByTestId("session-handoff-claude"));
    await waitFor(() => expect(screen.getByTestId("session-handoff-cmd")).toBeTruthy());

    expect(screen.getByTestId("session-handoff-cmd").textContent).toBe(
      "kb sessions handoff abc --to claude --launch",
    );
    expect(screen.getByTestId("session-handoff-brief").textContent).toContain(
      "resume the migration",
    );
    expect(screen.getByTestId("session-handoff-chars").textContent).toContain("52");
    expect(screen.getByTestId("session-handoff-truncated").textContent).toContain("tail, files");
    expect(screen.getByTestId("session-handoff-notes").textContent).toContain("no capture tail");
    expect(screen.getByTestId("session-handoff-scrubbed")).toBeTruthy();
    expect(screen.getByTestId("session-handoff-brief-copy")).toBeTruthy();
    expect(screen.getByTestId("session-handoff-cmd-copy")).toBeTruthy();
  });

  it("omits the truncated/notes/scrubbed lines when empty", async () => {
    stubFetch(200, out());
    render(<HandoffPanel sessionId="abc" harness="omp" />);
    fireEvent.click(screen.getByTestId("session-handoff-claude"));
    await waitFor(() => expect(screen.getByTestId("session-handoff-result")).toBeTruthy());
    expect(screen.queryByTestId("session-handoff-truncated")).toBeNull();
    expect(screen.queryByTestId("session-handoff-notes")).toBeNull();
    expect(screen.queryByTestId("session-handoff-scrubbed")).toBeNull();
  });

  it("shows the problem+json reason when the fetch fails", async () => {
    stubFetch(404, { title: "Not Found", detail: "no capture for session abc" });
    render(<HandoffPanel sessionId="abc" harness="claude" />);
    fireEvent.click(screen.getByTestId("session-handoff-omp"));
    await waitFor(() => expect(screen.getByTestId("session-handoff-error")).toBeTruthy());
    expect(screen.getByTestId("session-handoff-error").textContent).toContain(
      "Not Found: no capture for session abc",
    );
    expect(screen.queryByTestId("session-handoff-result")).toBeNull();
  });

  it("remounts per session: the previous brief is gone and nothing is fetched for the new one", async () => {
    const fetchFn = stubFetch(200, out());
    const { rerender } = render(<HandoffPanel key="a" sessionId="a" harness="omp" />);
    fireEvent.click(screen.getByTestId("session-handoff-claude"));
    await waitFor(() => expect(screen.getByTestId("session-handoff-result")).toBeTruthy());
    expect(fetchFn).toHaveBeenCalledTimes(1);

    rerender(<HandoffPanel key="b" sessionId="b" harness="omp" />);
    expect(screen.queryByTestId("session-handoff-result")).toBeNull();
    expect(screen.queryByTestId("session-handoff-brief")).toBeNull();
    expect(screen.queryByTestId("session-handoff-cmd")).toBeNull();
    expect(fetchFn).toHaveBeenCalledTimes(1);
  });

  it("renders nothing for a harness outside the omp ⇄ Claude pair", () => {
    const fetchFn = stubFetch(200, out());
    const { container } = render(<HandoffPanel sessionId="abc" harness="codex" />);
    expect(container.firstChild).toBeNull();
    expect(fetchFn).not.toHaveBeenCalled();
  });
});
