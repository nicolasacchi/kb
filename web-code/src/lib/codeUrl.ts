// The single deep-link builder for the reader (A2 of "kb-code v2 — The
// Operable Reader"). Every clickable surface that lands on a specific
// file/ref/line — breadcrumbs, the file tree, the ref picker, "back to
// file" from the diff view, the omnibox/search lanes, cursor-position
// permalinks, and (Wave E) the second split pane — goes through the
// helpers in this ONE module, so the URL grammar can't drift across
// components. Mirrors kb's own `galleryUrl` discipline (kb's
// CLAUDE.md invariant #35): golden-pinned outputs, one builder, one parser.
//
// Query param order is stable and matches the reader's pre-existing
// grammar (`?ref=&line=`, see the old `readerUrl` in breadcrumbs.ts) so
// URLs bookmarked before this module existed keep resolving the same way:
// `ref`, then `line`, then `pane2`.

export type LineSel = number | { start: number; end: number };

export interface PaneLoc {
  path: string;
  ref?: string;
  line?: LineSel;
}

export interface CodeLoc {
  repo: string;
  path: string;
  ref?: string;
  line?: LineSel;
  /// RESERVED for Wave E's split-pane view — the builder/parser ship now so
  /// the grammar is pinned early, but no route/component reads or writes
  /// `pane2` yet. Do not wire a consumer to this ahead of Wave E.
  pane2?: PaneLoc;
}

/// Percent-encode `path`'s segments exactly the way the reader always has
/// (see the pre-A2 `readerUrl`/`diffUrl` in breadcrumbs.ts): split on `/`,
/// drop empty segments (stray leading/trailing/doubled slashes), then
/// `encodeURIComponent` each segment individually so a literal `/` inside a
/// filename can never be confused with a path separator.
function encodePathSegments(path: string): string {
  return path
    .split("/")
    .filter((s) => s !== "")
    .map(encodeURIComponent)
    .join("/");
}

/// The reader route's base path for `repo`/`path`, with no query string —
/// shared by `codeUrl` and by breadcrumbs.ts's `diffUrl` (which appends its
/// own `/~diff` segment + `from`/`to` params instead of `ref`/`line`).
export function codeBasePath(repo: string, path: string): string {
  const encodedPath = encodePathSegments(path);
  const repoSeg = encodeURIComponent(repo);
  return encodedPath === "" ? `/r/${repoSeg}` : `/r/${repoSeg}/${encodedPath}`;
}

/// Serialize a line selector for the `line=` query param: a single 1-based
/// line number formats as `"10"`; a range formats as `"10-24"` (start/end
/// reordered so the smaller number always comes first). Returns `""` for a
/// non-positive/non-finite selector — callers treat that as "omit the
/// param" (mirrors the pre-A2 `readerUrl`'s `line > 0` gate).
export function formatLineParam(sel: LineSel): string {
  if (typeof sel === "number") {
    return Number.isFinite(sel) && sel > 0 ? String(sel) : "";
  }
  const { start, end } = sel;
  if (!Number.isFinite(start) || !Number.isFinite(end) || start <= 0 || end <= 0) return "";
  const lo = Math.min(start, end);
  const hi = Math.max(start, end);
  return lo === hi ? String(lo) : `${lo}-${hi}`;
}

const LINE_PARAM_PATTERN = /^(\d+)(?:-(\d+))?$/;

/// Parse a `line=` query value back into a normalized `{start, end}` range
/// (a single line normalizes to `start === end`). Total: any junk input
/// (`null`, empty, non-numeric, zero, negative, NaN) returns `null` rather
/// than throwing. `start`/`end` are swapped if given out of order.
export function parseLineParam(v: string | null): { start: number; end: number } | null {
  if (!v) return null;
  const m = v.match(LINE_PARAM_PATTERN);
  if (!m) return null;
  const a = parseInt(m[1], 10);
  const b = m[2] !== undefined ? parseInt(m[2], 10) : a;
  if (!Number.isFinite(a) || !Number.isFinite(b) || a <= 0 || b <= 0) return null;
  return a <= b ? { start: a, end: b } : { start: b, end: a };
}

