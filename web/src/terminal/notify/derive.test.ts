import { describe, expect, it } from "vitest";
import { deriveAttention, type AttnRow } from "./derive";

const row = (termId: string, p: Partial<AttnRow> = {}): AttnRow => ({
  termId,
  sid: `s-${termId}`,
  state: "working",
  blocked: false,
  ended: false,
  title: `T-${termId}`,
  project: "proj",
  ...p,
});
const kinds = (r: ReturnType<typeof deriveAttention>) => r.events.map((e) => e.kind);

describe("deriveAttention", () => {
  it("first snapshot (null baseline) emits nothing, even for blocked rows", () => {
    expect(deriveAttention(null, [row("a", { blocked: true })], null).events).toEqual([]);
  });
  it("became blocked -> one asks", () => {
    const r = deriveAttention([row("a")], [row("a", { blocked: true })], null);
    expect(r.events).toEqual([{ kind: "asks", termId: "a", title: "T-a", project: "proj" }]);
  });
  it("blocked in both snapshots -> no duplicate", () => {
    const b = row("a", { blocked: true });
    expect(deriveAttention([b], [b], null).events).toEqual([]);
  });
  it("working -> waiting -> one finished; stalled -> finished too", () => {
    expect(kinds(deriveAttention([row("a")], [row("a", { state: "waiting" })], null))).toEqual(["finished"]);
    expect(
      kinds(deriveAttention([row("a", { state: "stalled" })], [row("a", { state: "finished" })], null)),
    ).toEqual(["finished"]);
  });
  it("the viewed terminal never notifies", () => {
    const r = deriveAttention(
      [row("a"), row("b")],
      [row("a", { blocked: true }), row("b", { state: "waiting" })],
      "a",
    );
    expect(r.events.map((e) => (e.kind === "summary" ? "" : e.termId))).toEqual(["b"]);
  });
  it("ended rows emit nothing", () => {
    expect(deriveAttention([row("a")], [row("a", { blocked: true, ended: true })], null).events).toEqual([]);
    expect(
      deriveAttention([row("a", { ended: true })], [row("a", { blocked: true })], null).events,
    ).toEqual([]);
  });
  it("a row that appears already waiting or blocked is baseline only", () => {
    expect(deriveAttention([], [row("a", { state: "waiting" }), row("b", { blocked: true })], null).events).toEqual([]);
  });
  it("unbound (sid null) rows emit nothing", () => {
    expect(deriveAttention([row("a", { sid: null })], [row("a", { sid: null, blocked: true })], null).events).toEqual([]);
    expect(deriveAttention([row("a", { state: null })], [row("a", { state: "waiting" })], null).events).toEqual([]);
  });
  it("a reset baseline after events emits nothing", () => {
    const next = [row("a", { blocked: true })];
    expect(deriveAttention([row("a")], next, null).events).toHaveLength(1);
    expect(deriveAttention(null, next, null).events).toEqual([]);
  });
  it("blocked -> waiting without a working state emits nothing", () => {
    const r = deriveAttention([row("a", { blocked: true, state: "waiting" })], [row("a", { state: "waiting" })], null);
    expect(r.events).toEqual([]);
  });
  it("exactly 3 stay individual, asks first then by id", () => {
    const prev = [row("c"), row("a"), row("b")];
    const next = [row("c", { blocked: true }), row("a", { state: "waiting" }), row("b", { state: "waiting" })];
    const r = deriveAttention(prev, next, null);
    expect(r.events.map((e) => (e.kind === "summary" ? "s" : `${e.kind}:${e.termId}`))).toEqual([
      "asks:c",
      "finished:a",
      "finished:b",
    ]);
  });
  it("4 events coalesce into one summary with counts", () => {
    const ids = ["a", "b", "c", "d"];
    const prev = ids.map((i) => row(i));
    const next = ids.map((i) => (i < "c" ? row(i, { blocked: true }) : row(i, { state: "waiting" })));
    expect(deriveAttention(prev, next, null).events).toEqual([
      { kind: "summary", count: 4, asks: 2, finished: 2 },
    ]);
  });
});
