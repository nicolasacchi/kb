// V70-A6 — the Location Contract (§P7).
//
// ONE total, serialisable value for "where the reader is", ONE encoder/
// decoder pair, and ONE pure `transition(from, to) → push | replace | none`
// table. Everything else in the SPA that wants to move goes through
// `nav/navigate.ts`, which is the only consumer of the three functions here.
//
// WHY A SECOND LAYER OVER `lib/codeUrl.ts`
// `codeUrl.ts` is (and stays) THE builder — root CLAUDE.md #35, golden-pinned.
// It answers "what is the URL for this file at this line". It cannot answer
// "is moving from A to B a push or a replace", because that question is about
// a PAIR of places and about state (`?tab=`, the rail tab, an overlay) that
// several different builders own. So this module is a thin, total wrapper:
// every URL it emits comes out of `codeUrl.ts`'s own functions, and `decode`
// is the exact inverse of `encode` for every shape it claims to understand.
// It never re-implements a path/param grammar of its own — the one thing it
// adds is the `?trail=`/`?step=`/`?via=` triple, and even that lives in
// `codeUrl.ts` (`appendTrail`/`parseTrailLink`), not here.
//
// TOTALITY
// `decode` is total by construction: a URL this contract does not model
// (`/session/:sid/diff`, `/~lens/...`, a route a later unit adds) decodes to
// `{ mode: "other", raw }`, whose `encode` is `raw` verbatim. A round-trip is
// therefore the identity for EVERY same-origin path, which is what lets
// `navigate()` be the single door without having to model the whole app
// first. What `mode: "other"` costs is precision in `transition` — an
// unmodelled → unmodelled move with a different `raw` is a push, which is the
// safe answer.
//
// PANE LOCATION STAYS URL-DERIVED (root CLAUDE.md #30). `panes.pane2` is
// parsed from `?pane2=` by `codeUrl.ts`'s own `parsePane2`; `panes.focused`
// is the one field here that is NOT in the URL (the reader deliberately keeps
// `focusedPane` out of it — recon R10), so it is carried on the value for the
// transition table's benefit and is never encoded.

import {
  appendTrail,
  branchesUrl,
  canvasPageUrl,
  codeUrl,
  commitUrl,
  compareUrl,
  formatLineParam,
  hotspotsUrl,
  parseEntParam,
  parseLineParam,
  parsePane2,
  parseReviewPs,
  parseReviewTab,
  parseTrailLink,
  prsUrl,
  recipesPageUrl,
  reviewDiffHref,
  reviewUrl,
  reviewsUrl,
  stacksPageUrl,
  storyUrl,
  todosUrl,
  type LineSel,
  type PaneLoc,
  type ReviewCockpitTab,
  type TrailLink,
} from "../lib/codeUrl";
import { setsUrl } from "../lib/setsUrl";

/// The centre mode a location addresses. `"other"` is the honest catch-all
/// (see TOTALITY above) — never a guess, never a silent redirect to the
/// reader.
export type CenterMode =
  | "reader"
  | "diff"
  | "story"
  | "review"
  | "review-diff"
  | "page"
  | "search"
  | "home"
  | "inbox"
  | "other";

/// Where inside a file. `line` is 1-based; `range` is the visual-mode/`?line=
/// A-B` selection; `col` exists for the hover/`gd` lane, which addresses a
/// COLUMN, and is deliberately not encoded (no param carries it today).
export interface Anchor {
  line: number;
  col?: number;
  range?: { start: number; end: number };
}

/// The repo-scoped, non-file sentinel pages (`~branches`, `~todos`, …). A
/// closed set, so `encode` can dispatch to the matching `codeUrl.ts` builder
/// rather than string-building a path here.
export type PageId =
  | "branches"
  | "todos"
  | "hotspots"
  | "sets"
  | "prs"
  | "recipes"
  | "stacks"
  | "canvas"
  | "reviews"
  | "commit"
  | "compare";

