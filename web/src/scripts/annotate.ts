// kb annotator — runs INSIDE the artifact subdomain when the SPA toggles
// annotate mode and the daemon injects this script via ?cm=on.
//
// HARD CONSTRAINTS:
//   - vanilla DOM only (no React, no SPA imports — kept by Vite's
//     separate build entry; bundle MUST stay under 12 KiB — CI's
//     bundle-size guard fails the build otherwise). Comments are free:
//     esbuild strips every one of them, so the rationale below costs
//     nothing against that budget.
//   - reads window.__KB_COMMENTS (injected by the daemon's
//     iframe::inject_annotator before this script runs; defer ordering
//     in the spec guarantees it)
//   - all writes go through the parent SPA via postMessage; this script
//     never POSTs to the daemon directly
//
// postMessage protocol (see docs in CommentsPanel.tsx + AnnotatorBridge):
//   iframe → parent  (targetOrigin '*'; parent validates event.origin)
//     {type:"cm:probe",        origin}
//     {type:"cm:compose",      anchor, file, fileLabel}  // body composed in panel
//     {type:"cm:focus",        commentId}           // clicked an in-page marker
//     {type:"cm:exit-annotate"}                     // Esc inside iframe
//     {type:"cm:selection", anchor, rect, scroll_y}  // W2.16 — mode-INDEPENDENT;
//     {type:"cm:selection-clear"}                    // fires on mouseup OR
//       selectionchange (debounced 150ms; selectionchange is suppressed
//       while a MOUSE pointer is down — see onSelectionChange below).
//       mouseup alone misses iOS Safari / Android Chrome / Firefox-for-
//       Android selection-grip drags, which fire only selectionchange once
//       the long-press has started.
//     {type:"cm:selection-pull",  anchor, rect}      // reply to cm:selection-
//     {type:"cm:selection-pull",  anchor: null}      //   query (exactly one)
//   parent → iframe  (targetOrigin = this iframe's full origin)
//     {type:"cm:mode",      on}
//     {type:"cm:flash",     commentId}         // scroll + pulse
//     {type:"cm:emphasize", commentId, on}     // hover glow, no scroll
//     {type:"cm:refresh",   file}
//     {type:"cm:selection-query"}              // "what is selected NOW?" —
//       the PULL half of the selection protocol. The push relay above is at
//       the mercy of each engine's event timing; this one is driven by an
//       explicit user tap in the parent, so it is immune to all of it.

type Anchor =
  | { kind: "file" }
  | { kind: "chapter"; path: string }
  | { kind: "section"; id: string; tag?: string; snippet?: string }
  | { kind: "selection"; css_path: string; offset: number; snippet: string };

/// The wire body shared by `cm:selection` and `cm:selection-pull` — the
/// serialized anchor plus the selection's bounding rect in THIS iframe's
/// viewport coordinates (the parent's AnnotatorBridge translates it; it is
/// the only side holding the iframe element).
///
/// `scroll_y` is this document's scroll offset AT CAPTURE TIME, and it ships
/// with `cm:selection` because the rect above is viewport-relative and
/// therefore only meaningful at that offset. The parent's scroll beacon
/// (`kb:scroll`) is debounced 500ms, so it routinely arrives AFTER a
/// selection made once the scroll had already settled; comparing that
/// beacon's offset against this one is what tells "the reader scrolled away
/// from their selection" (rect now stale) from "a beacon describing a scroll
/// that predates the selection" (rect still exact). See ArtifactPane's
/// `kb:scroll` branch.
type SelPayload = {
  anchor: Anchor;
  rect: { top: number; left: number; bottom: number };
  scroll_y: number;
};

type Comment = {
  id: string;
  status: "open" | "resolved";
  file: string;
  fileLabel: string;
  anchor: Anchor;
  author: "you" | "claude";
  body: string;
  createdAt: string;
  editedAt: string | null;
  replies: Array<{
    id: string;
    author: "you" | "claude";
    body: string;
    createdAt: string;
  }>;
};

type ReviewFile = {
  schema: string;
  artifact: { id: string; title: string; kb: string };
  generatedAt: string;
  comments: Comment[];
};

type Envelope = {
  v: number;
  etag: string | null;
  file: ReviewFile;
};

declare global {
  interface Window {
    __KB_COMMENTS?: Envelope;
    __KB_MODE?: "view" | "annotate";
    /// X2 — origin of the parent SPA, injected by the daemon. The
    /// annotator only accepts `cm:*` messages from this origin. `"*"`
    /// (dev default) or unset → accept any.
    __KB_PARENT_ORIGIN?: string;
  }
}

