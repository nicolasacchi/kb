import { describe, expect, it } from "vitest";
import {
  formatSessionTurn,
  replayUrl,
  sessionsProjectsHomeUrl,
  sessionsProjectUrl,
  sessionsUrl,
  sessionsWorklogUrl,
} from "./sessionsUrl";

describe("sessionsUrl", () => {
  it("bare call is the unfiltered list landing", () => {
    expect(sessionsUrl()).toBe("/sessions");
    expect(sessionsUrl({})).toBe("/sessions");
  });

  it("omits view=list (the default) but includes other views", () => {
    expect(sessionsUrl({ view: "list" })).toBe("/sessions");
    expect(sessionsUrl({ view: "projects" })).toBe("/sessions?view=projects");
    expect(sessionsUrl({ view: "threads" })).toBe("/sessions?view=threads");
  });

  // R12 — this builder ONLY ever emits `project=`, never `folder=`.
  it("emits project=, never folder=", () => {
    expect(sessionsUrl({ project: "kb" })).toBe("/sessions?project=kb");
  });

  it("joins q, substance csv (order-preserved), and focus", () => {
    expect(sessionsUrl({ q: "reindex bug" })).toBe(
      "/sessions?q=reindex+bug",
    );
    expect(sessionsUrl({ substance: ["routine", "substantive"] })).toBe(
      "/sessions?substance=routine%2Csubstantive",
    );
    expect(sessionsUrl({ substance: [] })).toBe("/sessions");
    expect(sessionsUrl({ focus: "sid-123" })).toBe("/sessions?focus=sid-123");
  });

  it("composes every axis in a stable order", () => {
    expect(
      sessionsUrl({
        view: "list",
        project: "kb",
        q: "bug",
        substance: ["trivial"],
        focus: "sid-1",
      }),
    ).toBe("/sessions?project=kb&q=bug&substance=trivial&focus=sid-1");
  });

  it("sessionsProjectUrl pins the project and merges extras", () => {
    expect(sessionsProjectUrl("kb")).toBe("/sessions?project=kb");
    expect(sessionsProjectUrl("kb", { q: "x" })).toBe(
      "/sessions?project=kb&q=x",
    );
  });

  it("sessionsProjectsHomeUrl is the projects view with no other axes", () => {
    expect(sessionsProjectsHomeUrl()).toBe("/sessions?view=projects");
  });

  it("sessionsWorklogUrl focuses a session, optionally scoped to a project", () => {
    expect(sessionsWorklogUrl("sid-1")).toBe("/sessions?focus=sid-1");
    expect(sessionsWorklogUrl("sid-1", "kb")).toBe(
      "/sessions?project=kb&focus=sid-1",
    );
    expect(sessionsWorklogUrl("sid-1", null)).toBe("/sessions?focus=sid-1");
  });

  it("replayUrl encodes kb and session id", () => {
    expect(replayUrl("kb", "sid-1")).toBe("/replay/kb/sid-1");
    expect(replayUrl("a b", "s/1")).toBe("/replay/a%20b/s%2F1");
  });

  it("formatSessionTurn stringifies N or passes through 'end'", () => {
    expect(formatSessionTurn(12)).toBe("12");
    expect(formatSessionTurn("end")).toBe("end");
  });
});
