// @vitest-environment jsdom
//
// AH3 — the inspector shows the server's per-harness `resume`, never a
// client-built `claude -r`.
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { ResumeCommand } from "./sessions";

afterEach(cleanup);

describe("ResumeCommand", () => {
  it("renders the server resume string for a non-claude harness", () => {
    const { container } = render(<ResumeCommand sessionId="abc" resume="omp -r abc" />);
    expect(container.querySelector(".kb-ses__resume-cmd-text")?.textContent).toBe("omp -r abc");
    expect(container.textContent).not.toContain("claude -r");
    expect(screen.getByTestId("session-download-bundle")).toBeTruthy();
  });

  it("without a resume string shows no command but keeps the bundle button", () => {
    render(<ResumeCommand sessionId="abc" resume="" />);
    expect(screen.queryByTestId("session-resume-cmd-copy")).toBeNull();
    expect(screen.getByTestId("session-download-bundle")).toBeTruthy();
  });
});
