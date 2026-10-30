// @vitest-environment jsdom
//
// OL R4 — the worklog row: title + time, closure line, ONE facts line.
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { SessionListRow } from "./sessions";
import type { SessionEvidenceOut, SessionRow } from "../api/sessions";

function mk(over: Partial<SessionRow>): SessionRow {
  return {
    id: "s",
    kb: "kb",
    artifact_id: "a",
    session_id: "abcdef0123456789",
    started_at: 1_700_000_000,
    ended_at: 1_700_003_600,
    duration_ms: 3_600_000,
    message_count: 12,
    memory_count: 2,
    commit_count: 0,
    error_count: 0,
    source_relative: "x.html",
    display_name: "",
    folder: "widget",
    harness: "claude",
    files_read_count: 0,
    files_edited_count: 0,
    ...over,
  } as SessionRow;
}

function renderRow(row: SessionRow) {
  return render(
    <MemoryRouter>
      <ul>
        <SessionListRow
          row={row}
          isSelected={false}
          onSelect={() => {}}
          onPrefetch={() => {}}
          onCancelPrefetch={() => {}}
          presenceSet={new Set()}
          now={1_800_000_000_000}
        />
      </ul>
    </MemoryRouter>,
  );
}

afterEach(cleanup);

describe("SessionListRow (worklog)", () => {
  it("renders title, outcome line and one facts line", () => {
    renderRow(mk({ display_name: "Build the widget", outcome: "shipped it" }));
    expect(screen.getByText("Build the widget").className).not.toContain("row-name--prompt");
    expect(screen.getByText(/shipped it/)).toBeTruthy();
    const facts = document.querySelectorAll(".kb-ses__row-stats");
    expect(facts.length).toBe(1);
    expect(facts[0].textContent).toBe("widget · claude · 1h · 2 memories");
  });

  it("truncates a first-prompt title at 80 chars, regular weight, and does not repeat it as line 2", () => {
    const prompt = "p".repeat(200);
    renderRow(mk({ first_user_prompt: prompt }));
    const name = document.querySelector(".kb-ses__row-name") as HTMLElement;
    expect(name.className).toContain("kb-ses__row-name--prompt");
    expect(name.textContent!.length).toBe(80);
    expect(document.querySelector(".kb-ses__row-preview")).toBeNull();
  });

  // OL IDEA3 — the evidence verdict, in words, after the facts.
  const evidence = (over: Partial<SessionEvidenceOut>): SessionEvidenceOut => ({
    verdict: "verified",
    phrase: "cargo test passed after the last edit",
    deciding: {
      label: "cargo test",
      cmd: "cargo test -p widget",
      runners: ["cargo-test"],
      outcome: "pass",
      text: "passed",
    },
    checks_total: 1,
    checks_after_edit: 1,
    edits_ignored: 0,
    uncommitted: false,
    ...over,
  });

  it("appends the verdict phrase to the facts line, its title the deciding command", () => {
    renderRow(mk({ display_name: "Build the widget", evidence: evidence({}) }));
    const facts = document.querySelectorAll(".kb-ses__row-stats");
    expect(facts.length).toBe(1);
    expect(facts[0].textContent).toBe(
      "widget · claude · 1h · 2 memories · cargo test passed after the last edit",
    );
    const span = screen.getByTestId("session-row-evidence");
    expect(span.getAttribute("data-verdict")).toBe("verified");
    expect(span.getAttribute("title")).toBe("$ cargo test -p widget");
    // Words only: no glyph and no verdict-coloured class on the row.
    expect(span.className).toBe("");
  });

  it("spells every row verdict and nothing for the quiet ones", () => {
    for (const verdict of ["failing", "unconfirmed", "stale", "unchecked"] as const) {
      cleanup();
      renderRow(mk({ evidence: evidence({ verdict, phrase: `phrase for ${verdict}` }) }));
      expect(screen.getByTestId("session-row-evidence").textContent).toBe(`phrase for ${verdict}`);
    }
    for (const ev of [
      evidence({ verdict: "no_edits", phrase: undefined, deciding: undefined }),
      evidence({ verdict: "not_extracted", phrase: undefined, deciding: undefined }),
      undefined,
    ]) {
      cleanup();
      renderRow(mk({ evidence: ev }));
      expect(screen.queryByTestId("session-row-evidence")).toBeNull();
      expect(document.querySelector(".kb-ses__row-stats")!.textContent).toBe(
        "widget · claude · 1h · 2 memories",
      );
    }
  });

  it("shows the phrase alone when the facts line is empty", () => {
    renderRow(
      mk({
        folder: undefined,
        harness: "",
        duration_ms: 0,
        memory_count: 0,
        evidence: evidence({ verdict: "unchecked", phrase: "edited, no checks ran", deciding: undefined }),
      }),
    );
    const stats = document.querySelector(".kb-ses__row-stats")!;
    expect(stats.textContent).toBe("edited, no checks ran");
    expect(screen.getByTestId("session-row-evidence").getAttribute("title")).toBeNull();
  });
});
