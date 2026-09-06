// V3.4-C3 — `/r/{repo}/~browser[?symbol=]` URL builder/parser.
// URL owns the browser's focused symbol selection (Smalltalk-lens panes).

import { codeBasePath } from "./codeUrl";

/** One symbol pin: path + name + optional 1-based declaration line. */
export interface BrowserSymbolRef {
  path: string;
  name: string;
  /** 1-based declaration line when known. */
  line?: number;
  /**
   * Optional container name when this ref is a *member* of a type/class.
   * Kept so pane 1 stays selected after Enter-drilling into a member.
   */
  container?: string;
}

export interface BrowserUrlState {
  symbol: BrowserSymbolRef | null;
}

/**
 * Wire encoding for `?symbol=`:
 *   `path#name` or `path#name#line` or `path#name#line@container`
 *
 * Path may contain `/` but never `#` (path segments are repo-relative).
 * Name may not contain `#`/`@`/`:` — if it does, encodeURIComponent applies.
 * Total: junk/malformed → null (never throw).
 */
export function encodeBrowserSymbol(ref: BrowserSymbolRef): string {
  const path = ref.path.replace(/#/g, "");
  const name = encodeURIComponent(ref.name);
  let s = `${path}#${name}`;
  if (ref.line != null && Number.isFinite(ref.line) && ref.line >= 1) {
    s += `#${Math.floor(ref.line)}`;
  }
  if (ref.container) {
    s += `@${encodeURIComponent(ref.container)}`;
  }
  return s;
}

/** Parse a `?symbol=` value; total for junk. */
export function decodeBrowserSymbol(raw: string | null | undefined): BrowserSymbolRef | null {
  if (raw == null || raw === "") return null;
  // Split container suffix first (last @…).
  let container: string | undefined;
  let body = raw;
  const at = raw.lastIndexOf("@");
  if (at > 0) {
    const c = raw.slice(at + 1);
    if (c) {
      try {
        container = decodeURIComponent(c);
      } catch {
        container = c;
      }
      body = raw.slice(0, at);
    }
  }
  // path#name or path#name#line
  const hash = body.indexOf("#");
  if (hash <= 0) return null;
  const path = body.slice(0, hash);
  if (!path) return null;
  const rest = body.slice(hash + 1);
  if (!rest) return null;
  const parts = rest.split("#");
  let nameRaw = parts[0] ?? "";
  let line: number | undefined;
  if (parts.length >= 2 && parts[1] !== "") {
    const n = Number(parts[1]);
    if (Number.isFinite(n) && n >= 1) line = Math.floor(n);
  }
  let name: string;
  try {
    name = decodeURIComponent(nameRaw);
  } catch {
    name = nameRaw;
  }
  if (!name) return null;
  const out: BrowserSymbolRef = { path, name };
  if (line != null) out.line = line;
  if (container) out.container = container;
  return out;
}

function isSymbolRef(v: unknown): v is BrowserSymbolRef {
  return (
    !!v &&
    typeof v === "object" &&
    "path" in v &&
    "name" in v &&
    typeof (v as BrowserSymbolRef).path === "string" &&
    typeof (v as BrowserSymbolRef).name === "string" &&
    !("symbol" in v)
  );
}

/** `browserUrl(repo, { symbol? })` → `/r/{repo}/~browser[?symbol=…]`. */
export function browserUrl(
  repo: string,
  state?: BrowserUrlState | BrowserSymbolRef | null,
): string {
  const base = `${codeBasePath(repo, "")}/~browser`;
  let symbol: BrowserSymbolRef | null = null;
  if (state == null) {
    symbol = null;
  } else if (isSymbolRef(state)) {
    symbol = state;
  } else if ("symbol" in state) {
    symbol = state.symbol;
  }
  if (!symbol) return base;
  const qs = new URLSearchParams();
  qs.set("symbol", encodeBrowserSymbol(symbol));
  return `${base}?${qs.toString()}`;
}

/** Parse `?symbol=` from search params. */
export function parseBrowserSearch(
  sp: URLSearchParams | { get(name: string): string | null },
): BrowserUrlState {
  return { symbol: decodeBrowserSymbol(sp.get("symbol")) };
}
