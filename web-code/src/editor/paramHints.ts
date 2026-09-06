// Param-name inlay hints for the read-only CM6 reader (V3.1-H3a).
//
// For visible call sites whose callee resolves exact-or-likely AND whose
// signature parses into named params, decorate literal-ish arguments with a
// subtle `name:` widget BEFORE the arg. Pref-gated (default ON); debounced
// per viewport like occurrenceHighlight. Hard cap 30 resolve calls per
// viewport render; LRU cache keyed by (path, callee name).

import { StateEffect, StateField, type Extension } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  ViewPlugin,
  WidgetType,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { fetchResolve } from "../api/client";
import type { ResolveCandidate } from "../api/types";

export const PARAM_HINTS_DEBOUNCE_MS = 350;
/** Hard cap on resolve calls per viewport recompute. */
export const PARAM_HINTS_RESOLVE_CAP = 30;
const LRU_CAP = 64;

// --- pure: signature param name extraction --------------------------------

/**
 * Extract parameter NAMES from common signature shapes:
 * - Rust: `fn f(a: T, b: U)` / `pub fn f(mut a: T)`
 * - Python: `def f(a, b=1)` / `def f(self, a: int)`
 * - JS/TS: `function f(a, b)` / `(a: T, b: U) =>` / `f(a, b)`
 *
 * Returns `null` when the signature is unparseable or has no named params.
 */
export function parseSignatureParamNames(signature: string | null | undefined): string[] | null {
  if (!signature) return null;
  const s = signature.trim();
  if (!s) return null;

  // Find the first top-level `(…)` that looks like a param list.
  const open = findParamListOpen(s);
  if (open < 0) return null;
  const close = findMatchingParen(s, open);
  if (close < 0) return null;
  const inner = s.slice(open + 1, close).trim();
  if (inner === "") return []; // zero-arg — not useful for hints

  const parts = splitTopLevelCommas(inner);
  const names: string[] = [];
  for (const part of parts) {
    const name = paramNameFromPart(part.trim());
    if (!name) return null; // unparseable slot → abandon
    // Skip receiver-ish first params that aren't useful as arg labels.
    if (names.length === 0 && (name === "self" || name === "cls" || name === "this")) {
      continue;
    }
    names.push(name);
  }
  return names.length > 0 ? names : null;
}

function findParamListOpen(s: string): number {
  // Prefer the paren after `fn name` / `def name` / `function name`.
  const m = /\b(?:fn|def|function|method)\s+[A-Za-z_][\w']*\s*\(/.exec(s);
  if (m) return m.index + m[0].length - 1;
  // Fallback: first `(` that isn't inside a string.
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "(") return i;
  }
  return -1;
}

function findMatchingParen(s: string, open: number): number {
  let depth = 0;
  let inStr: string | null = null;
  for (let i = open; i < s.length; i++) {
    const ch = s[i];
    if (inStr) {
      if (ch === "\\" && i + 1 < s.length) {
        i++;
        continue;
      }
      if (ch === inStr) inStr = null;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === "`") {
      inStr = ch;
      continue;
    }
    if (ch === "(") depth++;
    else if (ch === ")") {
      depth--;
      if (depth === 0) return i;
    }
  }
  return -1;
}