export interface ReviewLoc {
  id: string;
  tab?: ReviewCockpitTab;
  ps?: number;
  /// The diff's file path (`~reviews/:id/diff/<file>`), `review-diff` only.
  file?: string;
  finding?: string;
}

export interface Location {
  repo: string;
  mode: CenterMode;
  /// `?ref=` — a git revspec. Named `frame` because D14/§P7 grow it into the
  /// frame binding (commit · patchset · session · worktree); today it is
  /// exactly the ref the reader already had.
  frame?: string;
  path?: string;
  /// `?sym=` — a symbol address, resolved server-side on landing.
  sym?: string;
  /// `?ent=` — an ENTITY address (a Ruby constant path). V72-G1.2 gave it its
  /// consumer: a location carrying one puts the reader shell into its
  /// `dossier` center mode (`routes/Reader.tsx`), and `samePlace` below
  /// already treats two different entities as two different PLACES, so
  /// dossier→dossier is a push and dossier→file is a push. The string is
  /// built and parsed by `lib/codeUrl.ts`'s `entityUrl`/`parseEntParam`.
  ent?: string;
  anchor?: Anchor;
  panes: { pane2?: PaneLoc; focused: 1 | 2 };
  /// The inspector rail's active tab + the drawer's active tab. Both are
  /// VIEW state: a change to either is a `replace`, never a push.
  railTab?: string;
  drawerTab?: string;
  /// Open overlays (peek, the palette, help, a sheet). Never encoded, never
  /// a history entry — the transition table returns `"none"` for a move that
  /// changes only this.
  overlays?: readonly string[];
  page?: PageId;
  /// `~commit/:sha` / `~compare?from=&to=` payloads.
  sha?: string;
  compare?: { from: string; to: string; threeDot?: boolean };
  review?: ReviewLoc;
  /// `/search?q=`
  query?: string;
  trail?: TrailLink;
  /// `mode: "other"` only — the verbatim path+search this contract does not
  /// model. `encode` returns it unchanged.
  raw?: string;
}

/// The zero value — used by `transition` when there is no previous location
/// (a cold entry), and by tests.
export function emptyLocation(repo = ""): Location {
  return { repo, mode: "other", panes: { focused: 1 }, raw: "/" };
}

// ── encode ────────────────────────────────────────────────────────────────

function lineSelOf(anchor: Anchor | undefined): LineSel | undefined {
  if (!anchor) return undefined;
  if (anchor.range) return { start: anchor.range.start, end: anchor.range.end };
  return anchor.line > 0 ? anchor.line : undefined;
}

function pageUrl(loc: Location): string {
  switch (loc.page) {
    case "branches":
      return branchesUrl(loc.repo);
    case "todos":
      return todosUrl(loc.repo);
    case "hotspots":
      return hotspotsUrl(loc.repo);
    case "sets":
      return setsUrl(loc.repo);
    case "prs":
      return prsUrl(loc.repo);
    case "recipes":
      return recipesPageUrl(loc.repo);
    case "stacks":
      return stacksPageUrl(loc.repo);
    case "canvas":
      return canvasPageUrl(loc.repo);
    case "reviews":
      return reviewsUrl(loc.repo);
    case "commit":
      return commitUrl(loc.repo, loc.sha ?? "");
    case "compare":
      return compareUrl(loc.repo, {
        from: loc.compare?.from ?? "",
        to: loc.compare?.to ?? "",
        threeDot: loc.compare?.threeDot,
      });
    default:
      return `/r/${encodeURIComponent(loc.repo)}`;
  }
}