/// Serialize a second-pane location for the (Wave E-reserved) `pane2=`
/// query param, e.g. `"src/foo.rs@abc123:10-24"`. `ref`/`line` are
/// optional but the `@`/`:` separators are always present so the grammar
/// is unambiguous to parse back — see `parsePane2`. The caller
/// (`codeUrl`) is responsible for `encodeURIComponent`-ing the result as
/// a single query value; this function returns the raw, un-encoded
/// grammar string.
export function formatPane2(p: PaneLoc): string {
  const lineParam = p.line !== undefined ? formatLineParam(p.line) : "";
  return `${p.path}@${p.ref ?? ""}:${lineParam}`;
}

/// Parse a `pane2=` query value (already URL-decoded, e.g. via
/// `URLSearchParams.get`) back into a `PaneLoc`. Total: returns `null` for
/// anything that doesn't match the `path@ref:line` grammar. Splits on the
/// LAST `@` so a path containing its own `@` (rare, but not impossible)
/// still resolves correctly, since the separator `@` is always the one
/// immediately preceding the trailing `ref:line` suffix. A malformed
/// trailing `line` segment invalidates the whole value (returns `null`)
/// rather than silently dropping just the line.
export function parsePane2(v: string | null): PaneLoc | null {
  if (!v) return null;
  const at = v.lastIndexOf("@");
  if (at === -1) return null;
  const path = v.slice(0, at);
  if (path === "") return null;
  const rest = v.slice(at + 1);
  const colon = rest.indexOf(":");
  if (colon === -1) return null;
  const refPart = rest.slice(0, colon);
  const lineParamPart = rest.slice(colon + 1);

  const loc: PaneLoc = { path };
  if (refPart !== "") loc.ref = refPart;
  if (lineParamPart !== "") {
    const parsed = parseLineParam(lineParamPart);
    if (!parsed) return null;
    loc.line = parsed;
  }
  return loc;
}

/// Build the reader's client-route URL for a `CodeLoc`. `path` empty
/// renders the repo-root tree URL. Query params, when present, always
/// appear in this order: `ref`, `line`, `pane2` (see the module doc for
/// why the order is load-bearing).
export function codeUrl(loc: CodeLoc): string {
  const base = codeBasePath(loc.repo, loc.path);
  const params: string[] = [];
  if (loc.ref) params.push(`ref=${encodeURIComponent(loc.ref)}`);
  if (loc.line !== undefined) {
    const lp = formatLineParam(loc.line);
    if (lp !== "") params.push(`line=${lp}`);
  }
  if (loc.pane2) {
    const p2 = formatPane2(loc.pane2);
    if (p2 !== "") params.push(`pane2=${encodeURIComponent(p2)}`);
  }
  return params.length > 0 ? `${base}?${params.join("&")}` : base;
}

/// Absolute-URL form of `codeUrl`, for permalinks copied out of the reader
/// (a trailing slash on `origin`, if any, is trimmed so the join never
/// double-slashes).
export function permalinkFor(origin: string, loc: CodeLoc): string {
  const trimmedOrigin = origin.endsWith("/") ? origin.slice(0, -1) : origin;
  return `${trimmedOrigin}${codeUrl(loc)}`;
}

// --- Phase C-SPA — time-first-class URLs (commit/compare/branches) --------
//
// These three are repo-scoped, non-file sentinels — like `~diff` (see
// `breadcrumbs.ts`'s `diffUrl`), `~commit`/`~compare`/`~branches` are a
// fixed path segment right after `repo`, never followed by an arbitrary
// nested file path, so (unlike `~diff`, which rides Reader's own splat
// parsing because it trails an arbitrary file path) they're plain static
// route segments `app.tsx` registers as their own `<Route>`s alongside the
// `/r/:repo/*` Reader catch-all.

/// `commitUrl(repo, sha)` → `/r/{repo}/~commit/{sha}` — the commit page hub.
/// `sha` is percent-encoded like any other path segment (`encodePathSegments`
/// discipline) even though a real sha is always plain hex — consistency
/// with every other segment in this module, not a defensive necessity.
export function commitUrl(repo: string, sha: string): string {
  return `${codeBasePath(repo, "")}/~commit/${encodeURIComponent(sha)}`;
}

export interface CompareOpts {
  from: string;
  to: string;
  /// `true` renders a three-dot compare (`from...to`, ranged from the
  /// merge-base) — see `history::compare`'s module doc. Omitted/`false` is
  /// the ordinary two-dot `from..to` form.
  threeDot?: boolean;
}