// Hand-minified (no whitespace/comments) — this string ships verbatim into
// the bundle and counts against the 10 KiB annotate.js CI budget the same
// as any other byte; the pretty-printed form cost ~1KiB here for zero
// runtime benefit (a <style> tag doesn't care about formatting). Rules,
// selectors and values are unchanged from the pretty-printed original —
// see git history for a diffable prior version if these need editing.
// .kb-annot-hl = the highlight band painted over an anchored comment's
// text (click-through so the reader can still select underneath);
// .kb-annot-icon = the clickable "❝" glyph at its trailing edge (very
// high z-index — sits above the artifact's own content).
const STYLE =
  ".kb-annot-mode-on,.kb-annot-mode-on *{cursor:crosshair!important}" +
  ".kb-annot-marker{position:absolute;left:0;width:4px;background:#b58900;border-radius:2px;pointer-events:auto;cursor:pointer;opacity:.8;user-select:none;-webkit-user-select:none}" +
  ".kb-annot-marker.kb-annot-marker--resolved{background:#93a1a1;opacity:.45}" +
  ".kb-annot-marker.kb-annot-marker--stale{background:#dc322f}" +
  ".kb-annot-marker.kb-annot-active{width:6px;opacity:1}" +
  ".kb-annot-hl{position:absolute;pointer-events:none;border-radius:2px;background:rgba(181,137,0,.3);box-shadow:inset 0 -2px 0 rgba(181,137,0,.85);transition:background .15s ease,box-shadow .15s ease}" +
  ".kb-annot-hl.kb-annot-hl--resolved{background:rgba(147,161,161,.18);box-shadow:inset 0 -2px 0 rgba(147,161,161,.6)}" +
  ".kb-annot-hl.kb-annot-active{background:rgba(181,137,0,.5);box-shadow:inset 0 -2px 0 rgba(181,137,0,1)}" +
  ".kb-annot-icon{position:absolute;width:16px;height:16px;display:flex;align-items:center;justify-content:center;font-size:11px;line-height:1;background:#b58900;color:#fff;border-radius:50% 50% 50% 2px;pointer-events:auto;cursor:pointer;opacity:.85;z-index:2147483646;user-select:none;-webkit-user-select:none;box-shadow:0 1px 2px rgba(0,0,0,.3);transition:opacity .15s ease,transform .15s ease}" +
  ".kb-annot-icon:hover{opacity:1;transform:scale(1.1)}" +
  ".kb-annot-icon.kb-annot-icon--resolved{background:#93a1a1;opacity:.55}" +
  ".kb-annot-icon.kb-annot-active{opacity:1;outline:2px solid #fff}" +
  ".kb-annot-flash{animation:kbflash 1.2s ease-out 1}" +
  "@keyframes kbflash{0%,50%{box-shadow:0 0 0 4px #b58900}100%{box-shadow:0 0 0 0 transparent}}" +
  // Touch ergonomics — a fingertip is far less precise than a mouse
  // cursor, so the 16px icon / 4-6px marker (sized for a desktop pointer)
  // are near-unhittable on a phone. `pointer:coarse` is true for
  // touchscreens (and false for a mouse/trackpad, incl. hybrid laptops),
  // so this only widens targets where the imprecision is real.
  "@media(pointer:coarse){.kb-annot-icon{width:24px;height:24px;font-size:14px}.kb-annot-marker{width:6px}.kb-annot-marker.kb-annot-active{width:8px}}";

const KB_MARKER_CLASS = "kb-annot-marker";
const KB_HL_CLASS = "kb-annot-hl";
const KB_ICON_CLASS = "kb-annot-icon";

