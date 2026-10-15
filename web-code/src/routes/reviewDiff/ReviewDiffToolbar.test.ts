import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { StaticRouter } from "react-router";
import { describe, expect, it } from "vitest";
import ReviewDiffToolbar, { type ReviewDiffToolbarProps } from "./ReviewDiffToolbar";
import { EMPTY_DRAFTS } from "../../lib/reviewDrafts";

// OL C5 — the review-diff toolbar is one ViewBar of labelled clusters. The
// More menu is closed in a static render, so everything marked up here is a
// control that is VISIBLE in the bar; the budget is nine.
const noop = () => {};
function props(over: Partial<ReviewDiffToolbarProps> = {}): ReviewDiffToolbarProps {
  return {
    repo: "r",
    id: 1,
    title: "T",
    patchsets: [],
    psSel: null,
    psRange: null,
    authorOnly: false,
    onSetAuthorOnly: noop,
    psQuery: "1",
    activePsNum: 1,
    onSetPs: noop,
    viewedCount: 0,
    filesCount: 3,
    pct: 0,
    mode: "unified",
    onSetView: noop,
    overlay: "all",
    onSetOverlay: noop,
    ctxDial: 3,
    onSetCtx: noop,
    noiseMode: "shown",
    onSetNoiseMode: noop,
    noiseStats: { total: 0, labelled: 0, byClass: new Map() },
    isMobile: false,
    mapOpen: false,
    mapParamOpen: false,
    onSetMapOpen: noop,
    draftsOpen: false,
    onSetDraftsOpen: noop,
    drafts: EMPTY_DRAFTS,
    tourOn: false,
    tourStops: [],
    tourStepIdx: 0,
    onStartTour: noop,
    onExitTour: noop,
    hasPrev: false,
    hasNext: true,
    cursorIdx: 0,
    onGoFile: noop,
    filesOpen: false,
    onSetFilesOpen: noop,
    jump: "",
    onSetJump: noop,
    jumpHits: [],
    paths: [],
    onSetHelpOpen: noop,
    onPickFile: noop,
    filesMode: "changed",
    onSetFilesMode: noop,
    ...over,
  } as ReviewDiffToolbarProps;
}
const render = (over?: Partial<ReviewDiffToolbarProps>) =>
  renderToStaticMarkup(h(StaticRouter, { location: "/" }, h(ReviewDiffToolbar, props(over))));
const count = (html: string, needle: string) => html.split(needle).length - 1;

describe("ReviewDiffToolbar (OL C5)", () => {
  it("draws four labelled clusters in reading order and keeps the shell hooks", () => {
    const html = render();
    const legends = [...html.matchAll(/kb-viewbar__legend">([^<]+)</g)].map((m) => m[1]);
    expect(legends).toEqual(["Patchset", "Navigate", "View", "Unsent"]);
    expect(html).toContain("data-kbc-rdiff-toolbar");
    for (const hook of ["back", "title", "help", "files-toggle"]) {
      expect(html).toContain(`data-kbc-rdiff-${hook}`);
    }
  });

  it("shows exactly nine controls on desktop; Lanes and Show map wait in More", () => {
    const html = render();
    const visible = [
      "data-kbc-rdiff-ps=\"\"", // patchset chip
      "data-kbc-review-progress=",
      "data-kbc-rdiff-jump=",
      "data-kbc-rdiff-tour-start",
      'data-kbc-diff-mode-toggle="unified"',
      "data-kbc-rdiff-files-mode=\"changed\"",
      "data-kbc-rdiff-ctx=",
      "data-kbc-rdiff-noise=",
      "data-kbc-rdiff-drafts-toggle=",
    ];
    for (const v of visible) expect(html, v).toContain(v);
    expect(count(html, "kb-viewbar__legend")).toBe(4);
    expect(html).toContain("data-kb-viewbar-more");
    expect(html).not.toContain("data-kbc-rdiff-overlay-select");
    expect(html).not.toContain("data-kbc-rdiff-map-toggle");
  });

  it("drops the overlay counts and renders the noise census as the Hide noise caption", () => {
    const html = render();
    expect(html).not.toContain("overlay-counts");
    expect(html).toContain("Hide noise");
    expect(html).toContain("data-kbc-rdiff-noise-census");
  });

  it("keeps Drafts visible with its count label", () => {
    expect(render()).toMatch(/data-kbc-rdiff-drafts-toggle="0"[^>]*>.*Drafts 0/s);
  });

  it("keeps every control in the bar on mobile (the toolbar scrolls, a menu would clip)", () => {
    const html = render({ isMobile: true });
    expect(html).not.toContain("data-kb-viewbar-more");
    expect(html).toContain("data-kbc-rdiff-overlay-select");
  });
});