/// `Location` → URL. Every branch delegates to a `lib/codeUrl.ts` builder;
/// the only string this function assembles itself is the `?sym=`/`?q=` tail
/// and the trail triple (through `appendTrail`). V72-G1.2 moved `?ent=`'s own
/// grammar into `codeUrl.ts` (`entityUrl`/`parseEntParam`) — the tail appended
/// below is byte-identical to what that builder emits, and
/// `location.test.ts` pins the two against each other.
export function encode(loc: Location): string {
  let url: string;
  switch (loc.mode) {
    case "other":
      url = loc.raw ?? "/";
      break;
    case "home":
      url = "/";
      break;
    case "inbox":
      url = "/~inbox";
      break;
    case "search":
      url = loc.query ? `/search?q=${encodeURIComponent(loc.query)}` : "/search";
      break;
    case "reader": {
      url = codeUrl({
        repo: loc.repo,
        path: loc.path ?? "",
        ref: loc.frame,
        line: lineSelOf(loc.anchor),
        pane2: loc.panes.pane2,
      });
      if (loc.sym) url += `${url.includes("?") ? "&" : "?"}sym=${encodeURIComponent(loc.sym)}`;
      if (loc.ent) url += `${url.includes("?") ? "&" : "?"}ent=${encodeURIComponent(loc.ent)}`;
      break;
    }
    case "diff": {
      const base = codeUrl({ repo: loc.repo, path: loc.path ?? "" });
      const params: string[] = [];
      if (loc.compare?.from) params.push(`from=${encodeURIComponent(loc.compare.from)}`);
      if (loc.compare?.to) params.push(`to=${encodeURIComponent(loc.compare.to)}`);
      url = `${base}/~diff${params.length > 0 ? `?${params.join("&")}` : ""}`;
      break;
    }
    case "story":
      url = storyUrl(loc.repo, loc.path ?? "", loc.sha);
      break;
    case "review":
      url = reviewUrl(loc.repo, loc.review?.id ?? "", {
        tab: loc.review?.tab,
        ps: loc.review?.ps,
      });
      break;
    case "review-diff":
      url = reviewDiffHref(loc.repo, loc.review?.id ?? "", loc.review?.file, {
        finding: loc.review?.finding,
      });
      break;
    case "page":
      url = pageUrl(loc);
      break;
  }
  return appendTrail(url, loc.trail);
}

// ── decode ────────────────────────────────────────────────────────────────

const PAGE_BY_SEGMENT: Readonly<Record<string, PageId>> = {
  "~branches": "branches",
  "~todos": "todos",
  "~hotspots": "hotspots",
  "~sets": "sets",
  "~prs": "prs",
  "~recipes": "recipes",
  "~stacks": "stacks",
  "~canvas": "canvas",
  "~reviews": "reviews",
  "~commit": "commit",
  "~compare": "compare",
};

function anchorFrom(params: URLSearchParams): Anchor | undefined {
  const sel = parseLineParam(params.get("line"));
  if (!sel) return undefined;
  return sel.start === sel.end ? { line: sel.start } : { line: sel.start, range: sel };
}

function splitUrl(url: string): { pathname: string; params: URLSearchParams } {
  const hash = url.indexOf("#");
  const noHash = hash === -1 ? url : url.slice(0, hash);
  const q = noHash.indexOf("?");
  return {
    pathname: q === -1 ? noHash : noHash.slice(0, q),
    params: new URLSearchParams(q === -1 ? "" : noHash.slice(q + 1)),
  };
}

