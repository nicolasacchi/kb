import { describe, expect, it } from "vitest";
import { tocJumpParams } from "./tocJumpTarget";
import type { PaneLoc } from "./paneUrl";

describe("tocJumpParams", () => {
  it("targets ?sec= when no split is open", () => {
    expect(tocJumpParams("intro", "canon", "a.html", null)).toEqual({
      param: "sec",
      value: "intro",
    });
  });

  it("targets ?sec= when pane2 is open but names a DIFFERENT artifact", () => {
    const pane2: PaneLoc = { kb: "canon", sourceRelative: "other.html" };
    expect(tocJumpParams("intro", "canon", "a.html", pane2)).toEqual({
      param: "sec",
      value: "intro",
    });
  });

  it("targets ?sec= when pane2 names the same path in a DIFFERENT kb", () => {
    const pane2: PaneLoc = { kb: "research", sourceRelative: "a.html" };
    expect(tocJumpParams("intro", "canon", "a.html", pane2)).toEqual({
      param: "sec",
      value: "intro",
    });
  });

  it("targets ?pane2= when the open doc IS pane 2's artifact", () => {
    const pane2: PaneLoc = { kb: "canon", sourceRelative: "a.html" };
    expect(tocJumpParams("intro", "canon", "a.html", pane2)).toEqual({
      param: "pane2",
      value: "canon:a.html:intro",
    });
  });

  it("overwrites an existing pane2 sec rather than dropping it", () => {
    const pane2: PaneLoc = {
      kb: "canon",
      sourceRelative: "a.html",
      sec: "old-heading",
    };
    expect(tocJumpParams("new-heading", "canon", "a.html", pane2)).toEqual({
      param: "pane2",
      value: "canon:a.html:new-heading",
    });
  });

  it("percent-encodes a heading id that collides with the ':' field separator", () => {
    const pane2: PaneLoc = { kb: "canon", sourceRelative: "a.html" };
    expect(tocJumpParams("a:b", "canon", "a.html", pane2)).toEqual({
      param: "pane2",
      value: "canon:a.html:a%3Ab",
    });
  });
});
