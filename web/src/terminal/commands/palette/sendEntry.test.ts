import { beforeEach, describe, expect, it, vi } from "vitest";

const sendText = vi.hoisted(() => vi.fn());
const sendKey = vi.hoisted(() => vi.fn());
const pressEnter = vi.hoisted(() => vi.fn());
vi.mock("../../phone/driveApi", async () => {
  const actual = await vi.importActual<typeof import("../../phone/driveApi")>("../../phone/driveApi");
  return { ...actual, sendText, sendKey, pressEnter };
});

import { DriveError, type Drive } from "../../phone/driveApi";
import { sendEntry } from "./sendEntry";

const d: Drive = { termId: "t1", harness: "claude", viewer: null, confirm: async () => true };

beforeEach(() => {
  sendText.mockReset().mockResolvedValue({ kind: "sent" });
  sendKey.mockReset().mockResolvedValue({ kind: "sent" });
  pressEnter.mockReset();
});

describe("sendEntry", () => {
  it("paste-mode is one sendText with enter:false and never an Enter call", async () => {
    const r = await sendEntry(d, { kind: "text", text: "/compact", enter: false, unverified: false });
    expect(r).toEqual({ kind: "sent" });
    expect(sendText).toHaveBeenCalledTimes(1);
    expect(sendText).toHaveBeenCalledWith(d, "/compact", { enter: false });
    expect(pressEnter).not.toHaveBeenCalled();
    expect(sendKey).not.toHaveBeenCalled();
  });

  it("flattens a multi-line paste-mode text, never a run-mode one", async () => {
    await sendEntry(d, { kind: "text", text: "a\nb", enter: false, unverified: false });
    expect(sendText).toHaveBeenLastCalledWith(d, "a b", { enter: false });
    await sendEntry(d, { kind: "text", text: "a b", enter: true, unverified: false });
    expect(sendText).toHaveBeenLastCalledWith(d, "a b", { enter: true });
  });

  it("run-mode asks for the Enter in the same request", async () => {
    await sendEntry(d, { kind: "text", text: "/model", enter: true, unverified: false });
    expect(sendText).toHaveBeenCalledWith(d, "/model", { enter: true });
  });

  it("pasted-not-sent is a refusal carrying the server detail", async () => {
    sendText.mockResolvedValue({ kind: "pasted-not-sent", detail: "waiting on you", conflict: true });
    expect(await sendEntry(d, { kind: "text", text: "/model", enter: true, unverified: false })).toEqual({
      kind: "refused",
      detail: "waiting on you",
    });
  });

  it("a declined guard sends nothing more", async () => {
    sendText.mockResolvedValue({ kind: "declined" });
    expect(await sendEntry(d, { kind: "text", text: "x", enter: false, unverified: false })).toEqual({ kind: "declined" });
  });

  it("a DriveError becomes an error with its detail", async () => {
    sendText.mockRejectedValue(new DriveError(422, null, "multi-line-refused"));
    expect(await sendEntry(d, { kind: "text", text: "x", enter: false, unverified: false })).toEqual({
      kind: "error",
      detail: "multi-line-refused",
    });
  });

  it("a chord sends each key in order and stops at the first failure", async () => {
    await sendEntry(d, { kind: "keys", keys: ["Esc", "Esc"], unverified: false });
    expect(sendKey.mock.calls.map((c) => c[1])).toEqual(["Esc", "Esc"]);
    sendKey.mockReset().mockRejectedValueOnce(new DriveError(409, null, "blocked"));
    const r = await sendEntry(d, { kind: "keys", keys: ["Esc", "Tab", "Enter"], unverified: false });
    expect(r).toEqual({ kind: "error", detail: "blocked" });
    expect(sendKey).toHaveBeenCalledTimes(1);
  });

  it("incomplete and invalid entries send nothing", async () => {
    expect((await sendEntry(d, { kind: "incomplete", missing: ["path"] })).kind).toBe("error");
    expect((await sendEntry(d, { kind: "invalid", reason: "bad-item" })).kind).toBe("error");
    expect((await sendEntry(d, { kind: "invalid", reason: "arg-control-char" })).kind).toBe("error");
    expect(sendText).not.toHaveBeenCalled();
    expect(sendKey).not.toHaveBeenCalled();
  });
});
