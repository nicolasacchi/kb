// @vitest-environment jsdom
//
// HUBHOST — the Continue-in-terminal door: a link to the hub's launcher,
// visible only for an admitted caller, a resumable session and no live bound
// terminal. It never POSTs.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";

const m = vi.hoisted(() => ({ identity: { terminal_admitted: true } as { terminal_admitted: boolean } | null }));
vi.mock("../hooks/useArtifactHost", () => ({ useIdentity: () => m.identity }));

import { ContinueInTerminal } from "./ContinueInTerminal";
import { ResumeInTerminal } from "./ResumeInTerminal";

let fetchSpy: ReturnType<typeof vi.fn>;
beforeEach(() => {
  m.identity = { terminal_admitted: true };
  fetchSpy = vi.fn(async () => new Response("{}", { status: 200 }));
  vi.stubGlobal("fetch", fetchSpy);
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

type P = React.ComponentProps<typeof ContinueInTerminal>;
function door(over: Partial<P> = {}) {
  const props: P = { sessionId: "sid-1", harness: "claude", cwd: "/srv/w", ...over };
  return render(
    <MemoryRouter>
      <ContinueInTerminal {...props} />
    </MemoryRouter>,
  );
}
const link = () => screen.queryByTestId("continue-in-terminal");

describe("visibility matrix", () => {
  it.each([
    ["claude", true],
    ["codex", true],
    ["omp", true],
    ["opencode", true],
    ["grok", true],
    ["kimi", false],
  ])("harness %s -> %s", (h, shown) => {
    door({ harness: h });
    expect(link() !== null).toBe(shown);
  });

  it.each([
    ["cwd ok", "/srv/w", true],
    ["cwd missing", null, false],
    ["cwd relative", "relative/dir", false],
  ])("%s -> %s", (_n, cwd, shown) => {
    door({ cwd });
    expect(link() !== null).toBe(shown);
  });

  it("a session id that is not a plain token hides the door", () => {
    door({ sessionId: "-bad" });
    expect(link()).toBeNull();
    cleanup();
    door({ sessionId: "a/b" });
    expect(link()).toBeNull();
  });

  it("not admitted, or identity not loaded -> hidden", () => {
    m.identity = { terminal_admitted: false };
    door();
    expect(link()).toBeNull();
    cleanup();
    m.identity = null;
    door();
    expect(link()).toBeNull();
  });

  it("a live bound terminal hides it", () => {
    door({ hasLiveTerminal: true });
    expect(link()).toBeNull();
  });
});

describe("the link", () => {
  it("points at the exact deep link and sends nothing on click", () => {
    door({ sessionId: "abc.1_2-3" });
    const a = link()!;
    expect(a.getAttribute("href")).toBe("/terminals?launch=1&resume=abc.1_2-3");
    fireEvent.click(a);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("carries no harness or cwd in the URL", () => {
    door({ harness: "codex", cwd: "/srv/secret" });
    const href = link()!.getAttribute("href")!;
    expect(href).not.toContain("harness");
    expect(href).not.toContain("cwd");
    expect(href).not.toContain("secret");
  });
});

describe("ResumeInTerminal (session page)", () => {
  const page = (over: Partial<React.ComponentProps<typeof ResumeInTerminal>> = {}) =>
    render(
      <MemoryRouter>
        <ResumeInTerminal sessionId="sid-1" harness="claude" cwd="/srv/w" hasLiveTerminal={false} {...over} />
      </MemoryRouter>,
    );
  it("renders the door inside the Resume section and never posts", () => {
    page();
    expect(screen.getByTestId("session-resume-term")).toBeTruthy();
    fireEvent.click(screen.getByTestId("continue-in-terminal"));
    expect(screen.queryByTestId("session-resume-term-go")).toBeNull();
    expect(fetchSpy).not.toHaveBeenCalled();
  });
  it("renders nothing when the door is hidden", () => {
    page({ hasLiveTerminal: true });
    expect(screen.queryByTestId("session-resume-term")).toBeNull();
  });
});
