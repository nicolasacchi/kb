// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";

const state = vi.hoisted(() => ({
  admitted: true,
  terms: [] as unknown[],
  live: [] as unknown[],
  isLoading: false,
  isError: false,
  liveLoading: false,
}));
vi.mock("../../hooks/useArtifactHost", () => ({
  useIdentity: () => (state.admitted ? { terminal_admitted: true } : { terminal_admitted: false }),
}));
vi.mock("../useTerms", () => ({
  useTerms: () => ({ terms: state.terms, isLoading: state.isLoading, isError: state.isError }),
}));
vi.mock("../../hooks/useLiveSessions", () => ({
  useLiveSessions: () => ({ rows: state.live, loading: state.liveLoading }),
}));
const toastInfo = vi.hoisted(() => vi.fn());
vi.mock("../../lib/toast", () => ({ toast: { info: toastInfo, ok: vi.fn(), err: vi.fn() } }));

import NotifyHost from "./NotifyHost";
import { peekMarks, __resetMarks } from "./attention";
import { NOTIFY_KEY } from "./notifyPref";

const term = (id: string, p: Record<string, unknown> = {}) => ({
  id,
  harness: "claude",
  cwd: "/w/proj",
  started_unix: 1,
  pane_dead: false,
  session_id: `s-${id}`,
  ...p,
});
const lv = (id: string, p: Record<string, unknown> = {}) => ({
  session_id: `s-${id}`,
  state: "working",
  blocked: false,
  title: `Title ${id}`,
  ...p,
});

let loc = "";
function Probe() {
  loc = useLocation().pathname;
  return null;
}
function ui(path: string) {
  return (
    <MemoryRouter initialEntries={[path]}>
      <NotifyHost />
      <Probe />
    </MemoryRouter>
  );
}
function setHidden(v: boolean) {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => v });
}