const env: Envelope | undefined = window.__KB_COMMENTS;
// Last painted set, kept so a window resize (which reflows the artifact
// and shifts every rect) can re-place markers + highlight bands.
let lastComments: Comment[] = [];
let resizeTimer: ReturnType<typeof setTimeout> | null = null;
// W2.16 — cm:selection debounce + "did we last send a selection" (so a
// mouseup that lands on a collapsed selection sends exactly one
// cm:selection-clear, not a clear per empty-selection mouseup).
let selTimer: ReturnType<typeof setTimeout> | null = null;
let selSent = false;
// MOUSE-ONLY suppression — true between a `pointerType === "mouse"`
// pointerdown and the next pointerup/pointercancel. selectionchange fires
// continuously while a mouse drag is in progress (desktop): without this
// guard onSelectionChange would schedule dozens of redundant timers per
// drag, and — worse — could post/paint an in-progress mid-drag selection to
// the parent before mouseup ever fires. mouseup already finalizes the
// desktop drag, so selectionchange is suppressed for its whole duration.
//
// Why touch/pen NEVER engage it (the Gecko fix): whether a browser completes
// the pointer lifecycle at the end of a plain-text long-press selection is
// unspecified and, on Gecko, undocumented — Firefox gates its touch-
// completion events on "Firefox's own context menu actually opened"
// (Bugzilla 1481923), and the AccessibleCaret selection toolbar is a
// separate path again. So a touch pointerdown with no matching pointerup /
// pointercancel is a real possibility, and under the old unconditional flag
// that wedged `pointerDown` true FOREVER, permanently killing the
// selectionchange relay for the life of the page. Gating the SET on
// pointerType makes the wedge structurally impossible: touch never arms the
// suppression, so it can never fail to disarm. (The resets stay
// unconditional — cheap, and they keep a mouse lifecycle that ends in
// pointercancel honest.)
//
// The cost of not suppressing touch is mid-gesture selectionchange spam,
// which is harmless here: the 150ms debounce coalesces it, and the mobile
// surface the posts drive is a POSITION-FIXED bottom bar, so repeated posts
// cause no visual churn. (The desktop floater is rect-anchored — it would
// jitter across the screen mid-drag — which is exactly why mouse stays
// suppressed.)
let pointerDown = false;
// R2 — the tap race. Gecko collapses a selection BEFORE the `click` that
// caused it (Chromium collapses later; the ordering is a W3C-documented
// divergence, not a bug in either). With a 150ms clear the parent's mobile
// action bar unmounts under the user's own finger and the tap lands on
// nothing. On a coarse pointer the clear therefore waits a second beat
// (150 + 450 ≈ 600ms total) and re-reads the selection first — long enough
// to cover any realistic tap-through, short enough that a genuine dismiss
// still feels immediate. Fine pointers are byte-identical to before.
const COARSE =
  typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches;
const SLOW_CLEAR_MS = 450;
// R4 — the last selection we actually read, kept so `cm:selection-query`
// can answer even after the engine has already collapsed the live range (the
// R2 race again, seen from the pull side: by the time the user has reached
// the panel button, `window.getSelection()` may be empty through no fault of
// theirs). 60s is "the selection the user is still thinking about"; anything
// older is stale enough that answering with it would be a lie.
let lastSel: SelPayload | null = null;
let lastSelAt = 0;
const SEL_CACHE_MS = 60_000;

if (env && env.file && Array.isArray(env.file.comments)) {
  init(env);
}

// Every iframe→parent message is targetOrigin '*' (the parent validates
// event.origin on its side — see the header comment); one helper instead
// of repeating `parent.postMessage(x, "*")` at each of the 7 call sites.
function post(msg: unknown) {
  parent.postMessage(msg, "*");
}

function init(envelope: Envelope) {
  injectStyle();
  paintAll(envelope.file.comments);
  window.addEventListener("message", onParentMessage);
  document.addEventListener("click", onClick, true);
  document.addEventListener("keydown", onKeyDown, true);
  document.addEventListener("mouseup", onMouseUp);
  // Mobile fix (see the `pointerDown` declaration above + onSelectionChange
  // below) — selectionchange is the ONLY event a gripper-handle drag fires
  // on iOS Safari / Android Chrome / Firefox for Android, so it must
  // independently trigger the same relay mouseup does; pointerdown/up just
  // bookend the desktop MOUSE-drag suppression window.
  document.addEventListener("selectionchange", onSelectionChange);
  document.addEventListener("pointerdown", (e) => {
    // MOUSE ONLY — a touch/pen pointerdown must never arm the suppression,
    // because we cannot rely on this engine ever completing that pointer's
    // lifecycle after a native selection gesture (see the `pointerDown`
    // declaration). No arm ⇒ no possible wedge.
    if (e.pointerType === "mouse") pointerDown = true;
  });
  // Both resets stay UNCONDITIONAL: they can only ever clear a flag that a
  // mouse set, and a stuck-false flag is harmless where a stuck-true one is
  // fatal.
  document.addEventListener("pointerup", () => {
    pointerDown = false;
  });
  // A gesture the browser takes over (long-press→native selection, or a
  // scroll) ends in pointercancel, not pointerup.
  document.addEventListener("pointercancel", () => {
    pointerDown = false;
  });
  window.addEventListener("resize", onResize);
  // Tell the parent we're alive.
  post({ type: "cm:probe", origin: location.origin });
}

function onResize() {
  if (resizeTimer) clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => paintAll(lastComments), 150);
}

function injectStyle() {
  const s = document.createElement("style");
  s.textContent = STYLE;
  document.head.appendChild(s);
}