/// `compareUrl(repo, {from, to, threeDot?})` → `/r/{repo}/~compare?from=&to=
/// [&dots=3]` — param order is always `from`, `to`, `dots` (present only for
/// a three-dot compare).
export function compareUrl(repo: string, opts: CompareOpts): string {
  const params = new URLSearchParams({ from: opts.from, to: opts.to });
  if (opts.threeDot) params.set("dots", "3");
  return `${codeBasePath(repo, "")}/~compare?${params.toString()}`;
}

/// `branchesUrl(repo)` → `/r/{repo}/~branches`.
export function branchesUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~branches`;
}

// --- Phase G-server ("the review-workflow endpoints") ----------------------

export interface RangeDiffOpts {
  old: string;
  new: string;
}

/// `rangeDiffUrl(repo, {old, new})` → `/r/{repo}/~range-diff?old=&new=` —
/// param order is always `old`, `new`; either/both may be omitted (the
/// Compare page's "compare rebases →" link omits both, landing on the
/// range-diff page's own blank-input empty state rather than guessing a
/// starting range from the compare it was clicked from).
export function rangeDiffUrl(repo: string, opts?: Partial<RangeDiffOpts>): string {
  const params = new URLSearchParams();
  if (opts?.old) params.set("old", opts.old);
  if (opts?.new) params.set("new", opts.new);
  const qs = params.toString();
  return `${codeBasePath(repo, "")}/~range-diff${qs ? `?${qs}` : ""}`;
}

/// `prsUrl(repo)` → `/r/{repo}/~prs` — the GitHub PR read overlay (Phase
/// G4).
export function prsUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~prs`;
}

/// `todosUrl(repo)` → `/r/{repo}/~todos` — Phase N TODO index page.
export function todosUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~todos`;
}

/// `hotspotsUrl(repo)` → `/r/{repo}/~hotspots` — V3.2-B3 attention hotspots.
export function hotspotsUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~hotspots`;
}

