// @vitest-environment jsdom
// T18 — the choice bar: only while the bound session is blocked, state from the
// shared live-sessions query alone, answers sent as key NAMES.

import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LiveStatusRow } from "../../api/sessions";
import { ChoiceBar } from "./ChoiceBar";
import { byId, maybeById, mkDrive, mount, press, rigFetch, settle, type FetchRig, type Mounted } from "./driveTestkit";

let rows: Array<Partial<LiveStatusRow>> = [];

vi.mock("../../hooks/useLiveSessions", () => ({
  useLiveSessions: () => ({ rows, loading: false }),
}));

let m: Mounted | null = null;
let rig: FetchRig;

beforeEach(() => {
  rig = rigFetch();
  rows = [];
});

afterEach(() => {
  m?.unmount();
  m = null;
  vi.unstubAllGlobals();
});

const show = (over: Partial<ReturnType<typeof mkDrive>> = {}): Mounted => {
  m = mount(<ChoiceBar drive={mkDrive(over)} />);
  return m;
};

describe("ChoiceBar", () => {
  it("renders nothing, and takes no layout, when no row is bound to the terminal", () => {
    rows = [{ term: "other999", blocked: true }];
    show();
    expect(m!.host.innerHTML).toBe("");
  });

  it("renders nothing while the bound session is not blocked", () => {
    rows = [{ term: "abcd1234", blocked: false }];
    show();
    expect(m!.host.innerHTML).toBe("");
  });

  it("shows the caption and buttons 1-4 while blocked, and issues no request of its own", () => {
    rows = [{ term: "abcd1234", blocked: true, blocked_tool: "Bash" }];
    show();
    expect(byId(m!.host, "kb-drive-choice-caption").textContent).toBe("Waiting on you: Bash");
    for (const k of ["1", "2", "3", "4"]) expect(maybeById(m!.host, `kb-drive-choice-${k}`)).not.toBeNull();
    expect(maybeById(m!.host, "kb-drive-choice-5")).toBeNull();
    expect(rig.calls).toHaveLength(0);
  });

  it("names what it is waiting on: tool, else reason, else a prompt", () => {
    rows = [{ term: "abcd1234", blocked: true, detail_reason: "plan approval" }];
    show();
    expect(byId(m!.host, "kb-drive-choice-caption").textContent).toBe("Waiting on you: plan approval");
    m!.unmount();
    rows = [{ term: "abcd1234", blocked: true }];
    show();
    expect(byId(m!.host, "kb-drive-choice-caption").textContent).toBe("Waiting on you: a prompt");
  });

  it("discloses 5-9 behind a button", () => {
    rows = [{ term: "abcd1234", blocked: true }];
    show();
    press(byId(m!.host, "kb-drive-choice-more"));
    for (const k of ["5", "6", "7", "8", "9"]) expect(maybeById(m!.host, `kb-drive-choice-${k}`)).not.toBeNull();
  });

  it("each button sends exactly its key name, with the viewer", async () => {
    rows = [{ term: "abcd1234", blocked: true }];
    show();
    press(byId(m!.host, "kb-drive-choice-more"));
    const names = ["1", "2", "3", "4", "5", "9", "y", "n", "Enter", "Esc", "Up", "Down"];
    for (const k of names) press(byId(m!.host, `kb-drive-choice-${k}`));
    await settle();
    expect(rig.urls().every((u) => u === "/api/terms/abcd1234/keys")).toBe(true);
    expect(rig.calls.map((c) => c.json?.keys)).toEqual(names.map((k) => [k]));
    for (const c of rig.calls) expect(c.json?.viewer).toBe("ws-abc1234567");
  });

  it("answers Enter with the key, not the enter op (the server refuses that one while blocked)", async () => {
    rows = [{ term: "abcd1234", blocked: true }];
    show();
    press(byId(m!.host, "kb-drive-choice-Enter"));
    await settle();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/keys"]);
  });

  it("disappears when the row stops being blocked (the live query flipped)", () => {
    rows = [{ term: "abcd1234", blocked: true }];
    show();
    expect(maybeById(m!.host, "kb-drive-choicebar")).not.toBeNull();
    rows = [{ term: "abcd1234", blocked: false }];
    act(() => m!.root.render(<ChoiceBar drive={mkDrive()} />));
    expect(m!.host.innerHTML).toBe("");
  });

  it("never takes focus on press", () => {
    rows = [{ term: "abcd1234", blocked: true }];
    show();
    const ev = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    byId(m!.host, "kb-drive-choice-1").dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
    expect(byId(m!.host, "kb-drive-choice-1").tabIndex).toBe(-1);
  });
});