function paintAll(comments: Comment[]) {
  lastComments = comments;
  // Drop existing markers + highlight bands + icons (re-paints on
  // cm:refresh / resize).
  document
    .querySelectorAll(
      "." + KB_MARKER_CLASS + ",." + KB_HL_CLASS + ",." + KB_ICON_CLASS,
    )
    .forEach((el) => el.remove());
  for (const c of comments) {
    const target = resolveAnchor(c.anchor);
    if (target) addMarker(target, c);
    const rects = rectsFor(c, target);
    for (const rect of rects) addHighlight(rect, c);
    // One clickable glyph per comment, at the trailing edge of its last
    // line. File-scope comments have no rects (the gutter marker spans
    // the doc) so they get no inline icon.
    if (target && rects.length) addIcon(rects[rects.length - 1], c);
  }
}

function addMarker(target: Element, comment: Comment) {
  const mark = document.createElement("div");
  let cls = KB_MARKER_CLASS;
  if (comment.status === "resolved") cls += " kb-annot-marker--resolved";
  mark.className = cls;
  mark.title = `${comment.author}: ${comment.body.slice(0, 80)}`;
  mark.dataset.kbCommentId = comment.id;
  const rect = target.getBoundingClientRect();
  mark.style.top = `${window.scrollY + rect.top}px`;
  mark.style.height = `${rect.height}px`;
  // Clicking a margin marker asks the parent to focus that comment in
  // the panel. In annotate mode the capture-phase `onClick` swallows the
  // event (it's authoring a new comment), so this fires only in view
  // mode — exactly when the panel may be closed and needs opening.
  mark.addEventListener("click", () => {
    post({ type: "cm:focus", commentId: comment.id });
  });
  document.body.appendChild(mark);
}

// Yellow highlight bands over the anchored text. `target` is the
// already-resolved anchor element (avoids re-resolving). Selection
// anchors get one band per wrapped line of the matched snippet; other
// scopes get the element's box. File scope paints no band (the marker
// already spans the whole document) — returns [].
function rectsFor(c: Comment, target: Element | null): DOMRect[] {
  if (!target || c.anchor.kind === "file") return [];
  if (c.anchor.kind === "selection" && c.anchor.snippet) {
    const range = rangeForSnippet(target, c.anchor.snippet);
    if (range) {
      const rects = Array.from(range.getClientRects()).filter(
        (r) => r.width > 0 && r.height > 0,
      );
      if (rects.length) return rects;
    }
  }
  return [target.getBoundingClientRect()];
}

function addHighlight(rect: DOMRect, comment: Comment) {
  const hl = document.createElement("div");
  let cls = KB_HL_CLASS;
  if (comment.status === "resolved") cls += " kb-annot-hl--resolved";
  hl.className = cls;
  hl.dataset.kbCommentId = comment.id;
  hl.style.left = `${window.scrollX + rect.left}px`;
  hl.style.top = `${window.scrollY + rect.top}px`;
  hl.style.width = `${rect.width}px`;
  hl.style.height = `${rect.height}px`;
  document.body.appendChild(hl);
}

// The clickable comment glyph anchored to the trailing edge of `rect`
// (the comment's last highlighted line). Carries data-kb-comment-id so
// the cm:emphasize / cm:flash paths glow + pulse it alongside the band.
// Clicking asks the parent to focus the comment (which opens the panel +
// activates the row); the capture-phase onClick skips our overlay nodes,
// so this works in both view and annotate mode.
function addIcon(rect: DOMRect, comment: Comment) {
  const icon = document.createElement("div");
  let cls = KB_ICON_CLASS;
  if (comment.status === "resolved") cls += " kb-annot-icon--resolved";
  icon.className = cls;
  icon.dataset.kbCommentId = comment.id;
  icon.title = `${comment.author}: ${comment.body.slice(0, 80)}`;
  icon.textContent = "❝";
  const maxLeft = window.scrollX + document.documentElement.clientWidth - 18;
  icon.style.left = `${Math.min(window.scrollX + rect.right + 2, maxLeft)}px`;
  icon.style.top = `${window.scrollY + rect.top - 2}px`;
  icon.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    post({ type: "cm:focus", commentId: comment.id });
  });
  document.body.appendChild(icon);
}

