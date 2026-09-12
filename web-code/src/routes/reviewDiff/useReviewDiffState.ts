// `ReviewDiff`'s URL state, in one hook (V73-K2b — the readers and the
// writers moved out of `routes/ReviewDiff.tsx` verbatim, no behaviour
// change).
//
// Diff v2's first rule is that **the URL is the only state** (web-code/
// CLAUDE.md § Review diff v2): `?ps=` · `?ctx=` · `?noise=` · `?map=` ·
// `?file=` · `?hunk=` join the pre-existing `?view=`/`?line=`/`?side=`/
// `?thread=`/`?finding=`/`?overlay=`/`?tour=`, each with a TOTAL parser that
// degrades junk to the documented default. Before this unit those thirteen
// reads and six writers were spread over four hundred lines of a two-
// thousand-line component, which is how "the URL is the state, except this
// one control" starts. They are one surface here so that the rule is
// checkable by reading ONE file.
//
// Two properties are load-bearing and both are preserved verbatim:
//
// * every writer uses `replace: true` — a view knob is not a navigation
//   step, and Back must leave the diff rather than undo a dial click;
// * `prefMode` (the persisted layout preference) is the FALLBACK for
//   `?view=`, never the other way round, so a URL always wins over a
//   remembered preference and a copied link reproduces the sender's view.
import { useCallback, useState } from "react";
import { useSearchParams } from "react-router";
import {
  formatDiffPs,
  parseDiffCtx,
  parseDiffMap,
  parseDiffPs,
  type DiffCtxDial,
  type DiffPsSelection,
} from "../../lib/codeUrl";
import {
  overlayParamValue,
  parseOverlayParam,
  type OverlayMode,
} from "../../lib/diffFindings";
import { parseNoiseMode, type NoiseMode } from "../../lib/diffNoise";
import { loadDiffMode, saveDiffMode, type DiffMode } from "../../lib/prefs";
import { formatExpandedParam, parseExpandedParam } from "../../lib/collapseOnTick";
import { parseLine, parseSide, parseView } from "./helpers";

export interface ReviewDiffUrlState {
  /** `null` = latest; a number = one patchset; a range = the interdiff. */
  psSel: DiffPsSelection | null;
  /** The `a..b` arm of `psSel`, or `null` for the ordinary single view. */
  psRange: { from: number; to: number } | null;
  /** What the file/comment queries ask for: `"latest"` or a number. */
  psQuery: string;
  ctxDial: DiffCtxDial;
  noiseMode: NoiseMode;
  mapParamOpen: boolean;
  hunkParam: string | null;
  urlView: DiffMode | null;
  line: number | null;
  side: "old" | "new" | null;
  fileHint: string;
  threadId: string | null;
  findingParam: string | null;
  overlay: OverlayMode;
  /** `?view=` when present, else the persisted layout preference. */
  mode: DiffMode;
  tourParamOn: boolean;
  /// V76-R2c — viewed-but-expanded file paths (`?expanded=`) and hunk ids
  /// (`?hexpanded=`). Empty is the omitted default.
  expandedFiles: readonly string[];
  expandedHunks: readonly string[];

  setParam: (key: string, value: string | null) => void;
  setPs: (next: DiffPsSelection | null) => void;
  setCtx: (next: DiffCtxDial) => void;
  setNoiseMode: (next: NoiseMode) => void;
  setMapOpen: (next: boolean) => void;
  setView: (next: DiffMode) => void;
  setOverlay: (next: OverlayMode) => void;
  setTourParam: (on: boolean) => void;
  /** `href` + the page's CURRENT query string — the single-file focus link. */
  withQuery: (href: string) => string;
  setExpanded: (files: readonly string[], hunks: readonly string[]) => void;
}