/// URL → `Location`. TOTAL: anything outside the modelled grammar comes back
/// as `mode: "other"` carrying the URL verbatim, so `encode(decode(u)) === u`
/// holds for every same-origin path (see the module doc).
///
/// `focused` is not in the URL (recon R10) and cannot be recovered here — the
/// caller passes the pane it knows has focus, defaulting to 1.
export function decode(url: string, focused: 1 | 2 = 1): Location {
  const { pathname, params } = splitUrl(url);
  const trail = parseTrailLink(params) ?? undefined;
  const base = (extra: Partial<Location>): Location => ({
    repo: "",
    mode: "other",
    panes: { focused },
    raw: url,
    ...(trail ? { trail } : {}),
    ...extra,
  });

  if (pathname === "/") return base({ mode: "home", raw: undefined });
  if (pathname === "/~inbox") return base({ mode: "inbox", raw: undefined });
  if (pathname === "/search") {
    const q = params.get("q");
    return base({ mode: "search", raw: undefined, ...(q ? { query: q } : {}) });
  }

  const segs = pathname.split("/").filter((s) => s !== "");
  if (segs[0] !== "r" || segs.length < 2) return base({});
  const repo = decodeURIComponent(segs[1]);
  const rest = segs.slice(2).map(decodeURIComponent);
  const panes = { focused, ...(parsePane2(params.get("pane2")) ? { pane2: parsePane2(params.get("pane2"))! } : {}) };
  const frame = params.get("ref") ?? undefined;

  // Repo-scoped, non-file sentinels — a FIXED segment right after the repo.
  const page = rest.length > 0 ? PAGE_BY_SEGMENT[rest[0]] : undefined;
  if (page === "reviews" && rest.length >= 2) {
    const id = rest[1];
    // `~reviews/:id/diff[/*]` and `~reviews/:id/f/:slug` are their own modes.
    if (rest[2] === "diff") {
      const file = rest.length > 3 ? rest.slice(3).join("/") : undefined;
      const finding = params.get("finding") ?? undefined;
      return base({
        repo,
        mode: "review-diff",
        raw: undefined,
        panes,
        review: { id, ...(file ? { file } : {}), ...(finding ? { finding } : {}) },
      });
    }
    if (rest[2] === "f") return base({ repo, panes });
    const tab = parseReviewTab(params.get("tab")) ?? undefined;
    const ps = parseReviewPs(params.get("ps")) ?? undefined;
    return base({
      repo,
      mode: "review",
      raw: undefined,
      panes,
      review: { id, ...(tab ? { tab } : {}), ...(ps !== undefined ? { ps } : {}) },
    });
  }
  if (page === "commit" && rest.length >= 2) {
    return base({ repo, mode: "page", raw: undefined, panes, page, sha: rest[1] });
  }
  if (page === "compare") {
    return base({
      repo,
      mode: "page",
      raw: undefined,
      panes,
      page,
      compare: {
        from: params.get("from") ?? "",
        to: params.get("to") ?? "",
        ...(params.get("dots") === "3" ? { threeDot: true } : {}),
      },
    });
  }
  if (page && rest.length === 1) {
    return base({ repo, mode: "page", raw: undefined, panes, page });
  }
  if (page) return base({ repo, panes });

  // File-scoped sentinels trail an arbitrary path (`~diff`, `~story`).
  const last = rest[rest.length - 1];
  if (last === "~diff") {
    const path = rest.slice(0, -1).join("/");
    return base({
      repo,
      mode: "diff",
      raw: undefined,
      panes,
      path,
      compare: { from: params.get("from") ?? "", to: params.get("to") ?? "" },
    });
  }
  if (last === "~story") {
    const path = rest.slice(0, -1).join("/");
    const at = params.get("at") ?? undefined;
    return base({ repo, mode: "story", raw: undefined, panes, path, ...(at ? { sha: at } : {}) });
  }
  // Anything else under `/r/:repo/` that starts with `~` is a sentinel this
  // build does not model — honest `other`, never treated as a file path.
  if (rest.length > 0 && rest[0].startsWith("~")) return base({ repo, panes });

  const sym = params.get("sym") ?? undefined;
  const ent = parseEntParam(params.get("ent")) ?? undefined;
  return base({
    repo,
    mode: "reader",
    raw: undefined,
    panes,
    path: rest.join("/"),
    ...(frame ? { frame } : {}),
    ...(sym ? { sym } : {}),
    ...(ent ? { ent } : {}),
    ...(anchorFrom(params) ? { anchor: anchorFrom(params) } : {}),
  });
}

// ── the transition table ──────────────────────────────────────────────────

export type Transition = "push" | "replace" | "none";