// Locate `snippet` inside `root`'s text and return a Range spanning it.
// Whitespace is collapsed for matching (the stored snippet came from
// Selection.toString(), whose whitespace can differ from the raw DOM).
// Returns null when the text can't be found (e.g. after a regen).
function rangeForSnippet(root: Element, snippet: string): Range | null {
  const needle = snippet.replace(/\s+/g, " ").trim();
  if (!needle) return null;
  const tw = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let raw = "";
  const owner: Text[] = []; // owner[i] / pos[i] = node + offset for raw[i]
  const pos: number[] = [];
  let n: Node | null;
  while ((n = tw.nextNode())) {
    const t = (n as Text).data;
    for (let i = 0; i < t.length; i++) {
      raw += t[i];
      owner.push(n as Text);
      pos.push(i);
    }
  }
  // Normalised string + map back to raw indices.
  let norm = "";
  const toRaw: number[] = [];
  let prevSpace = false;
  for (let i = 0; i < raw.length; i++) {
    const ws = /\s/.test(raw[i]);
    if (ws) {
      if (prevSpace) continue;
      norm += " ";
      toRaw.push(i);
      prevSpace = true;
    } else {
      norm += raw[i];
      toRaw.push(i);
      prevSpace = false;
    }
  }
  const at = norm.indexOf(needle);
  if (at < 0) return null;
  const s = toRaw[at];
  const e = toRaw[Math.min(at + needle.length - 1, toRaw.length - 1)];
  const range = document.createRange();
  range.setStart(owner[s], pos[s]);
  range.setEnd(owner[e], pos[e] + 1);
  return range;
}

function resolveAnchor(a: Anchor): Element | null {
  if (a.kind === "file") return document.body;
  if (a.kind === "chapter") return findByHeadingPath(a.path);
  if (a.kind === "section") {
    return (
      document.getElementById(a.id) ||
      document.querySelector(`[data-kb-id="${cssEscape(a.id)}"]`) ||
      findBySyntheticId(a.id)
    );
  }
  if (a.kind === "selection") {
    try {
      return document.querySelector(a.css_path);
    } catch {
      return null;
    }
  }
  return null;
}

function findByHeadingPath(path: string): Element | null {
  const parts = path.split(">").map((s) => s.trim()).filter(Boolean);
  if (parts.length === 0) return null;
  const headings = document.querySelectorAll("h1, h2, h3, h4, h5, h6");
  const active: { level: number; el: Element; text: string }[] = [];
  for (const h of Array.from(headings)) {
    const level = parseInt(h.tagName[1], 10);
    while (active.length && active[active.length - 1].level >= level) {
      active.pop();
    }
    const text = (h.textContent || "").trim();
    active.push({ level, el: h, text });
    if (
      active.length === parts.length &&
      active.every((a, i) => a.text === parts[i])
    ) {
      return h;
    }
  }
  return null;
}

function findBySyntheticId(id: string): Element | null {
  // Synthetic ids have shape `<heading-slug>-<tag>-<nth>` (1-indexed).
  const m = /^(.+)-([a-z]+)-(\d+)$/.exec(id);
  if (!m) return null;
  const headingSlug = m[1];
  const tag = m[2];
  const nth = parseInt(m[3], 10) - 1;
  const headings = document.querySelectorAll("h1, h2, h3, h4, h5, h6");
  let anchorH: Element | null = null;
  for (const h of Array.from(headings)) {
    if (slugify(h.textContent || "") === headingSlug) {
      anchorH = h;
      break;
    }
  }
  if (!anchorH) return null;
  const matches: Element[] = [];
  let node: Element | null = anchorH.nextElementSibling;
  while (node) {
    if (/^h[1-6]$/i.test(node.tagName)) break;
    if (node.tagName.toLowerCase() === tag) matches.push(node);
    node = node.nextElementSibling;
  }
  return matches[nth] || null;
}

// Esc in annotate mode backs out to read mode (Vim insert→normal).
// Layer resolution lives in the parent; we just signal "exit".
function onKeyDown(ev: KeyboardEvent) {
  if (ev.key !== "Escape") return;
  if (window.__KB_MODE !== "annotate") return;
  const t = ev.target as HTMLElement | null;
  if (t?.isContentEditable) return;
  ev.preventDefault();
  window.getSelection()?.removeAllRanges();
  post({ type: "cm:exit-annotate" });
}

function onClick(ev: MouseEvent) {
  // Never treat a click on our own overlay affordances (comment icon /
  // gutter marker) as authoring a new comment — let their own handlers
  // post cm:focus instead, even while annotate mode is on. This capture-
  // phase listener runs before their bubble-phase handlers, so a plain
  // stopPropagation in those wouldn't be enough; we bail out here.
  const overlay = ev.target as Element | null;
  if (overlay?.closest?.("." + KB_ICON_CLASS + ",." + KB_MARKER_CLASS)) return;
  if (window.__KB_MODE !== "annotate") return;
  ev.preventDefault();
  ev.stopPropagation();
  const target = ev.target as Element;
  const anchor = pickAnchor(target);
  const fileId = env?.file?.artifact?.id || location.host.split(".")[0];
  // Body composition happens in the parent SPA panel (the shared tabbed
  // editor). We send only the anchor; the panel builds + saves the comment.
  post({ type: "cm:compose", anchor, file: fileId, fileLabel: "main" });
}