/// `reviewsUrl(repo)` → `/r/{repo}/~reviews` — V3.R2 review-session list.
export function reviewsUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~reviews`;
}

/// The cockpit's view-tab vocabulary — `CockpitTabs.tsx`'s own `CockpitView`
/// union, mirrored here (not imported — `lib/` doesn't reach into
/// `components/`, same posture `ReviewDiffHrefOpts.overlay` already takes
/// on this file's own `OverlayMode` twin) so `reviewUrl`'s `tab` option and
/// `parseReviewTab`'s return type stay structurally assignable to it.
export type ReviewCockpitTab = "report" | "files" | "map" | "order" | "timeline";

const REVIEW_COCKPIT_TABS: ReadonlySet<string> = new Set<ReviewCockpitTab>([
  "report",
  "files",
  "map",
  "order",
  "timeline",
]);

/// `"files"` is the cockpit's own fallback default (`CockpitTabs.tsx` /
/// `ReviewDetail.tsx`'s pre-V70-A3S `useState<CockpitView>("files")`) — kept
/// as its own named constant so `reviewUrl`'s omit-the-default rule and
/// `parseReviewTab`'s fallback can't drift apart by hand-typing `"files"`
/// twice.
const REVIEW_COCKPIT_DEFAULT_TAB: ReviewCockpitTab = "files";

export interface ReviewUrlOpts {
  /// `?tab=` — omitted for the default `"files"` tab (byte-identical to
  /// every pre-V70-A3S `reviewUrl(repo, id)` 2-arg call site).
  tab?: ReviewCockpitTab;
  /// `?ps=` — a 1-based patchset number, or the literal `"latest"` default
  /// (omitted, same "don't emit the default" rule as `tab`).
  ps?: number | "latest";
}

/// `reviewUrl(repo, id, opts?)` → `/r/{repo}/~reviews/{id}[?tab=][&ps=]` —
/// one review's cockpit. V70-A3S added `opts` (cockpit tab + selected
/// patchset, ReviewDetail.tsx's own local-state-turned-URL-state); every
/// pre-existing 2-arg call site's output is byte-identical (`opts`
/// undefined, or either field at its default, is a no-op — the `pane2`/
/// `reviewDiffHref` precedent this module's header doc already
/// establishes). Param order is always `tab` THEN `ps`, appended after the
/// base path.
export function reviewUrl(repo: string, id: number | string, opts?: ReviewUrlOpts): string {
  const base = `${codeBasePath(repo, "")}/~reviews/${encodeURIComponent(String(id))}`;
  if (!opts) return base;
  const params: string[] = [];
  if (opts.tab && opts.tab !== REVIEW_COCKPIT_DEFAULT_TAB) {
    params.push(`tab=${encodeURIComponent(opts.tab)}`);
  }
  if (opts.ps !== undefined && opts.ps !== "latest") {
    params.push(`ps=${encodeURIComponent(String(opts.ps))}`);
  }
  return params.length > 0 ? `${base}?${params.join("&")}` : base;
}

/// Parse a `?tab=` value back into a `ReviewCockpitTab`, or `null` for
/// anything absent/unrecognized (an older bookmarked URL with no `tab=`,
/// or a future tab this build doesn't know — both degrade to the caller's
/// own default rather than throwing).
export function parseReviewTab(v: string | null): ReviewCockpitTab | null {
  return v !== null && REVIEW_COCKPIT_TABS.has(v) ? (v as ReviewCockpitTab) : null;
}

/// Parse a `?ps=` value back into a positive patchset number, or `null` for
/// anything absent/malformed — callers treat `null` as "latest" (`ps=`'s
/// own omit-the-default rule mirrored on the read side). `"latest"` itself
/// is NOT a valid `?ps=` value on the wire (`reviewUrl` never emits it —
/// absence IS "latest"), so it parses to `null` here too, same as any other
/// non-numeric junk.
export function parseReviewPs(v: string | null): number | null {
  if (v === null) return null;
  const n = Number(v);
  return Number.isFinite(n) && n > 0 && Number.isInteger(n) ? n : null;
}

// --- PRR-U3 — full-page diff findings overlay (design-ui.md §5) -----------
//
// `reviewDiffHref` pre-existed as an identical local helper duplicated in
// both `routes/ReviewDiff.tsx` and `components/reviews/ReviewHeader.tsx`
// (neither imported from this module) — a pre-existing duplication, not
// introduced here. This unit canonicalizes ONE copy into `codeUrl.ts` (the
// home design-ui.md §5 names) and repoints its own `routes/ReviewDiff.tsx`
// at it. V70-A3S finished the reconciliation: `ReviewHeader.tsx`'s own
// weaker local copy (no `opts`/`finding=`/`overlay=`) is now a bare
// re-export of THIS function — one implementation, still reachable from
// both `from "../../lib/codeUrl"` and the four pre-existing
// `from "./ReviewHeader"` importers.

export interface ReviewDiffHrefOpts {
  /// `?finding=<f-slug>` — scroll+expand+flash, same machinery as `?thread=`.
  finding?: string;
  /// `?overlay=findings|comments|diagnostics|none` — omitted for the
  /// default `"all"`. PRR-U9 added `"diagnostics"` (design-addendum-2.md
  /// §D); kept as its own literal union here rather than importing
  /// `lib/diffFindings.ts`'s `OverlayMode` — a pre-existing duplication
  /// (this unit's own report notes the same choice for `parseOverlayParam`'s
  /// twin), so the two are kept in lock-step by hand.
  overlay?: "all" | "findings" | "comments" | "diagnostics" | "none";
}

/// `reviewDiffHref(repo, id, file?, opts?)` → `/r/{repo}/~reviews/{id}/diff
/// [/file]` `[?finding=][&overlay=]`. `opts` params are appended LAST (the
/// `pane2` precedent, `codeUrl`'s own module doc) so every existing 2/3-arg
/// call site's URL stays byte-identical — `opts` is purely additive.
export function reviewDiffHref(
  repo: string,
  id: number | string,
  file?: string,
  opts?: ReviewDiffHrefOpts,
): string {
  const base = `${reviewUrl(repo, id)}/diff`;
  const withFile = !file
    ? base
    : `${base}/${file
        .split("/")
        .filter((s) => s !== "")
        .map(encodeURIComponent)
        .join("/")}`;
  const params: string[] = [];
  if (opts?.finding) params.push(`finding=${encodeURIComponent(opts.finding)}`);
  if (opts?.overlay && opts.overlay !== "all") params.push(`overlay=${opts.overlay}`);
  return params.length > 0 ? `${withFile}?${params.join("&")}` : withFile;
}