/// True when two locations name the same PLACE — the same repo, mode and
/// subject (path / sym / page / review id). This is the axis `push` keys on;
/// everything else (anchor, pane focus, view tabs, overlays) refines a place
/// rather than changing it.
function samePlace(a: Location, b: Location): boolean {
  if (a.repo !== b.repo || a.mode !== b.mode) return false;
  switch (a.mode) {
    case "reader":
      return (a.path ?? "") === (b.path ?? "") && (a.sym ?? "") === (b.sym ?? "") && (a.ent ?? "") === (b.ent ?? "");
    case "diff":
    case "story":
      return (a.path ?? "") === (b.path ?? "");
    case "review":
    case "review-diff":
      return (a.review?.id ?? "") === (b.review?.id ?? "");
    case "page":
      return a.page === b.page && (a.sha ?? "") === (b.sha ?? "");
    case "search":
      return true; // a query change is a refinement of one page (recon R6)
    case "home":
    case "inbox":
      return true;
    case "other":
      return (a.raw ?? "") === (b.raw ?? "");
  }
}

function sameAnchor(a: Location, b: Location): boolean {
  const la = lineSelOf(a.anchor);
  const lb = lineSelOf(b.anchor);
  return (la === undefined ? "" : formatLineParam(la)) === (lb === undefined ? "" : formatLineParam(lb));
}

function sameOverlays(a: Location, b: Location): boolean {
  const oa = [...(a.overlays ?? [])].sort().join(",");
  const ob = [...(b.overlays ?? [])].sort().join(",");
  return oa === ob;
}

/// Everything except the overlay set — i.e. "is this move ONLY an overlay
/// open/close?".
function sameExceptOverlays(a: Location, b: Location): boolean {
  return encode({ ...a, overlays: undefined }) === encode({ ...b, overlays: undefined }) &&
    a.panes.focused === b.panes.focused &&
    (a.railTab ?? "") === (b.railTab ?? "") &&
    (a.drawerTab ?? "") === (b.drawerTab ?? "");
}

/// THE table (§P7, golden-pinned in `location.test.ts`). Read top to bottom:
///
///   1. an overlay opening or closing is NOT a navigation           → none
///   2. nothing at all changed                                      → none
///   3. a different place (repo · mode · path · sym · page · review) → push
///   4. same file, different cursor/scroll anchor                    → replace
///   5. view state only (rail tab · drawer tab · review tab · patchset ·
///      pane focus · frame · trail linkage)                          → replace
///
/// Rule 4 is what `lib/cursorUrlSync.ts` has always done by hand
/// (`history.replaceState`, "a cursor move is not a navigation event"); it is
/// written down here so the other twenty surfaces cannot disagree with it.
export function transition(from: Location | null, to: Location): Transition {
  if (!from) return "push";
  // 1 — an overlay open/close, and NOTHING else, is not a navigation.
  if (sameExceptOverlays(from, to) && !sameOverlays(from, to)) return "none";
  // 2 — a different place.
  if (!samePlace(from, to)) return "push";
  // 3 — same file, different cursor/scroll anchor.
  if (!sameAnchor(from, to)) return "replace";
  // 4 — view state. `railTab`/`drawerTab` are NOT in kb-code's URL today (the
  // Desk persists them), so this branch produces a replace whose URL is
  // byte-identical — a deliberate no-op. It is written down anyway because the
  // point of a table is that the ANSWER does not change when the encoding
  // does: the unit that puts the rail tab in the URL must not have to
  // re-litigate whether switching tabs is a push.
  if (
    (from.railTab ?? "") !== (to.railTab ?? "") ||
    (from.drawerTab ?? "") !== (to.drawerTab ?? "") ||
    from.panes.focused !== to.panes.focused ||
    encode({ ...from, trail: undefined }) !== encode({ ...to, trail: undefined }) ||
    (from.trail?.id ?? "") !== (to.trail?.id ?? "") ||
    (from.trail?.step ?? -1) !== (to.trail?.step ?? -1)
  ) {
    return "replace";
  }
  // 5 — genuinely the same location.
  return "none";
}

/// The comparison key `transition` and the scroll layer use: the encoded URL
/// with the trail triple removed, because a hop that differs only in its
/// trail linkage is the same place (`lib/codeUrl.ts`'s `stripTrail`).
export function locationKey(loc: Location): string {
  return encode({ ...loc, trail: undefined });
}