// Shared by pickAnchor (click-to-compose) and the mode-independent
// cm:selection capture below (W2.16) — one selection-anchor ladder, not two.
// Returns the Range alongside the Anchor so the capture path can also pull
// a bounding rect without re-querying window.getSelection().
function selInfo(): { anchor: Anchor; range: Range } | null {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0 || sel.isCollapsed) return null;
  const range = sel.getRangeAt(0);
  return {
    range,
    anchor: {
      kind: "selection",
      css_path: cssPath(range.startContainer.parentElement || document.body),
      offset: range.startOffset,
      snippet: sel.toString().slice(0, 200),
    },
  };
}

function pickAnchor(target: Element): Anchor {
  // 1. Selection scope if there's a non-collapsed range.
  const si = selInfo();
  if (si) return si.anchor;
  // 2. Chapter scope when clicking a heading or summary.
  const headEl = target.closest("h1, h2, h3, h4, h5, h6, summary");
  if (headEl) {
    return { kind: "chapter", path: headingPath(headEl) };
  }
  // 3. Section scope, real id if available.
  const realId = target.closest("[id], [data-kb-id]");
  if (realId) {
    const id =
      realId.getAttribute("id") || realId.getAttribute("data-kb-id") || "";
    return {
      kind: "section",
      id,
      tag: realId.tagName.toLowerCase(),
      snippet: textSnippet(realId),
    };
  }
  // 4. Section scope, synthetic id from heading-slug + tag + nth.
  return synthSectionAnchor(target);
}

// W2.16 — mode-independent selection capture (cite / add-to-list), fires
// regardless of window.__KB_MODE. Debounced 150ms off mouseup AND
// selectionchange (mobile fix, see below) so a drag selection settles
// before posting; unrelated to (and doesn't touch) the annotate-mode
// click→cm:compose path above. `selSent` skips a redundant clear on every
// ordinary (non-selecting) click/tap.
//
// Two triggers, one shared debounce (scheduleSelPost): mouseup covers
// desktop drag-to-select and the INITIAL synthetic mouseup a mobile
// long-press selection fires. Neither fires again as the user then drags
// the native selection grip handles to refine the range — iOS Safari,
// Android Chrome and Firefox for Android emit ONLY selectionchange for that
// gesture (no further mouse/touch/pointer events at all), so a mouseup-only
// relay silently drops every mobile refinement and posts a stale anchor.
// selectionchange closes that gap; pattern + suppression approach borrowed
// from the Hypothesis client's selection-observer. On Gecko even the
// SYNTHETIC mouseup is not guaranteed (its touch-completion events are
// gated on Firefox's own context menu having opened — Bugzilla 1481923),
// which is why the selectionchange lane must never be suppressed on touch
// and why the pull lane (`cm:selection-query`) exists at all.
function onMouseUp() {
  scheduleSelPost();
}

// selectionchange also fires continuously mid-drag on desktop (every
// pixel the mouse moves while a button is held extends the Selection).
// Without suppression this would schedule a fresh 150ms timer on nearly
// every animation frame of a drag, each one reading + posting a
// half-made selection; mouseup already finalizes the desktop gesture, so
// selectionchange is a no-op for its whole duration. `pointerDown` is only
// ever armed by a MOUSE pointerdown, so no touch gesture — grip-handle drag
// or otherwise — is ever suppressed here, on any engine.
function onSelectionChange() {
  if (pointerDown) return;
  scheduleSelPost();
}

/// Read the live selection into wire form, refreshing the pull cache. Every
/// non-null read goes through here so the cache can never drift from what
/// was last posted.
function readSel(): SelPayload | null {
  const si = selInfo();
  if (!si) return null;
  const r = si.range.getBoundingClientRect();
  const p: SelPayload = {
    anchor: si.anchor,
    rect: { top: r.top, left: r.left, bottom: r.bottom },
    scroll_y: window.scrollY || document.documentElement.scrollTop || 0,
  };
  lastSel = p;
  lastSelAt = Date.now();
  return p;
}

function scheduleSelPost() {
  if (selTimer) clearTimeout(selTimer);
  selTimer = setTimeout(fireSelPost, 150);
}

