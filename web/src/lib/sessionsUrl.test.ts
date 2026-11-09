import { describe, expect, it } from "vitest";
import {
  formatSessionTurn,
  replayUrl,
  SESSIONS_SCROLL_EPHEMERAL,
  sessionsProjectsHomeUrl,
  sessionsProjectUrl,
  sessionsUrl,
  sessionsWorklogUrl,
  sessionsTreeFromSearch,
} from "./sessionsUrl";
import { normalizeScrollKey } from "../hooks/useScrollRestoration";

describe("sessionsUrl", () => {
  it("bare call is the unfiltered list landing", () => {
    expect(sessionsUrl()).toBe("/sessions");
    expect(sessionsUrl({})).toBe("/sessions");
  });

  it("omits view=list (the default) but includes other views", () => {
    expect(sessionsUrl({ view: "list" })).toBe("/sessions");
    expect(sessionsUrl({ view: "projects" })).toBe("/sessions?view=projects");
    expect(sessionsUrl({ view: "threads" })).toBe("/sessions?view=threads");
    expect(sessionsUrl({ view: "work" })).toBe("/sessions?view=work");
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

  // A4 — `tree=flat` shows subagent children; absent means collapsed.
  it("tree=flat is emitted only for flat; collapsed omits the param", () => {
    expect(sessionsUrl({ tree: "flat" })).toBe("/sessions?tree=flat");
    expect(sessionsUrl({ tree: "collapsed" })).toBe("/sessions");
    expect(sessionsUrl({})).toBe("/sessions");
  });

  it("sessionsTreeFromSearch round-trips flat and ignores junk", () => {
    expect(sessionsTreeFromSearch("?tree=flat")).toBe("flat");
    expect(sessionsTreeFromSearch("")).toBe("collapsed");
    expect(sessionsTreeFromSearch("?tree=collapsed")).toBe("collapsed");
    expect(sessionsTreeFromSearch("?tree=banana")).toBe("collapsed");
    expect(sessionsTreeFromSearch(new URLSearchParams({ tree: "flat" }))).toBe("flat");
    // Round-trip: what the builder writes, the parser reads back.
    expect(sessionsTreeFromSearch(sessionsUrl({ tree: "flat" }).split("?")[1] ?? "")).toBe("flat");
  });
});

// A4/FXA4 — the scroll-restoration key must DIFFER between flat and
// collapsed (different lists), while `focus`/`follow` stay ephemeral.
// Pins against the route's REAL ephemeral list (SESSIONS_SCROLL_EPHEMERAL,
// imported by routes/sessions.tsx): adding "tree" there — exactly what
// brief A4 §4 forbids — would fail the second test. MUTATION: appending
// "tree" to SESSIONS_SCROLL_EPHEMERAL breaks these assertions.
describe("tree scroll key (FXA4: pinned to the route list)", () => {
  it("tree=flat changes the scroll key; focus does not", () => {
    expect(SESSIONS_SCROLL_EPHEMERAL).toEqual(["focus", "follow"]);
    const collapsed = normalizeScrollKey("/sessions", SESSIONS_SCROLL_EPHEMERAL);
    const flat = normalizeScrollKey("/sessions?tree=flat", SESSIONS_SCROLL_EPHEMERAL);
    expect(flat).toBe("/sessions?tree=flat");
    expect(flat).not.toBe(collapsed);
    expect(normalizeScrollKey("/sessions?focus=sid-1", SESSIONS_SCROLL_EPHEMERAL)).toBe(collapsed);
  });

  it("tree is absent from the ephemeral list (flat vs collapsed is a different list)", () => {
    expect(SESSIONS_SCROLL_EPHEMERAL).not.toContain("tree");
  });
});
