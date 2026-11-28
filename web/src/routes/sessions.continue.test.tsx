// @vitest-environment jsdom
//
// HUBHOST — the list row's Continue door: a link to the hub launcher that does
// not select the row, is hidden for an unresumable row, and never POSTs.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import type { SessionRow } from "../api/sessions";

const m = vi.hoisted(() => ({ identity: { terminal_admitted: true } as { terminal_admitted: boolean } | null }));
vi.mock("../hooks/useArtifactHost", async (orig) => {
  const real = await orig<typeof import("../hooks/useArtifactHost")>();
  return { ...real, useIdentity: () => m.identity };
});
vi.mock("../api/sse", () => ({ sse: { subscribeEvent: () => () => {}, onResync: () => () => {} } }));

import { SessionListRow } from "./sessions";

const mk = (over: Partial<SessionRow> = {}): SessionRow =>
  ({
    id: "s",
    kb: "kb",
    artifact_id: "a",
    session_id: "abcdef0123456789",
    started_at: 1_700_000_000,
    ended_at: 1_700_003_600,
    duration_ms: 3_600_000,
    message_count: 12,
    memory_count: 0,
    commit_count: 0,
    error_count: 0,
    source_relative: "x.html",
    display_name: "Row",
    folder: "widget",
    harness: "claude",
    cwd: "/srv/w",
    files_read_count: 0,
    files_edited_count: 0,
    ...over,
  }) as SessionRow;

function renderRow(row: SessionRow, onSelect = () => {}) {
  return render(
    <MemoryRouter>
      <ul>
        <SessionListRow
          row={row}
          isSelected={false}
          onSelect={onSelect}
          onPrefetch={() => {}}
          onCancelPrefetch={() => {}}
          presenceSet={new Set()}
          now={1_800_000_000_000}
        />
      </ul>
    </MemoryRouter>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  m.identity = { terminal_admitted: true };
});

describe("SessionListRow Continue door", () => {
  it("links to the deep link from the row's own session id, and a click does not select the row", () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    const onSelect = vi.fn();
    renderRow(mk(), onSelect);
    const a = screen.getByTestId("continue-in-terminal");
    expect(a.getAttribute("href")).toBe("/terminals?launch=1&resume=abcdef0123456789");
    fireEvent.click(a);
    expect(onSelect).not.toHaveBeenCalled();
    expect(fetchSpy).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button"));
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it("is hidden for a kimi row, a row without cwd, and when not admitted", () => {
    renderRow(mk({ harness: "kimi" }));
    expect(screen.queryByTestId("continue-in-terminal")).toBeNull();
    cleanup();
    renderRow(mk({ cwd: undefined }));
    expect(screen.queryByTestId("continue-in-terminal")).toBeNull();
    cleanup();
    m.identity = { terminal_admitted: false };
    renderRow(mk());
    expect(screen.queryByTestId("continue-in-terminal")).toBeNull();
  });
});