beforeEach(() => {
  state.admitted = true;
  state.terms = [term("a"), term("b")];
  state.live = [lv("a"), lv("b")];
  state.isLoading = state.isError = state.liveLoading = false;
  toastInfo.mockReset();
  __resetMarks();
  setHidden(false);
  localStorage.clear();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function change(r: ReturnType<typeof render>, path: string, live: unknown[], terms?: unknown[]) {
  state.live = live;
  if (terms) state.terms = terms;
  act(() => r.rerender(ui(path)));
}

describe("NotifyHost", () => {
  it("no toast on first render, even when something is already blocked", () => {
    state.live = [lv("a"), lv("b", { blocked: true })];
    render(ui("/terminals/a"));
    expect(toastInfo).not.toHaveBeenCalled();
  });

  it("asks toast has exact wording, a Go action and navigates by the hub builder", () => {
    const r = render(ui("/terminals/a"));
    change(r, "/terminals/a", [lv("a"), lv("b", { blocked: true, last_line: "SECRET-LINE", blocked_tool: "SECRET-TOOL" })]);
    expect(toastInfo).toHaveBeenCalledTimes(1);
    const [msg, action] = toastInfo.mock.calls[0];
    expect(msg).toBe("Title b (proj) needs you");
    expect(action.label).toBe("Go");
    act(() => action.onClick());
    expect(loc).toBe("/terminals/b");
    expect(peekMarks().b).toEqual({ asks: true, finishedUnseen: false });
    expect(JSON.stringify(toastInfo.mock.calls)).not.toMatch(/SECRET|\/w\//);
  });

  it("finished toast has distinct wording and sets a finished-unseen mark", () => {
    const r = render(ui("/terminals/a"));
    change(r, "/terminals/a", [lv("a"), lv("b", { state: "waiting" })]);
    expect(toastInfo.mock.calls[0][0]).toBe("Title b (proj) finished");
    expect(peekMarks().b).toEqual({ asks: false, finishedUnseen: true });
    // viewing b clears the unseen mark
    change(r, "/terminals/b", [lv("a"), lv("b", { state: "waiting" })]);
    expect(peekMarks().b).toBeUndefined();
  });

  it("summary toast for more than 3 events leads to the hub", () => {
    const ids = ["a", "b", "c", "d", "e"];
    state.terms = ids.map((i) => term(i));
    state.live = ids.map((i) => lv(i));
    const r = render(ui("/terminals/a"));
    change(r, "/terminals/a", ids.map((i) => lv(i, { blocked: i !== "a" })));
    expect(toastInfo).toHaveBeenCalledTimes(1);
    expect(toastInfo.mock.calls[0][0]).toBe("4 terminals need attention");
    act(() => toastInfo.mock.calls[0][1].onClick());
    expect(loc).toBe("/terminals");
  });

  it("nothing when not admitted", () => {
    state.admitted = false;
    const r = render(ui("/"));
    change(r, "/", [lv("a"), lv("b", { blocked: true })]);
    expect(toastInfo).not.toHaveBeenCalled();
  });

  it("the viewed terminal is silent, unless the tab is hidden", () => {
    const r = render(ui("/terminals/b"));
    change(r, "/terminals/b", [lv("a"), lv("b", { blocked: true })]);
    expect(toastInfo).not.toHaveBeenCalled();
    setHidden(true);
    change(r, "/terminals/b", [lv("a"), lv("b", { blocked: false }), ]);
    change(r, "/terminals/b", [lv("a"), lv("b", { blocked: true })]);
    expect(toastInfo).toHaveBeenCalledTimes(1);
  });

  it("a transient error resets the baseline: recovery causes no storm", () => {
    const r = render(ui("/terminals/a"));
    state.isError = true;
    change(r, "/terminals/a", [lv("a"), lv("b")]);
    state.isError = false;
    change(r, "/terminals/a", [lv("a"), lv("b", { blocked: true })]);
    expect(toastInfo).not.toHaveBeenCalled();
  });

  it("ended terminals never notify", () => {
    const r = render(ui("/terminals/a"));
    change(r, "/terminals/a", [lv("a"), lv("b", { blocked: true })], [term("a"), term("b", { pane_dead: true })]);
    expect(toastInfo).not.toHaveBeenCalled();
  });
});

describe("NotifyHost browser Notification", () => {
  class Fake {
    static made: { title: string; opts: { body: string; tag: string } }[] = [];
    static permission = "granted";
    static requestPermission = vi.fn();
    onclick: (() => void) | null = null;
    constructor(public title: string, public opts: { body: string; tag: string }) {
      Fake.made.push({ title, opts });
    }
    close() {}
  }
  beforeEach(() => {
    Fake.made = [];
    Fake.permission = "granted";
    Fake.requestPermission.mockReset();
    vi.stubGlobal("Notification", Fake);
  });
  const fire = () => {
    const r = render(ui("/terminals/a"));
    change(r, "/terminals/a", [lv("a"), lv("b", { blocked: true, last_line: "SECRET" })]);
  };

  it("pref off -> never", () => {
    setHidden(true);
    fire();
    expect(Fake.made).toHaveLength(0);
    expect(toastInfo).toHaveBeenCalledTimes(1);
  });
  it("pref on but visible -> never", () => {
    localStorage.setItem(NOTIFY_KEY, "1");
    fire();
    expect(Fake.made).toHaveLength(0);
  });
  it("pref on + hidden + granted -> exactly one, tagged, without secrets", () => {
    localStorage.setItem(NOTIFY_KEY, "1");
    setHidden(true);
    fire();
    expect(Fake.made).toHaveLength(1);
    expect(Fake.made[0].opts.tag).toBe("kb-term-b");
    expect(Fake.made[0].opts.body).toBe("Title b (proj) needs you");
    expect(JSON.stringify(Fake.made)).not.toContain("SECRET");
  });
  it("hidden + default/denied -> never, and the host never requests permission", () => {
    localStorage.setItem(NOTIFY_KEY, "1");
    setHidden(true);
    for (const p of ["default", "denied"]) {
      Fake.permission = p;
      fire();
      cleanup();
      toastInfo.mockReset();
      __resetMarks();
    }
    expect(Fake.made).toHaveLength(0);
    expect(Fake.requestPermission).not.toHaveBeenCalled();
  });
});
