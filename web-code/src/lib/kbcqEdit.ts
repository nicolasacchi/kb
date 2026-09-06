// V71-D2 — the ONE builder that WRITES a kbcq/1 query string.
//
// Design D3's rule for the results page is that a facet, a scope chip, a
// grouping control and the `sort:` escape hatch all narrow the search by
// EDITING THE QUERY IN THE BOX — never by setting hidden component state
// that the string does not show. That is the same discipline root
// CLAUDE.md #35 records for kb's `galleryUrl`: one builder, golden-pinned,
// so every click is a shareable, pasteable, CLI-runnable query.
//
// This module is the write half of `lib/kbcq.ts`'s read half. It never
// re-implements the grammar: every function here VERIFIES its own edit by
// re-parsing the result with `kbcq.parse` — so a clause this file appends
// that the parser does not read comes back as "not applied" instead of
// silently doing nothing. (kb-core's `mentions::apply_wikilink` verifies a
// splice by re-running the parser rather than translating an offset; same
// idea, same reason.)
//
// Nothing here is a MATCHER. `lib/speedSearch.ts` is deprecated as one
// (kb-code-server/CLAUDE.md #16b) and `lib/matchRanges.ts` owns highlight
// rendering; this file only rewrites text.

import { parse, type GroupKey, type Lane, type ParsedQuery, type SortKey } from "./kbcq";

/// The lane prefixes, mirroring `search::results::lane_prefix` on the
/// server — the facet rail's `writes: "prefix"` groups send these.
export const LANE_PREFIXES: Record<Lane, string> = {
  files: "#",
  symbols: "@",
  text: "/",
  semantic: "?nl ",
  sessions: "~",
  transcripts: "~~",
};

/// Every prefix, longest first — the order `kbcq.parse` itself checks in,
/// so `~~` is recognised before `~`.
const PREFIXES_LONGEST_FIRST = ["~~", "?nl ", "?nl", "~", "@", "#", "/"];

/// Strip whatever lane prefix `q` opens with, returning the bare remainder.
/// Idempotent, and total: a query with no prefix comes back unchanged.
export function stripLanePrefix(q: string): string {
  const t = q.trimStart();
  for (const p of PREFIXES_LONGEST_FIRST) {
    if (t.startsWith(p)) return t.slice(p.length).trimStart();
  }
  return t;
}

/// Select `lane` for `q`, replacing any prefix already there. Idempotent:
/// applying the same lane twice is the same string.
export function setLane(q: string, lane: Lane): string {
  const prefix = LANE_PREFIXES[lane];
  // Total by construction: a lane name this build does not know (an older
  // SPA against a newer daemon that grew a seventh lane) leaves the query
  // ALONE rather than writing `undefinedorder` into the box.
  if (prefix === undefined) return q;
  const rest = stripLanePrefix(q);
  // A bare prefix must still parse as that lane: `?nl` (no trailing space)
  // and `/` both do, which is why the empty-remainder case trims.
  return rest === "" ? prefix.trimEnd() : prefix + rest;
}

/// Split a `key:value` clause. Returns `null` for anything that is not
/// shaped like one (a lane prefix, a bare word) — the caller then knows to
/// use `setLane` instead of `toggleClause`.
export function splitClause(clause: string): { key: string; value: string; negated: boolean } | null {
  const negated = clause.startsWith("-");
  const body = negated ? clause.slice(1) : clause;
  const colon = body.indexOf(":");
  if (colon <= 0) return null;
  const key = body.slice(0, colon);
  if (!/^[A-Za-z0-9_-]+$/.test(key)) return null;
  return { key, value: body.slice(colon + 1), negated };
}

/// Is `clause` already in effect in `q`? Answered by PARSING, never by a
/// substring test — `path:app` inside a quoted phrase is a term, not a
/// filter, and only the parser knows the difference.
export function hasClause(q: string, clause: string): boolean {
  const parts = splitClause(clause);
  if (!parts) return false;
  const want = unquote(parts.value);
  const p = parse(q);
  return clauseValues(p, parts.key, parts.negated).includes(want);
}

function unquote(v: string): string {
  return v.startsWith('"') && v.endsWith('"') && v.length >= 2 ? v.slice(1, -1) : v;
}