/// The debounce's fire path. `slow` marks the SECOND beat of a coarse-pointer
/// clear (see COARSE above) — reached only when the first beat found the
/// selection already gone, and the only state in which we actually post the
/// clear on touch.
function fireSelPost(slow?: boolean) {
  const p = readSel();
  if (p) {
    post({
      type: "cm:selection",
      anchor: p.anchor,
      rect: p.rect,
      scroll_y: p.scroll_y,
    });
    selSent = true;
    return;
  }
  if (!selSent) return;
  if (COARSE && !slow) {
    // R2 — hold the parent's bar up a little longer and look again; a tap on
    // it collapses the selection FIRST on Gecko, so an immediate clear would
    // unmount the very button being tapped.
    selTimer = setTimeout(() => fireSelPost(true), SLOW_CLEAR_MS);
    return;
  }
  post({ type: "cm:selection-clear" });
  selSent = false;
}

function synthSectionAnchor(target: Element): Anchor {
  const block = target.closest("p, li, blockquote, pre, table, figure, div") || target;
  const tag = block.tagName.toLowerCase();
  let walker: Element | null = block;
  let heading: Element | null = null;
  while (walker) {
    let prev: Element | null = walker.previousElementSibling;
    while (prev) {
      if (/^h[1-6]$/i.test(prev.tagName)) {
        heading = prev;
        break;
      }
      prev = prev.previousElementSibling;
    }
    if (heading) break;
    walker = walker.parentElement;
    if (walker === document.body) break;
  }
  const slug = slugify(heading?.textContent || "section");
  // LOW (deep-review): pre-fix this counted `nth` by walking
  // `heading.nextElementSibling` until `node === block`, then bumping
  // on tag match. That only works when block is a SIBLING of the
  // heading. When the block is nested deeper (heading at body level,
  // block inside `<section><div>...<p>`) the walk runs straight past
  // block (which is not in the sibling chain) until it hits the next
  // heading, and `nth` stays at 1 — generating an id that collides
  // with the first sibling-level block of the same tag under the same
  // heading. The synthetic id then resolved to the wrong element.
  //
  // Fix: depth-first walk through the document, starting AFTER the
  // heading, counting elements of the target tag until we hit `block`.
  // Stops at the next heading (the section boundary).
  let nth = 1;
  if (heading) {
    const stopBoundary = (el: Element): boolean =>
      /^h[1-6]$/i.test(el.tagName);
    // TreeWalker visiting all elements; entries before `heading` are
    // skipped by tracking whether we've passed it yet.
    const tw = document.createTreeWalker(
      document.body,
      NodeFilter.SHOW_ELEMENT,
    );
    let passedHeading = false;
    while (tw.nextNode()) {
      const el = tw.currentNode as Element;
      if (!passedHeading) {
        if (el === heading) passedHeading = true;
        continue;
      }
      if (el === block) break;
      if (stopBoundary(el)) break; // next heading = next section
      if (el.tagName.toLowerCase() === tag) nth++;
    }
  }
  return {
    kind: "section",
    id: `${slug}-${tag}-${nth}`,
    tag,
    snippet: textSnippet(block),
  };
}

function headingPath(headEl: Element): string {
  const headings = document.querySelectorAll("h1, h2, h3, h4, h5, h6");
  const stack: { level: number; text: string }[] = [];
  for (const h of Array.from(headings)) {
    const lvl = parseInt(h.tagName[1], 10);
    while (stack.length && stack[stack.length - 1].level >= lvl) stack.pop();
    stack.push({ level: lvl, text: (h.textContent || "").trim() });
    if (h === headEl) break;
  }
  return stack.map((s) => s.text).join(" > ");
}

function cssPath(el: Element): string {
  const parts: string[] = [];
  let cur: Element | null = el;
  while (cur && cur !== document.body && cur.parentElement) {
    const tag = cur.tagName.toLowerCase();
    const sib = Array.from(cur.parentElement.children).filter(
      (c) => c.tagName === cur!.tagName,
    );
    const i = sib.indexOf(cur) + 1;
    parts.unshift(`${tag}:nth-of-type(${i})`);
    cur = cur.parentElement;
  }
  return "body > " + parts.join(" > ");
}

function textSnippet(el: Element): string {
  return ((el.textContent || "").replace(/\s+/g, " ").trim()).slice(0, 200);
}

function slugify(s: string): string {
  let out = "";
  let lastDash = true;
  for (const ch of s) {
    if (/[a-zA-Z0-9]/.test(ch)) {
      out += ch.toLowerCase();
      lastDash = false;
    } else if (!lastDash) {
      out += "-";
      lastDash = true;
    }
  }
  out = out.replace(/^-+|-+$/g, "");
  return out || "section";
}

