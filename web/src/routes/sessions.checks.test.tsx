// @vitest-environment jsdom
//
// OL IDEA3 — the Checks section of the session inspector.
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import { ChecksSection } from "./sessions";
import type { SessionCheckOut, SessionDetail, SessionEvidenceOut, SessionRow } from "../api/sessions";

const check = (over: Partial<SessionCheckOut>): SessionCheckOut => ({
  label: "cargo test",
  cmd: "cargo test -p widget 2>&1 | tail -5",
  runners: ["cargo-test"],
  outcome: "pass",
  text: "passed (read from its output)",
  after_last_edit: false,
  ...over,
});

const evidence = (over: Partial<SessionEvidenceOut>): SessionEvidenceOut => ({
  verdict: "verified",
  phrase: "cargo clippy passed after the last edit",
  deciding: check({ label: "cargo clippy", cmd: "cargo clippy -p widget", text: "passed" }),
  checks_total: 2,
  checks_after_edit: 1,
  edits_ignored: 0,
  uncommitted: false,
  ...over,
});

function row(over: Partial<SessionRow>): SessionRow {
  return { session_id: "s", kb: "kb", ...over } as SessionRow;
}

function detail(over: Partial<SessionDetail>): SessionDetail {
  return { checks: [], unnamed_edits: [], unnamed_edits_total: 0, ...over } as unknown as SessionDetail;
}

afterEach(cleanup);

describe("ChecksSection", () => {
  it("lists the phrase, each check with its command and when it ran, and the caption", () => {
    const ev = evidence({});
    render(
      <ChecksSection
        row={row({ evidence: ev })}
        detail={detail({
          evidence: ev,
          checks: [
            check({}),
            check({
              label: "cargo clippy",
              cmd: "cargo clippy -p widget",
              text: "passed (background run)",
              after_last_edit: true,
            }),
          ],
        })}
      />,
    );
    const sec = screen.getByTestId("session-checks");
    expect(within(sec).getByTestId("session-checks-phrase").textContent).toBe(
      "cargo clippy passed after the last edit",
    );
    const items = within(sec).getAllByTestId("session-check");
    expect(items.length).toBe(2);
    expect(items[0].getAttribute("data-outcome")).toBe("pass");
    expect(items[0].textContent).toContain("passed (read from its output)");
    expect(items[0].textContent).toContain("before the last edit");
    expect(items[0].querySelector("code")!.textContent).toBe("cargo test -p widget 2>&1 | tail -5");
    expect(items[1].textContent).toContain("passed (background run)");
    expect(items[1].textContent).toContain("after the last edit");
    expect(within(sec).getByTestId("session-checks-caption").textContent).toBe(
      "From this session's main transcript: edits are Edit/Write tool calls (edits made through Bash or by subagents are not seen); checks are a fixed list of test, lint and build commands.",
    );
    // Nothing optional leaks in.
    expect(within(sec).queryByTestId("session-checks-not-named")).toBeNull();
    expect(within(sec).queryByTestId("session-checks-uncommitted")).toBeNull();
    expect(within(sec).queryByTestId("session-checks-segment")).toBeNull();
  });

  it("names unnamed edits, a missing commit, and the ignored-edit count", () => {
    const ev = evidence({ uncommitted: true, edits_ignored: 2 });
    render(
      <ChecksSection
        row={row({ evidence: ev })}
        detail={detail({
          evidence: ev,
          unnamed_edits: ["render.rs"],
          unnamed_edits_total: 3,
        })}
      />,
    );
    expect(screen.getByTestId("session-checks-not-named").textContent).toBe(
      "Not named in the closing text: render.rs (+2 more)",
    );
    expect(screen.getByTestId("session-checks-uncommitted").textContent).toBe(
      "No commit detected in this session.",
    );
    expect(screen.getByTestId("session-checks-caption").textContent).toMatch(
      / · 2 edits to agent or scratch files not counted$/,
    );
  });

  it("says a segmented part's evidence covers that part only", () => {
    const ev = evidence({});
    render(
      <ChecksSection
        row={row({ evidence: ev, segment_idx: 2, segment_count: 3 })}
        detail={detail({ evidence: ev })}
      />,
    );
    expect(screen.getByTestId("session-checks-segment").textContent).toBe(
      "part 2 of 3: this part's checks and edits only",
    );
  });

  it("is honest about a capture that predates the evidence", () => {
    const ev = evidence({ verdict: "not_extracted", phrase: undefined, deciding: undefined });
    render(<ChecksSection row={row({ evidence: ev })} detail={detail({ evidence: ev })} />);
    expect(screen.getByTestId("session-checks-phrase").textContent).toMatch(
      /^Not extracted for this capture/,
    );
    expect(screen.queryAllByTestId("session-check").length).toBe(0);
  });

  it("says there was nothing to check when no edit was seen", () => {
    const ev = evidence({ verdict: "no_edits", phrase: undefined, deciding: undefined });
    render(<ChecksSection row={row({ evidence: ev })} detail={null} />);
    expect(screen.getByTestId("session-checks-phrase").textContent).toMatch(/No edits seen/);
  });

  it("renders nothing for a daemon that sends no evidence", () => {
    const { container } = render(<ChecksSection row={row({})} detail={null} />);
    expect(container.innerHTML).toBe("");
  });
});