/// The values a parsed query holds for one key — the single place this
/// module maps a kbcq/1 key onto a `ParsedQuery` field, so a key added to
/// the grammar without a case here is caught by
/// `kbcqEdit.test.ts`'s walk over `FILTER_SPECS`.
function clauseValues(p: ParsedQuery, key: string, negated: boolean): string[] {
  const one = (v: string | null) => (v === null ? [] : [v]);
  switch (`${key}:${negated}`) {
    case "lang:false":
      return one(p.filters.lang);
    case "lang:true":
      return p.filters.not_lang;
    case "path:false":
      return one(p.filters.path);
    case "path:true":
      return p.filters.not_path;
    case "repo:false":
      return one(p.filters.repo);
    case "ext:false":
      return p.filters.ext;
    case "ext:true":
      return p.filters.not_ext;
    case "kind:false":
      return p.filters.kind;
    case "kind:true":
      return p.filters.not_kind;
    case "case:false":
      return p.filters.case === null ? [] : [p.filters.case ? "yes" : "no"];
    case "sort:false":
      return p.sort === null ? [] : [p.sort as SortKey];
    case "explain:false":
      return p.explain ? ["1"] : [];
    case "group:false":
      return p.group === null ? [] : [p.group as GroupKey];
    case "facets:false":
      return p.facets ? ["1"] : [];
    default:
      return [];
  }
}

/// Append `clause` to `q` unless it is already in effect. Whitespace is
/// normalised to exactly one separating space; the caller's own text is
/// otherwise untouched.
export function appendClause(q: string, clause: string): string {
  if (hasClause(q, clause)) return q;
  const base = q.trimEnd();
  return base === "" ? clause : `${base} ${clause}`;
}

/// Remove every TOKEN of `q` that is exactly `clause` (quoting-insensitive
/// on the value). Token-wise, so a term that merely contains the text is
/// never touched.
export function removeClause(q: string, clause: string): string {
  const parts = splitClause(clause);
  if (!parts) return q;
  const wantKey = (parts.negated ? "-" : "") + parts.key;
  const wantValue = unquote(parts.value);
  const kept = tokenize(q).filter((tok) => {
    const t = splitClause(tok);
    if (!t) return true;
    const key = (t.negated ? "-" : "") + t.key;
    if (key !== wantKey) return true;
    // A multi-valued token (`ext:rb|erb`) is dropped only when it is
    // exactly this one value — narrowing an alternation is a different
    // edit, and guessing at it would silently drop the other value.
    return unquote(t.value) !== wantValue;
  });
  return kept.join(" ").trim();
}

/// Add the clause if absent, drop it if present — what a facet row's click
/// does. The result always PARSES to a query whose meaning matches the
/// toggle (asserted in the unit suite for every facet field the server can
/// emit).
export function toggleClause(q: string, clause: string): string {
  return hasClause(q, clause) ? removeClause(q, clause) : appendClause(q, clause);
}

/// Set `group:` to exactly `key`, replacing whatever was there. `null`
/// removes the token entirely, which is NOT the same as `group:none` —
/// see `kbcq.ts`'s `GroupKey` doc.
export function setGroup(q: string, key: GroupKey | null): string {
  const current = parse(q).group;
  let out = q;
  if (current !== null) out = removeClause(out, `group:${current}`);
  return key === null ? out.trim() : appendClause(out, `group:${key}`);
}

/// Turn `facets:1` on or off.
export function setFacets(q: string, on: boolean): string {
  const has = parse(q).facets;
  if (on === has) return q;
  return on ? appendClause(q, "facets:1") : removeClause(q, "facets:1");
}

/// Whitespace-split honouring double quotes — the same rule `kbcq.ts`'s own
/// `tokenize` uses, kept here (rather than exported from there) because
/// this one keeps the quotes ON, which is what re-joining a query needs.
function tokenize(s: string): string[] {
  const out: string[] = [];
  let cur = "";
  let inQuotes = false;
  for (const ch of s) {
    if (ch === '"') {
      inQuotes = !inQuotes;
      cur += ch;
      continue;
    }
    if (/\s/.test(ch) && !inQuotes) {
      if (cur !== "") out.push(cur);
      cur = "";
      continue;
    }
    cur += ch;
  }
  if (cur !== "") out.push(cur);
  return out;
}