function cssEscape(s: string): string {
  return s.replace(/[^a-zA-Z0-9-_]/g, (c) => `\\${c}`);
}

// Best-effort: walk up from a marker and open ancestors that hide it
// — closed <details> and inactive ARIA tabpanels. Without this, a jump
// to a comment whose anchor sits inside a closed disclosure or a
// non-default tab is a silent no-op (scrollIntoView on a hidden node).
// Failing silently is fine — custom-class tabs that don't carry ARIA
// semantics fall through and the user clicks manually, same as today.
function revealAncestors(el: Element) {
  let cur: Element | null = el;
  while (cur && cur !== document.documentElement) {
    if (cur.tagName === "DETAILS") {
      const d = cur as HTMLDetailsElement;
      if (!d.open) d.open = true;
    }
    if (cur.getAttribute("role") === "tabpanel" && cur.id) {
      const sel = `[role="tab"][aria-controls="${cssEscape(cur.id)}"]`;
      const tab = document.querySelector(sel);
      if (tab instanceof HTMLElement) tab.click();
    }
    cur = cur.parentElement;
  }
}

function onParentMessage(ev: MessageEvent) {
  // X2 — origin gate. The parent validates the iframe's origin on its
  // side (see the header comment); mirror it here so a different embedder
  // can't drive this annotator (e.g. `cm:refresh` to swap the comment
  // set, `cm:mode` to force annotate mode). In production
  // `__KB_PARENT_ORIGIN` is the SPA's origin; `"*"` (dev) or unset accepts
  // any, keeping `npm run dev` (vite proxy origin differs) working.
  const expected = window.__KB_PARENT_ORIGIN;
  if (expected && expected !== "*" && ev.origin !== expected) return;
  const data = ev.data;
  if (!data || typeof data !== "object" || !("type" in data)) return;
  switch (data.type) {
    case "cm:mode":
      window.__KB_MODE = data.on ? "annotate" : "view";
      document.body.classList.toggle("kb-annot-mode-on", !!data.on);
      break;
    case "cm:flash": {
      const id = data.commentId as string;
      // Scroll the actual anchored content into view (revealing any
      // closed <details> / inactive tabpanel first), then repaint so the
      // bands sit correctly after that reflow, then pulse them.
      const c = lastComments.find((x) => x.id === id);
      const el = c ? resolveAnchor(c.anchor) : null;
      if (el) {
        revealAncestors(el);
        paintAll(lastComments);
        el.scrollIntoView({ behavior: "smooth", block: "center" });
      }
      flashNodes(id);
      break;
    }
    case "cm:emphasize": {
      // Only one comment glows at a time; clear all then set the target.
      document
        .querySelectorAll(".kb-annot-active")
        .forEach((el) => el.classList.remove("kb-annot-active"));
      if (data.on) {
        nodesFor(data.commentId as string).forEach((node) =>
          node.classList.add("kb-annot-active"),
        );
      }
      break;
    }
    case "cm:refresh":
      if (data.file && Array.isArray(data.file.comments)) {
        if (env) env.file = data.file;
        paintAll(data.file.comments);
      }
      break;
    case "cm:selection-query": {
      // The PULL half of the selection protocol (R4). The push relay above
      // depends on each engine firing the events it fires, in the order it
      // fires them — something we can verify on Chromium and WebKit but not
      // on Firefox for Android from CI. This path depends on none of it: the
      // parent asks because the user tapped a button, and we answer once,
      // synchronously, from whatever we can see.
      //
      // Ladder: a LIVE selection wins (it is the truth); else the cached one
      // if it is recent enough to still be what the user means; else an
      // explicit null, so the parent can say "select some text first"
      // instead of silently doing nothing. Exactly one reply either way —
      // the parent has no way to distinguish "no selection" from "dropped
      // message" if we stay silent.
      const live = readSel();
      const cached =
        !live && lastSel && Date.now() - lastSelAt < SEL_CACHE_MS
          ? lastSel
          : null;
      const hit = live || cached;
      post(
        hit
          ? { type: "cm:selection-pull", anchor: hit.anchor, rect: hit.rect }
          : { type: "cm:selection-pull", anchor: null },
      );
      break;
    }
  }
}

// All marker + highlight nodes belonging to one comment.
function nodesFor(id: string): Element[] {
  return Array.from(
    document.querySelectorAll(`[data-kb-comment-id="${cssEscape(id)}"]`),
  );
}

function flashNodes(id: string) {
  nodesFor(id).forEach((node) => {
    node.classList.add("kb-annot-flash");
    setTimeout(() => node.classList.remove("kb-annot-flash"), 1500);
  });
}

export {};