export function useReviewDiffState(): ReviewDiffUrlState {
  const [searchParams, setSearchParams] = useSearchParams();

  // V73-K2a — `?ps=` is now WRITTEN as well as read (the full-page diff was
  // permanently pinned to "latest" before this unit). A bare number picks
  // one patchset; `a..b` picks the INTERDIFF between two.
  const psSel = parseDiffPs(searchParams.get("ps"));
  const psRange = psSel !== null && typeof psSel !== "number" ? psSel : null;
  const psQuery = psSel === null ? "latest" : String(typeof psSel === "number" ? psSel : psSel.to);
  const ctxDial = parseDiffCtx(searchParams.get("ctx"));
  const noiseMode: NoiseMode = parseNoiseMode(searchParams.get("noise"));
  const mapParamOpen = parseDiffMap(searchParams.get("map"));
  const hunkParam = searchParams.get("hunk");
  const urlView = parseView(searchParams.get("view"));
  const line = parseLine(searchParams.get("line"));
  const side = parseSide(searchParams.get("side"));
  const fileHint = searchParams.get("file") ?? "";
  const threadId = searchParams.get("thread");
  // PRR-U3 — `?finding=<f-slug>` deep link + `?overlay=` toggle.
  const findingParam = searchParams.get("finding");
  const overlay: OverlayMode = parseOverlayParam(searchParams.get("overlay"));

  const [prefMode, setPrefMode] = useState<DiffMode>(() => loadDiffMode());
  const mode: DiffMode = urlView ?? prefMode;
  const expandedFiles = parseExpandedParam(searchParams.get("expanded"));
  const expandedHunks = parseExpandedParam(searchParams.get("hexpanded"));

  function withQuery(href: string): string {
    const q = searchParams.toString();
    return q ? `${href}?${q}` : href;
  }

  function setView(next: DiffMode) {
    saveDiffMode(next);
    setPrefMode(next);
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set("view", next);
    setSearchParams(nextParams, { replace: true });
  }

  function setOverlay(next: OverlayMode) {
    const nextParams = new URLSearchParams(searchParams);
    const v = overlayParamValue(next);
    if (v) nextParams.set("overlay", v);
    else nextParams.delete("overlay");
    setSearchParams(nextParams, { replace: true });
  }

  function setTourParam(on: boolean) {
    const nextParams = new URLSearchParams(searchParams);
    if (on) nextParams.set("tour", "1");
    else nextParams.delete("tour");
    setSearchParams(nextParams, { replace: true });
  }

  // --- V73-K2a — the five URL writers -----------------------------------
  //
  // ONE helper, so every diff-v2 control writes its param the same way and
  // "the URL is the state" cannot rot into "the URL is the state, except
  // this control". `replace: true` matches every pre-existing writer on
  // this route (view/overlay/tour): a view knob is not a navigation step,
  // and Back must still leave the diff rather than undo a dial click.
  const setParam = useCallback(
    (key: string, value: string | null) => {
      const next = new URLSearchParams(searchParams);
      if (value === null) next.delete(key);
      else next.set(key, value);
      setSearchParams(next, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  const setPs = useCallback(
    (next: DiffPsSelection | null) => setParam("ps", next === null ? null : formatDiffPs(next)),
    [setParam],
  );
  const setCtx = useCallback(
    (next: DiffCtxDial) => setParam("ctx", next === 3 ? null : String(next)),
    [setParam],
  );
  const setNoiseMode = useCallback(
    (next: NoiseMode) => setParam("noise", next === "shown" ? null : next),
    [setParam],
  );
  const setMapOpen = useCallback(
    (next: boolean) => setParam("map", next ? null : "0"),
    [setParam],
  );

  const setExpanded = useCallback(
    (files: readonly string[], hunks: readonly string[]) => {
      const next = new URLSearchParams(searchParams);
      const f = formatExpandedParam(files);
      const h = formatExpandedParam(hunks);
      if (f) next.set("expanded", f);
      else next.delete("expanded");
      if (h) next.set("hexpanded", h);
      else next.delete("hexpanded");
      setSearchParams(next, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  return {
    psSel,
    psRange,
    psQuery,
    ctxDial,
    noiseMode,
    mapParamOpen,
    hunkParam,
    urlView,
    line,
    side,
    fileHint,
    threadId,
    findingParam,
    overlay,
    mode,
    tourParamOn: searchParams.get("tour") === "1",
    expandedFiles,
    expandedHunks,
    setParam,
    setPs,
    setCtx,
    setNoiseMode,
    setMapOpen,
    setView,
    setOverlay,
    setTourParam,
    withQuery,
    setExpanded,
  };
}