/// `findingUrl(repo, id, slug)` → `/r/{repo}/~reviews/{id}/f/{slug}` — the
/// short, share/CLI-printable form (design-ui.md §5). `routes/
/// FindingEntry.tsx` (V70-A3S) registers the redirect route for this
/// grammar — a thin `<Navigate replace>` onto `reviewDiffHref(repo, id,
/// undefined, {finding: slug})`'s real Room URL, the same guaranteed-
/// working target the finding permalink COPY action in `DiffThread`/
/// keyboard `y` already used directly.
export function findingUrl(repo: string, id: number | string, slug: string): string {
  return `${reviewUrl(repo, id)}/f/${encodeURIComponent(slug)}`;
}

/// `recipesUrl(repo)` → `/r/{repo}/~recipes` — V3.3-U1 recipe catalog.
/// Query state (`recipe`/`since`/`limit`) is owned by `lib/recipesUrl.ts`.
export function recipesPageUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~recipes`;
}

/// `stacksPageUrl(repo)` → `/r/{repo}/~stacks` — V3.3-U1 stack awareness.
/// Query state (`all`/`branch`) is owned by `lib/stacksFormat.ts`.
export function stacksPageUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~stacks`;
}

/// `canvasPageUrl(repo)` → `/r/{repo}/~canvas` — V3.4-C2 working-set canvas.
/// Query state (`id`/`review`) is owned by `lib/canvasUrl.ts`.
export function canvasPageUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~canvas`;
}

/// `browserPageUrl(repo)` → `/r/{repo}/~browser` — V3.4-C3 symbol-first browser.
/// Query state (`symbol`) is owned by `lib/browserUrl.ts`.
export function browserPageUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/~browser`;
}

// --- Phase C7 ("story mode") -----------------------------------------------
//
// `~story` is a FILE-scoped sentinel — unlike `~commit`/`~compare`/
// `~branches` above (repo-scoped, fixed position, their own static
// `<Route>`), it trails an arbitrary file path the same way `~diff` does
// (`breadcrumbs.ts`'s `diffUrl`), so it rides Reader's own splat parsing
// rather than getting a dedicated route — see `Reader.tsx`'s
// `STORY_SENTINEL`.

const STORY_SEGMENT = "~story";

/// `storyUrl(repo, path, sha?)` → `/r/{repo}/{path}/~story[?at=<sha>]` — the
/// file's commit-by-commit playback view. `sha` pins the player's initial
/// step (the History tab's per-row "open story here" link); omitted starts
/// at the file's OLDEST commit ("watch this file being made," from the
/// beginning).
export function storyUrl(repo: string, path: string, sha?: string): string {
  const base = `${codeBasePath(repo, path)}/${STORY_SEGMENT}`;
  return sha ? `${base}?at=${encodeURIComponent(sha)}` : base;
}

// --- T1 — `?sym=` deep links (design-nav.md §4 / design-ui.md §5) ----------
//
// A `sym=` link names a symbol independent of its CURRENT line —
// `GET /api/resolve-symbol` (`crates/kb-code-server/src/symbol_addr.rs`)
// re-resolves it fresh against the live `symbols` table (or `rails_edges`
// for the `rails:` namespace) on every load, so the link survives code
// motion/reformatting within the same defining scope. This module only
// BUILDS the string + the carrying URL; resolution + the found/not-found
// landing happens in `Reader.tsx`'s on-load effect.

/// Grammar: `<namespace>:<container>:<name>[:<kind>]`, or the no-container
/// short form `<namespace>:<name>` — mirrors `symbol_addr.rs`'s
/// `parse_sym`/`split_sym_fields` field-for-field, INCLUDING that module's
/// documented limitation: a symbol with an empty-but-present container is
/// NOT the same thing as no container at all, so an absent/empty
/// `container` always collapses to the 2-field short form here (never a
/// 3-field form with an empty middle segment) — the one shape that module's
/// own doc says client builders are expected to emit.
export interface SymGrammarInput {
  namespace: string;
  name: string;
  container?: string | null;
  /// Only meaningful (and only ever emitted) alongside a non-empty
  /// `container` — the 2-field short form has no kind slot in the grammar.
  kind?: string | null;
}

export function buildSym({ namespace, name, container, kind }: SymGrammarInput): string {
  if (!container) return `${namespace}:${name}`;
  return kind ? `${namespace}:${container}:${name}:${kind}` : `${namespace}:${container}:${name}`;
}

