// V70-A6 — the identifier hover tooltip: the FIRST SPA consumer of hover/1.
//
// `GET /api/hover` shipped in PRR-N5 and, until this unit, had zero callers
// in `web-code/` — the recon found it while looking for the mouse path to a
// definition and found none of that either: `gd`/`K` were keyboard-only, and
// a mouse user had no way to ask "what is this" or "take me there".
//
// FOUR RULES, each one a failure mode this avoids:
//
//   1. **≥500 ms, and never a fetch per mouse move.** CM6's `hoverTooltip`
//      already debounces to `hoverTime`; on top of that every answer is
//      cached per `(path, line, col)`, so re-hovering the same identifier —
//      which happens constantly while reading — costs nothing.
//   2. **An honest empty.** Sourcegraph's own tracker records the failure:
//      a tooltip that silently vanishes when nothing comes back, leaving
//      "guess and check". Here a resolve with no candidate renders the word
//      and says "nothing resolves this", which is a different and useful
//      fact from "this is not an identifier" (for which no tooltip opens at
//      all).
//   3. **Trust is shown, never implied.** `hover/1` returns `trust: null`
//      when resolve found nothing; the badge is then absent rather than
//      defaulted. `precision: "lsp-live"` is labelled as such — that tier is
//      blob-verified by kb-lip and is the only one allowed to say `exact`
//      from a live provider.
//   4. **Ctrl/Cmd-click goes to the definition; a plain click still places
//      the caret.** The modifier is the whole discrimination — a bare click
//      must never navigate out of a file the reader is in the middle of.
//
// The cache is per EXTENSION INSTANCE, i.e. per open pane, and dies with the
// view (a file/ref switch recreates the view — `CodeView.tsx`'s `blobHash`
// effect), so it can never serve a stale answer for edited bytes.

import { EditorView, hoverTooltip, type Tooltip } from "@codemirror/view";
import type { Extension } from "@codemirror/state";
import { fetchHover } from "../api/client";
import type { HoverOut } from "../api/types";
import { wordAt } from "./vimKeys";

/// How long the pointer must rest on an identifier. 500 ms is the design's
/// number and sits above CM6's 300 ms default deliberately: a code buffer is
/// dense, and a 300 ms tooltip fires while the eye is still travelling.
export const HOVER_DELAY_MS = 500;
/// Bound the per-view cache. A reader hovers dozens of identifiers per file,
/// not thousands, and an unbounded map on a long-lived view is a leak.
export const HOVER_CACHE_MAX = 200;

export interface HoverTooltipOptions {
  /// `null` disables the extension entirely (no repo/path yet).
  getRepo: () => string | null;
  getPath: () => string | null;
  getRef: () => string | undefined;
  /// Ctrl/Cmd-click — go to the definition at this position.
  onGotoDefinition?: (pos: { line: number; col: number; word: string }) => void;
}

function el(tag: string, cls?: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/// Render `hover/1` into the tooltip's DOM. Exported for the unit test, which
/// asserts the honest-empty and trust-absent branches without a browser.
export function renderHoverDom(out: HoverOut | null, word: string): HTMLElement {
  const root = el("div", "kbc-hovertip");
  root.setAttribute("data-kbc-hovertip", "");
  if (!out || (!out.symbol && !out.defsite && !out.framework)) {
    root.appendChild(el("code", "kbc-hovertip__sig", word));
    root.appendChild(
      el("div", "kbc-hovertip__empty", "nothing in this repo resolves this identifier"),
    );
    return root;
  }
  const sym = out.symbol;
  root.appendChild(el("code", "kbc-hovertip__sig", sym?.signature ?? sym?.name ?? word));
  const meta = el("div", "kbc-hovertip__meta");
  if (sym?.kind) meta.appendChild(el("span", "kbc-hovertip__kind", sym.kind));
  if (sym?.container) meta.appendChild(el("span", "kbc-hovertip__container", `in ${sym.container}`));
  if (out.trust) {
    const badge = el("span", `kbc-hovertip__trust kbc-trust-${out.trust}`, out.trust);
    badge.setAttribute("data-kbc-hovertip-trust", out.trust);
    if (out.precision) badge.title = out.precision;
    meta.appendChild(badge);
  }
  if (out.precision === "lsp-live") meta.appendChild(el("span", "kbc-hovertip__live", "live"));
  if (meta.childNodes.length > 0) root.appendChild(meta);
  if (sym?.doc) {
    const doc = sym.doc.split(/\r?\n/).slice(0, 6).join("\n");
    root.appendChild(el("p", "kbc-hovertip__doc", doc));
  }
  // ONE provenance line — where it is defined, or which framework edge
  // named it. Never both stacked: the tooltip is a glance, not a card.
  if (out.defsite) {
    root.appendChild(
      el("div", "kbc-hovertip__prov", `defined at ${out.defsite.path}:${out.defsite.line}`),
    );
  } else if (out.framework?.dst_path) {
    root.appendChild(
      el(
        "div",
        "kbc-hovertip__prov",
        `${out.framework.kind} → ${out.framework.dst_path} (${out.framework.trust})`,
      ),
    );
  }
  root.appendChild(el("div", "kbc-hovertip__keys", "Ctrl-click to go to the definition · K for the full card"));
  return root;
}

export function hoverTooltipExtension(opts: HoverTooltipOptions): Extension {
  const cache = new Map<string, HoverOut | null>();

  async function lookup(repo: string, path: string, line: number, col: number, ref?: string) {
    const key = `${path}\0${ref ?? ""}\0${line}\0${col}`;
    if (cache.has(key)) return cache.get(key) ?? null;
    let out: HoverOut | null = null;
    try {
      out = await fetchHover({ repo, path, line, col, ref });
    } catch {
      // A 400 here means "no identifier at that position" (`resolve_position`
      // 400s on that, `hover.rs`'s doc) — a normal answer for a hover over
      // punctuation, not an error to surface.
      out = null;
    }
    if (cache.size >= HOVER_CACHE_MAX) cache.clear();
    cache.set(key, out);
    return out;
  }

  const tip = hoverTooltip(
    async (view, pos): Promise<Tooltip | null> => {
      const repo = opts.getRepo();
      const path = opts.getPath();
      if (!repo || !path) return null;
      const lineObj = view.state.doc.lineAt(pos);
      const col = pos - lineObj.from;
      const w = wordAt(lineObj.text, col);
      // Only identifiers get a tooltip — hovering punctuation or whitespace
      // must produce nothing, not an empty box.
      if (!w) return null;
      const out = await lookup(repo, path, lineObj.number, w.start, opts.getRef());
      return {
        pos: lineObj.from + w.start,
        end: lineObj.from + w.end,
        above: true,
        create: () => ({ dom: renderHoverDom(out, w.word) }),
      };
    },
    { hoverTime: HOVER_DELAY_MS },
  );

  const click = EditorView.domEventHandlers({
    mousedown(event, view) {
      // Rule 4 — the modifier IS the discrimination. Without it the event is
      // left entirely alone so CM6 places the caret as usual.
      if (!(event.ctrlKey || event.metaKey) || event.button !== 0) return false;
      const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
      if (pos === null) return false;
      const lineObj = view.state.doc.lineAt(pos);
      const w = wordAt(lineObj.text, pos - lineObj.from);
      if (!w) return false;
      event.preventDefault();
      opts.onGotoDefinition?.({ line: lineObj.number, col: w.start, word: w.word });
      return true;
    },
  });

  return [tip, click];
}