function splitTopLevelCommas(inner: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  let inStr: string | null = null;
  for (let i = 0; i < inner.length; i++) {
    const ch = inner[i];
    if (inStr) {
      if (ch === "\\" && i + 1 < inner.length) {
        i++;
        continue;
      }
      if (ch === inStr) inStr = null;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === "`") {
      inStr = ch;
      continue;
    }
    if (ch === "(" || ch === "[" || ch === "{") depth++;
    else if (ch === ")" || ch === "]" || ch === "}") depth = Math.max(0, depth - 1);
    else if (ch === "," && depth === 0) {
      parts.push(inner.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(inner.slice(start));
  return parts;
}

function paramNameFromPart(part: string): string | null {
  if (!part) return null;
  // Strip leading attributes / keywords: `mut`, `ref`, `pub`, `&`, `*`, `**`.
  let p = part
    .replace(/^&+'?mut\s+/, "")
    .replace(/^&+'?\s*/, "")
    .replace(/^\*+\s*/, "")
    .replace(/^(?:mut|ref|pub|const|var|let)\s+/, "")
    .trim();
  // Python *args / **kwargs
  p = p.replace(/^\*+\s*/, "");
  // `name: Type` / `name = default` / bare `name`
  const m = /^([A-Za-z_][\w']*)/.exec(p);
  if (!m) return null;
  return m[1];
}

// --- pure: call-site scan + literal args ----------------------------------

export interface CallSiteHit {
  /** Document offset of the callee identifier start. */
  calleeFrom: number;
  calleeTo: number;
  calleeName: string;
  /** 1-based line of the callee (for resolve). */
  line: number;
  /** 0-based col of the callee start. */
  col: number;
  args: ArgSpan[];
}

export interface ArgSpan {
  /** Document offset of the argument expression start. */
  from: number;
  to: number;
  text: string;
  index: number;
}

const IDENT_RE = /^[A-Za-z_][\w']*$/;

/** True when the arg text is literal-ish (numbers, strings, true/false/null/None). */
export function isLiteralIshArg(text: string): boolean {
  const t = text.trim();
  if (!t) return false;
  if (/^(?:true|false|null|None|nil|undefined)$/.test(t)) return true;
  if (/^[-+]?(\d+(\.\d*)?|\.\d+)([eE][-+]?\d+)?$/.test(t)) return true;
  if (/^0[xX][\da-fA-F]+$/.test(t)) return true;
  if (
    (t.startsWith('"') && t.endsWith('"')) ||
    (t.startsWith("'") && t.endsWith("'")) ||
    (t.startsWith("`") && t.endsWith("`"))
  ) {
    return true;
  }
  return false;
}

/** Skip when the arg is an identifier equal-ish to the param name. */
export function shouldSkipArgHint(argText: string, paramName: string): boolean {
  const t = argText.trim();
  if (!IDENT_RE.test(t)) return false;
  return t.toLowerCase() === paramName.toLowerCase() || t === paramName;
}

/**
 * Scan `docText` for simple call sites: `name(…)` with balanced parens.
 * Only considers identifiers not preceded by `.` (method calls still work
 * when the identifier is after `.` — we take the last segment).
 */
export function findCallSitesInRange(
  docText: string,
  from: number,
  to: number,
  lineAt: (pos: number) => { number: number; from: number },
): CallSiteHit[] {
  const hits: CallSiteHit[] = [];
  // Bound the scan to the viewport slice (with a little padding already applied by caller).
  const slice = docText.slice(from, to);
  const re = /\b([A-Za-z_][\w']*)\s*\(/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(slice)) !== null) {
    const name = m[1];
    const nameStartInSlice = m.index;
    const absNameStart = from + nameStartInSlice;
    // Skip if this looks like a definition keyword form we already handled poorly —
    // e.g. `fn foo(` — the name is after fn, that's fine; but skip `if (` / `for (` etc.
    if (isControlKeyword(name)) continue;
    const openAbs = from + m.index + m[0].length - 1;
    const closeAbs = findMatchingParen(docText, openAbs);
    if (closeAbs < 0 || closeAbs > to + 200) continue;
    const argsInner = docText.slice(openAbs + 1, closeAbs);
    const argParts = splitTopLevelCommas(argsInner);
    // Empty call — no hints needed.
    if (argParts.length === 1 && argParts[0].trim() === "") continue;
    const args: ArgSpan[] = [];
    let offset = openAbs + 1;
    for (let i = 0; i < argParts.length; i++) {
      const raw = argParts[i];
      // Preserve leading whitespace for correct offsets.
      const lead = raw.match(/^\s*/)?.[0].length ?? 0;
      const trail = raw.match(/\s*$/)?.[0].length ?? 0;
      const content = raw.slice(lead, raw.length - trail);
      const argFrom = offset + lead;
      const argTo = argFrom + content.length;
      if (content.length > 0) {
        args.push({ from: argFrom, to: argTo, text: content, index: i });
      }
      offset += raw.length + 1; // +1 for the comma
    }
    if (args.length === 0) continue;
    const line = lineAt(absNameStart);
    hits.push({
      calleeFrom: absNameStart,
      calleeTo: absNameStart + name.length,
      calleeName: name,
      line: line.number,
      col: absNameStart - line.from,
      args,
    });
  }
  return hits;
}

function isControlKeyword(name: string): boolean {
  return /^(if|for|while|match|switch|catch|with|fn|def|function|class|struct|enum|trait|impl|return|yield|await|typeof|new|delete)$/.test(
    name,
  );
}

// --- LRU cache for resolve results ----------------------------------------

interface CacheEntry {
  candidate: ResolveCandidate | null;
  params: string[] | null;
}

const resolveCache = new Map<string, CacheEntry>();

function cacheKey(path: string, name: string): string {
  return `${path}\0${name}`;
}

function lruGet(key: string): CacheEntry | undefined {
  const v = resolveCache.get(key);
  if (!v) return undefined;
  // Refresh recency.
  resolveCache.delete(key);
  resolveCache.set(key, v);
  return v;
}

function lruSet(key: string, entry: CacheEntry): void {
  if (resolveCache.has(key)) resolveCache.delete(key);
  resolveCache.set(key, entry);
  while (resolveCache.size > LRU_CAP) {
    const first = resolveCache.keys().next().value;
    if (first === undefined) break;
    resolveCache.delete(first);
  }
}

/** Test-only: clear module LRU. */
export function _clearParamHintsCacheForTests(): void {
  resolveCache.clear();
}

// --- CM6 widgets + extension ----------------------------------------------

class ParamHintWidget extends WidgetType {
  constructor(readonly name: string) {
    super();
  }
  eq(other: ParamHintWidget) {
    return other.name === this.name;
  }
  toDOM() {
    const span = document.createElement("span");
    span.className = "cm-kbc-param-hint";
    span.textContent = `${this.name}:`;
    span.setAttribute("data-kbc-param-hint", this.name);
    return span;
  }
  ignoreEvent() {
    return true;
  }
}

export interface ParamHintRange {
  from: number;
  name: string;
}

const setParamHints = StateEffect.define<ParamHintRange[]>();
/** Dispatched when the pref toggles so the plugin re-runs without a viewport change. */
export const paramHintsRefreshEffect = StateEffect.define<null>();

function buildDeco(hints: readonly ParamHintRange[]): DecorationSet {
  if (hints.length === 0) return Decoration.none;
  // Decorations must be sorted by from.
  const sorted = [...hints].sort((a, b) => a.from - b.from || a.name.localeCompare(b.name));
  return Decoration.set(
    sorted.map((h) =>
      Decoration.widget({
        widget: new ParamHintWidget(h.name),
        side: -1,
      }).range(h.from),
    ),
    true,
  );
}

const paramHintsField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const e of tr.effects) {
      if (e.is(setParamHints)) return buildDeco(e.value);
      // Pref off: clear immediately so the toggle feels instant.
      if (e.is(paramHintsRefreshEffect)) {
        // Leave deco as-is here; the plugin recompute will set the final set.
        // When disabled, clear right away for snappy UI.
        return deco;
      }
    }
    if (tr.docChanged) return Decoration.none;
    return deco.map(tr.changes);
  },
  provide: (field) => EditorView.decorations.from(field),
});

export interface ParamHintsOptions {
  /** File path (for resolve + cache key). */
  getPath: () => string | null;
  getRepo: () => string | null;
  /** Pref gate — when false, clear decorations. */
  isEnabled: () => boolean;
}

/**
 * Build decorations for a known set of call sites + param lists (pure;
 * used by tests and the plugin after resolve).
 */
export function buildHintsForSites(
  sites: CallSiteHit[],
  paramsForCallee: (name: string) => string[] | null,
): ParamHintRange[] {
  const hints: ParamHintRange[] = [];
  for (const site of sites) {
    const params = paramsForCallee(site.calleeName);
    if (!params) continue;
    for (const arg of site.args) {
      if (arg.index >= params.length) continue;
      const pname = params[arg.index];
      if (!isLiteralIshArg(arg.text)) continue;
      if (shouldSkipArgHint(arg.text, pname)) continue;
      hints.push({ from: arg.from, name: pname });
    }
  }
  return hints;
}

function createParamHintsPlugin(opts: ParamHintsOptions) {
  return ViewPlugin.fromClass(
    class {
      private timer: ReturnType<typeof setTimeout> | null = null;
      private gen = 0;

      constructor(view: EditorView) {
        this.schedule(view);
      }

      update(update: ViewUpdate) {
        const refreshed = update.transactions.some((tr) =>
          tr.effects.some((e) => e.is(paramHintsRefreshEffect)),
        );
        if (
          update.docChanged ||
          update.viewportChanged ||
          update.geometryChanged ||
          refreshed
        ) {
          this.schedule(update.view);
        }
      }

      destroy() {
        if (this.timer != null) clearTimeout(this.timer);
        this.gen++;
      }

      private schedule(view: EditorView) {
        if (this.timer != null) clearTimeout(this.timer);
        this.timer = setTimeout(() => {
          this.timer = null;
          void this.recompute(view);
        }, PARAM_HINTS_DEBOUNCE_MS);
      }

      private async recompute(view: EditorView) {
        const myGen = ++this.gen;
        try {
          if (!view.dom.isConnected) return;
        } catch {
          return;
        }
        if (!opts.isEnabled()) {
          view.dispatch({ effects: setParamHints.of([]) });
          return;
        }
        const repo = opts.getRepo();
        const path = opts.getPath();
        if (!repo || !path) {
          view.dispatch({ effects: setParamHints.of([]) });
          return;
        }

        const docText = view.state.doc.sliceString(0);
        const vp = view.viewport;
        const lineAt = (pos: number) => view.state.doc.lineAt(pos);
        const sites = findCallSitesInRange(docText, vp.from, vp.to, lineAt);
        if (sites.length === 0) {
          view.dispatch({ effects: setParamHints.of([]) });
          return;
        }

        // Resolve unique callee names (cap 30).
        const unique = new Map<string, CallSiteHit>();
        for (const s of sites) {
          if (!unique.has(s.calleeName)) unique.set(s.calleeName, s);
        }
        let resolves = 0;
        const paramsMap = new Map<string, string[] | null>();

        for (const [name, site] of unique) {
          const key = cacheKey(path, name);
          const cached = lruGet(key);
          if (cached) {
            paramsMap.set(name, cached.params);
            continue;
          }
          if (resolves >= PARAM_HINTS_RESOLVE_CAP) {
            paramsMap.set(name, null);
            continue;
          }
          resolves++;
          try {
            const out = await fetchResolve({
              repo,
              path,
              line: site.line,
              col: site.col,
            });
            if (myGen !== this.gen) return;
            const top = out.candidates[0];
            const cls = (top?.class ?? classFromPrecision(top?.precision)).toLowerCase();
            const ok = top && (cls === "exact" || cls === "likely");
            const params = ok ? parseSignatureParamNames(top.signature) : null;
            lruSet(key, { candidate: top ?? null, params });
            paramsMap.set(name, params);
          } catch {
            if (myGen !== this.gen) return;
            lruSet(key, { candidate: null, params: null });
            paramsMap.set(name, null);
          }
        }

        if (myGen !== this.gen) return;
        const hints = buildHintsForSites(sites, (n) => paramsMap.get(n) ?? null);
        view.dispatch({ effects: setParamHints.of(hints) });
      }
    },
  );
}

function classFromPrecision(precision: string | undefined): string {
  if (!precision) return "candidate";
  if (precision === "scip-exact" || precision === "locals") return "exact";
  if (
    precision === "file-local" ||
    precision === "import-heuristic" ||
    precision === "import-filtered"
  ) {
    return "likely";
  }
  return "candidate";
}

/**
 * CM6 extension factory — pass path/repo/enabled getters so a single view
 * recreation can re-read live Reader prefs without rebuilding the field.
 */
export function paramHintsExtension(opts: ParamHintsOptions): Extension {
  return [paramHintsField, createParamHintsPlugin(opts)];
}