/// Extension → `lang::LangInfo.id` (`crates/kb-code-server/src/lang.rs`'s
/// `detect`), mirrored client-side so a peek/structure row can build an
/// honest `sym=` namespace for a file that isn't necessarily the one
/// currently open (so the server's own `FileOut.lang` isn't always at
/// hand). Deliberately narrower than the server's `lang::detect` — no
/// shebang sniff, since a row never carries file bytes here — `null` for
/// anything unrecognized, same "no grammar for this file" contract that fn
/// documents. Kept in lock-step with `lang.rs`'s `detect` match arms by
/// hand (same discipline `wikilink.ts`/`memento.ts` use for their own
/// Rust-mirrored grammars — see kb's own CLAUDE.md invariant #29/#14).
const EXT_TO_LANG_ID: Readonly<Record<string, string>> = {
  rs: "rust",
  py: "python",
  rb: "ruby",
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "tsx",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  sh: "bash",
  bash: "bash",
  yml: "yaml",
  yaml: "yaml",
  go: "go",
  toml: "toml",
  json: "json",
  erb: "erb",
};

export function langIdForPath(path: string): string | null {
  const dot = path.lastIndexOf(".");
  if (dot === -1 || dot === path.length - 1) return null;
  const ext = path.slice(dot + 1).toLowerCase();
  return EXT_TO_LANG_ID[ext] ?? null;
}

export interface SymbolUrlOpts {
  /// Where to land BEFORE `sym=` resolves (and where `Reader.tsx` falls
  /// back to on a `found: false` miss) — the row/popup's own CURRENT
  /// path, so the link is never a dead end even against an older daemon
  /// that doesn't understand `sym=` at all.
  fallbackPath: string;
  fallbackLine?: LineSel;
}

/// `symbolUrl(repo, sym, {fallbackPath, fallbackLine})` — a `?sym=`-carrying
/// reader URL. `sym` is appended LAST (the `pane2` precedent above — see
/// this module's header doc), after the fallback `line=`, so an existing
/// bookmarked/shared link's param ORDER discipline extends rather than
/// breaks: every current `codeUrl`/`formatPane2` golden stays byte-for-byte
/// unchanged (this is a new, separate builder, not an edit to `codeUrl`
/// itself).
export function symbolUrl(repo: string, sym: string, opts: SymbolUrlOpts): string {
  const base = codeBasePath(repo, opts.fallbackPath);
  const params: string[] = [];
  if (opts.fallbackLine !== undefined) {
    const lp = formatLineParam(opts.fallbackLine);
    if (lp !== "") params.push(`line=${lp}`);
  }
  params.push(`sym=${encodeURIComponent(sym)}`);
  return `${base}?${params.join("&")}`;
}

/// Absolute-URL form of `symbolUrl`, for the copy-symbol-link affordance
/// (peek rows + `StructurePopup` rows) — mirrors `permalinkFor`'s exact
/// trailing-slash trim so a `sym=` link copied out of the reader is stable
/// pasted anywhere (chat, an agent CLI, another daemon's clipboard), not
/// just valid as a relative in-app href.
export function symbolPermalinkFor(origin: string, repo: string, sym: string, opts: SymbolUrlOpts): string {
  const trimmedOrigin = origin.endsWith("/") ? origin.slice(0, -1) : origin;
  return `${trimmedOrigin}${symbolUrl(repo, sym, opts)}`;
}

// --- V70-A6 — trail linkage (`trail=`/`step=`/`via=`) ---------------------
//
// The Ramp's two "open elsewhere" rungs (`ramp.open-tab` / `ramp.open-window`,
// `nav/ramp.ts`) hand the destination three extra params so a fresh tab can
// say where it came from and `u` can walk back:
//
//   `trail=<id>`   the reading trail this hop belongs to (browser-local in
//                  v7.0 — `lib/trail.ts`; kbc-trail/1 server trails are v7.4)
//   `step=<n>`     0-based ordinal of the hop INSIDE that trail
//   `via=<kind>`   the typed edge that was traversed (`usage_of`,
//                  `definition_of`, …) — the closed `TrailVia` vocabulary
//
// Three rules make this additive rather than a grammar change:
//
//   1. They are appended LAST, after `sym` (which is itself appended after
//      `pane2`, which is appended after `line`) — the same precedent this
//      module's header doc records, so EVERY pre-A6 golden is byte-identical.
//   2. `appendTrail` operates on a finished URL STRING, so it composes with
//      every builder here (`codeUrl`, `reviewDiffHref`, `storyUrl`, the page
//      builders) without any of them growing a `trail` option.
//   3. `parseTrailLink` is total — a missing/garbage `trail`/`step` yields
//      `null`, never a partially-populated link, and an unknown `via` is
//      dropped rather than fabricated (the destination then renders the hop
//      with no edge label, which is honest: we know WHERE from, not WHY).

/// The closed `via` vocabulary — the typed edge a reader traversed to get
/// here (research §3.2's `via`, narrowed to what v7.0 can actually mint).
/// Mirrored by `lib/trail.ts`'s `TRAIL_VIA` runtime list; the two are kept in
/// lock-step by `codeUrl.test.ts`.
export type TrailVia =
  | "search"
  | "definition_of"
  | "usage_of"
  | "caller_of"
  | "blame"
  | "why"
  | "story"
  | "review"
  | "framework"
  | "bookmark"
  | "tree"
  | "manual";

const TRAIL_VIA_SET: ReadonlySet<string> = new Set<TrailVia>([
  "search",
  "definition_of",
  "usage_of",
  "caller_of",
  "blame",
  "why",
  "story",
  "review",
  "framework",
  "bookmark",
  "tree",
  "manual",
]);

export interface TrailLink {
  id: string;
  /// 0-based ordinal of this hop within the trail.
  step: number;
  via?: TrailVia;
}

/// Append `trail`/`step`/`via` to an already-built URL from any builder in
/// this module. `link` absent/empty ⇒ the URL is returned UNCHANGED (the
/// byte-identical rule above). Never emits a partial link: an id is
/// required, `step` is clamped to a non-negative integer, and `via` is
/// omitted unless it names a real `TrailVia`.
export function appendTrail(url: string, link: TrailLink | null | undefined): string {
  if (!link || !link.id) return url;
  const step = Number.isFinite(link.step) ? Math.max(0, Math.floor(link.step)) : 0;
  const params = [`trail=${encodeURIComponent(link.id)}`, `step=${step}`];
  if (link.via && TRAIL_VIA_SET.has(link.via)) params.push(`via=${encodeURIComponent(link.via)}`);
  const sep = url.includes("?") ? "&" : "?";
  return `${url}${sep}${params.join("&")}`;
}

/// Read a trail link back off a URL's query params. Total: `null` whenever
/// `trail` is absent/empty or `step` is not a non-negative integer (a
/// half-link is not a link — the destination would render an origin chip
/// pointing at nothing).
export function parseTrailLink(params: URLSearchParams): TrailLink | null {
  const id = params.get("trail");
  if (!id) return null;
  const rawStep = params.get("step");
  const step = rawStep === null ? NaN : Number(rawStep);
  if (!Number.isFinite(step) || !Number.isInteger(step) || step < 0) return null;
  const rawVia = params.get("via");
  const via = rawVia !== null && TRAIL_VIA_SET.has(rawVia) ? (rawVia as TrailVia) : undefined;
  return via ? { id, step, via } : { id, step };
}

/// Strip the three trail params from a URL — what the Location Contract's
/// encoder does before comparing two locations (`nav/location.ts`), so a hop
/// that differs ONLY in its trail linkage is still the same place.
export function stripTrail(url: string): string {
  const q = url.indexOf("?");
  if (q === -1) return url;
  // Filter the RAW `&`-separated pairs rather than round-tripping through
  // `URLSearchParams` — its `toString()` re-encodes (`%20`→`+`, and it
  // normalises every other value too), which would make a stripped URL
  // differ from the builder's own output in bytes that carry no meaning.
  // This function feeds an EQUALITY test (`nav/location.ts`), so preserving
  // the original encoding exactly is the whole point.
  const kept = url
    .slice(q + 1)
    .split("&")
    .filter((pair) => {
      const eq = pair.indexOf("=");
      const key = eq === -1 ? pair : pair.slice(0, eq);
      return key !== "trail" && key !== "step" && key !== "via";
    });
  return kept.length > 0 ? `${url.slice(0, q)}?${kept.join("&")}` : url.slice(0, q);
}
