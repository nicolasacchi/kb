// kbcq/1 — the SPA half of the ONE query grammar (V71-D1, design D3).
//
// This is a MIRROR of `crates/kb-code-server/src/search/grammar.rs`, not a
// derivation of it: neither side generates the other, and neither is
// authoritative over the other at runtime (the daemon parses what it runs;
// this parses what the query bar renders — chips, the removable-filter row,
// the "unknown filter, did you mean…" line, and `extractRepoFilter`'s repo
// recovery). They are held in LOCK-STEP by ONE shared fixture,
// `crates/kb-code-server/grammar/kbcq.golden.json`, which BOTH sides walk:
// `grammar.rs`'s `golden_corpus_matches_the_rust_parser` and this file's
// `kbcq.golden.test.ts` read the same bytes. Change the grammar on one side
// only and the other side's golden test fails — the same discipline root
// CLAUDE.md #29 records for wikilinks and #35 for the gallery URL builder.
//
// ONE deliberate exception, documented rather than hidden: for a BARE `/…`
// text query, "is this a regex?" is decided by trying to compile it, and the
// two sides use different engines (Rust's `regex` crate vs ECMAScript
// `RegExp`). Their dialects diverge on patterns nobody in the golden uses
// (`(?P<n>…)` compiles in Rust and not in JS; a backreference `\1` the other
// way round). Where they disagree, the DAEMON's answer is the real one — this
// side's `text_mode` is a display hint, never a thing the SPA sends.
//
// See `grammar.rs`'s module doc for the grammar itself; the comments here
// cover only what a TypeScript reader needs.

export type Lane = "files" | "symbols" | "text" | "semantic" | "sessions" | "transcripts";

/// The canonical, FIXED section order — `grammar.rs`'s `LANE_ORDER`.
export const LANE_ORDER: Lane[] = [
  "files",
  "symbols",
  "text",
  "semantic",
  "sessions",
  "transcripts",
];

export type TextMode = "literal" | "regex";
export type SortKey = "relevance" | "path";
/// `group:` — how the returned page is grouped (V71-D2). `"none"` is a real
/// value, distinct from the token being absent: `null` means "the author said
/// nothing", `"none"` means "the author said ungrouped", and `normalize`
/// renders them differently so a saved search keeps its answer.
export type GroupKey = "file" | "kind" | "lane" | "dir" | "none";

/// `grammar.rs`'s `GROUP_KEYS`, in the same order.
export const GROUP_KEYS: GroupKey[] = ["file", "kind", "lane", "dir", "none"];

/// `crate::rails::NOUNS`, in the same order — the `rails:` atom's closed
/// vocabulary (V72-I1). The Rust side reads its copy straight out of the
/// `rails` module so there is one home THERE; this mirror is pinned to it
/// by the shared `kbcq.golden.json` corpus, the same way every other key's
/// vocabulary is.
export const RAILS_NOUNS: string[] = [
  "model",
  "controller",
  "action",
  "route",
  "job",
  "mailer",
  "view",
  "concern",
];

/**
 * V75-M3 — `agent:`'s closed vocabulary, mirroring `grammar.rs`'s
 * `AGENT_FILTER_VALUES`. `any` = any evidence (exact OR likely); there is
 * deliberately no value meaning "definitely not an agent".
 */
export const AGENT_FILTER_VALUES = ["exact", "likely", "any", "none"] as const;

export interface Filters {
  lang: string | null;
  path: string | null;
  repo: string | null;
  /** `true` = case-sensitive (`case:yes`), `false` = insensitive. */
  case: boolean | null;
  ext: string[];
  kind: string[];
  not_lang: string[];
  not_path: string[];
  not_ext: string[];
  not_kind: string[];
  /**
   * V72-I1 — the `rails/1` facet atoms, mirroring `grammar.rs`'s own
   * fields. The value is carried verbatim; resolving it to a file set is
   * the daemon's job (the SPA has no index to resolve against), so these
   * are display + round-trip state here and nothing more.
   */
  model: string | null;
  controller: string | null;
  action: string | null;
  route: string | null;
  job: string | null;
  /** `rails:<noun>` — the generic form; vocabulary is `RAILS_NOUNS`. */
  rails: string | null;
  /**
   * V75-M3 — the `~branches` atoms (`branch-facts/1`). Same posture as
   * the Rails atoms: the value is carried verbatim and resolved by the
   * daemon (`history::facts`), never here.
   */
  branch: string | null;
  touches: string | null;
  by: string | null;
  /** `agent:` — vocabulary is `AGENT_FILTER_VALUES`. */
  agent: string | null;
}

export interface QueryTerm {
  text: string;
  quoted: boolean;
}

export interface Diagnostic {
  severity: string;
  token: string;
  message: string;
  suggestion?: string;
}

export interface ParsedQuery {
  lanes: Lane[];
  query: string;
  terms: QueryTerm[];
  filters: Filters;
  text_mode: TextMode;
  sort: SortKey | null;
  explain: boolean;
  group: GroupKey | null;
  facets: boolean;
  diagnostics: Diagnostic[];
  normalized: string;
}

export interface FilterKeySpec {
  key: string;
  multi: boolean;
  negatable: boolean;
  values: string[] | null;
}

/// `grammar.rs`'s `FILTER_SPECS`, in the same DECLARATION ORDER — which is
/// also the order `normalize` renders filters in, so the order here is
/// load-bearing, not cosmetic.
export const FILTER_SPECS: FilterKeySpec[] = [
  { key: "lang", multi: false, negatable: true, values: null },
  { key: "path", multi: false, negatable: true, values: null },
  { key: "repo", multi: false, negatable: false, values: null },
  { key: "case", multi: false, negatable: false, values: ["yes", "true", "1", "no", "false", "0"] },
  { key: "ext", multi: true, negatable: true, values: null },
  { key: "kind", multi: true, negatable: true, values: null },
  { key: "sort", multi: false, negatable: false, values: ["relevance", "path"] },
  { key: "explain", multi: false, negatable: false, values: ["1", "0", "yes", "no", "true", "false"] },
  { key: "group", multi: false, negatable: false, values: [...GROUP_KEYS] },
  { key: "facets", multi: false, negatable: false, values: ["1", "0", "yes", "no", "true", "false"] },
  // V72-I1 — the rails/1 facet atoms, APPENDED (declaration order is
  // `normalize`'s render order, so every pre-existing query's normalized
  // form is byte-identical).
  { key: "model", multi: false, negatable: false, values: null },
  { key: "controller", multi: false, negatable: false, values: null },
  { key: "action", multi: false, negatable: false, values: null },
  { key: "route", multi: false, negatable: false, values: null },
  { key: "job", multi: false, negatable: false, values: null },
  { key: "rails", multi: false, negatable: false, values: [...RAILS_NOUNS] },
  // V75-M3 — the ~branches omnibox atoms, APPENDED for the same reason.
  { key: "branch", multi: false, negatable: false, values: null },
  { key: "touches", multi: false, negatable: false, values: null },
  { key: "by", multi: false, negatable: false, values: null },
  { key: "agent", multi: false, negatable: false, values: [...AGENT_FILTER_VALUES] },
];

export const SUGGEST_MAX_DISTANCE = 2;

const LANE_PREFIX: Partial<Record<Lane, string>> = {
  files: "#",
  symbols: "@",
  semantic: "?nl ",
  sessions: "~",
  transcripts: "~~",
};

export function emptyFilters(): Filters {
  return {
    lang: null,
    path: null,
    repo: null,
    case: null,
    ext: [],
    kind: [],
    not_lang: [],
    not_path: [],
    not_ext: [],
    not_kind: [],
    model: null,
    controller: null,
    action: null,
    route: null,
    job: null,
    rails: null,
    branch: null,
    touches: null,
    by: null,
    agent: null,
  };
}

interface RawToken {
  text: string;
  raw: string;
  quoted: boolean;
}

/// Whitespace-tokenise, honouring double quotes — `grammar.rs`'s `tokenize`.
/// An unterminated quote runs to the end of the string: a half-typed query is
/// the common case in an interactive box.
function tokenize(s: string): RawToken[] {
  const out: RawToken[] = [];
  let text = "";
  let raw = "";
  let quoted = false;
  let inQuotes = false;
  for (const ch of s) {
    if (ch === '"') {
      inQuotes = !inQuotes;
      quoted = true;
      raw += ch;
      continue;
    }
    if (/\s/.test(ch) && !inQuotes) {
      if (raw !== "") {
        out.push({ text, raw, quoted });
        text = "";
        raw = "";
        quoted = false;
      }
      continue;
    }
    text += ch;
    raw += ch;
  }
  if (raw !== "") out.push({ text, raw, quoted });
  return out;
}

/// `[negated, key, value]` for a `key:value`-shaped token, else null. The key
/// must be ASCII alphanumerics/`_`/`-` so `https://x` and `Foo::bar` are
/// never mistaken for filters.
function splitKeyValue(text: string): [boolean, string, string] | null {
  const negated = text.startsWith("-");
  const body = negated ? text.slice(1) : text;
  const colon = body.indexOf(":");
  if (colon < 0) return null;
  const key = body.slice(0, colon);
  if (key === "" || !/^[A-Za-z0-9_-]+$/.test(key)) return null;
  return [negated, key, body.slice(colon + 1)];
}

function specFor(key: string): FilterKeySpec | undefined {
  return FILTER_SPECS.find((s) => s.key === key);
}

/// Plain Levenshtein over two short strings — `grammar.rs`'s `edit_distance`.
export function editDistance(a: string, b: string): number {
  const A = [...a];
  const B = [...b];
  if (A.length === 0) return B.length;
  let prev = Array.from({ length: B.length + 1 }, (_, i) => i);
  let cur = new Array<number>(B.length + 1).fill(0);
  for (let i = 0; i < A.length; i++) {
    cur[0] = i + 1;
    for (let j = 0; j < B.length; j++) {
      const cost = A[i] === B[j] ? 0 : 1;
      cur[j + 1] = Math.min(prev[j] + cost, prev[j + 1] + 1, cur[j] + 1);
    }
    [prev, cur] = [cur, prev];
  }
  return prev[B.length];
}

/// The did-you-mean for an unknown key — nearest spec key within
/// `SUGGEST_MAX_DISTANCE`, ties broken by key name (matching the Rust
/// `min_by_key((distance, key))`).
export function suggestKey(key: string): string | null {
  const lower = key.toLowerCase();
  const ranked = FILTER_SPECS.map((s) => ({ key: s.key, d: editDistance(lower, s.key) }))
    .filter((c) => c.d <= SUGGEST_MAX_DISTANCE)
    .sort((a, b) => a.d - b.d || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  return ranked.length > 0 ? ranked[0].key : null;
}

interface Extracted {
  terms: QueryTerm[];
  filters: Filters;
  sort: SortKey | null;
  explain: boolean;
  group: GroupKey | null;
  facets: boolean;
  diagnostics: Diagnostic[];
}

function warn(token: string, message: string, suggestion?: string): Diagnostic {
  const d: Diagnostic = { severity: "warning", token, message };
  if (suggestion !== undefined) d.suggestion = suggestion;
  return d;
}

/// `grammar.rs`'s `extract` — pull out recognised filters and modifiers, keep
/// everything else as a term, and EXPLAIN every token that could not be
/// honoured (never a hard failure, never a silent drop).
function extract(s: string): Extracted {
  const filters = emptyFilters();
  const terms: QueryTerm[] = [];
  const diagnostics: Diagnostic[] = [];
  let sort: SortKey | null = null;
  let explain = false;
  let group: GroupKey | null = null;
  let facets = false;

  const keep = (tok: RawToken) => terms.push({ text: tok.text, quoted: tok.quoted });

  for (const tok of tokenize(s)) {
    if (tok.quoted && tok.raw.startsWith('"')) {
      keep(tok);
      continue;
    }
    const kv = splitKeyValue(tok.text);
    if (!kv) {
      keep(tok);
      continue;
    }
    const [negated, key, value] = kv;
    const spec = specFor(key);
    if (!spec) {
      const hint = suggestKey(key);
      if (hint) {
        diagnostics.push(
          warn(tok.raw, `unknown filter \`${key}:\` — searched as an ordinary word`, `${hint}:${value}`),
        );
      }
      keep(tok);
      continue;
    }
    if (value === "") {
      diagnostics.push(warn(tok.raw, `\`${key}:\` has no value — searched as an ordinary word`));
      keep(tok);
      continue;
    }
    if (negated && !spec.negatable) {
      diagnostics.push(warn(tok.raw, `\`${key}:\` cannot be negated — searched as an ordinary word`));
      keep(tok);
      continue;
    }
    let values = spec.multi ? value.split("|").filter((v) => v !== "") : [value];
    if (spec.values) {
      const canon: string[] = [];
      let bad: string | null = null;
      for (const v of values) {
        const hit = spec.values.find((k) => k.toLowerCase() === v.toLowerCase());
        if (hit === undefined) {
          bad = v;
          break;
        }
        canon.push(hit);
      }
      if (bad !== null) {
        diagnostics.push(
          warn(
            tok.raw,
            `\`${key}:\` takes ${spec.values.join("|")} — \`${bad}\` searched as an ordinary word`,
          ),
        );
        keep(tok);
        continue;
      }
      values = canon;
    }
    const last = values.length > 0 ? values[values.length - 1] : "";
    switch (`${spec.key}:${negated}`) {
      case "lang:false":
        filters.lang = last;
        break;
      case "lang:true":
        filters.not_lang.push(...values);
        break;
      case "path:false":
        filters.path = last;
        break;
      case "path:true":
        filters.not_path.push(...values);
        break;
      case "repo:false":
      case "repo:true":
        filters.repo = last;
        break;
      case "case:false":
      case "case:true":
        filters.case = last === "yes" || last === "true" || last === "1";
        break;
      case "ext:false":
        filters.ext.push(...values);
        break;
      case "ext:true":
        filters.not_ext.push(...values);
        break;
      case "kind:false":
        filters.kind.push(...values);
        break;
      case "kind:true":
        filters.not_kind.push(...values);
        break;
      case "model:false":
      case "model:true":
        filters.model = last;
        break;
      case "controller:false":
      case "controller:true":
        filters.controller = last;
        break;
      case "action:false":
      case "action:true":
        filters.action = last;
        break;
      case "route:false":
      case "route:true":
        filters.route = last;
        break;
      case "job:false":
      case "job:true":
        filters.job = last;
        break;
      case "rails:false":
      case "rails:true":
        filters.rails = last;
        break;
      case "branch:false":
      case "branch:true":
        filters.branch = last;
        break;
      case "touches:false":
      case "touches:true":
        filters.touches = last;
        break;
      case "by:false":
      case "by:true":
        filters.by = last;
        break;
      case "agent:false":
      case "agent:true":
        filters.agent = last;
        break;
      case "sort:false":
      case "sort:true":
        sort = last === "path" ? "path" : "relevance";
        break;
      case "explain:false":
      case "explain:true":
        explain = last === "1" || last === "yes" || last === "true";
        break;
      case "group:false":
      case "group:true":
        group = (GROUP_KEYS as string[]).includes(last) ? (last as GroupKey) : "none";
        break;
      case "facets:false":
      case "facets:true":
        facets = last === "1" || last === "yes" || last === "true";
        break;
      default:
        break;
    }
  }
  return { terms, filters, sort, explain, group, facets, diagnostics };
}

function joinTerms(terms: QueryTerm[]): string {
  return terms.map((t) => t.text).join(" ");
}

function finish(lanes: Lane[], e: Extracted, textMode: TextMode): ParsedQuery {
  return {
    lanes,
    query: joinTerms(e.terms),
    terms: e.terms,
    filters: e.filters,
    text_mode: textMode,
    sort: e.sort,
    explain: e.explain,
    group: e.group,
    facets: e.facets,
    diagnostics: e.diagnostics,
    normalized: "",
  };
}

/// Does `pattern` compile as a regex? See the module doc's ONE documented
/// divergence: this is ECMAScript's answer, the daemon's is Rust's.
function compilesAsRegex(pattern: string): boolean {
  try {
    // eslint-disable-next-line no-new
    new RegExp(pattern);
    return true;
  } catch {
    return false;
  }
}

function parseTextPrefix(rest: string): ParsedQuery {
  const close = rest.indexOf("/");
  let out: ParsedQuery;
  if (close >= 0) {
    const pattern = rest.slice(0, close);
    const after = rest.slice(close + 1);
    const flagsMatch = /^[A-Za-z]*/.exec(after);
    const flags = flagsMatch ? flagsMatch[0] : "";
    const e = extract(after.slice(flags.length).replace(/^\s+/, ""));
    if (flags.includes("i") && e.filters.case === null) e.filters.case = false;
    e.terms = [{ text: pattern, quoted: false }];
    out = finish(["text"], e, "regex");
  } else {
    const e = extract(rest);
    out = finish(["text"], e, compilesAsRegex(joinTerms(e.terms)) ? "regex" : "literal");
  }
  out.normalized = normalize(out);
  return out;
}

/// Parse one raw query string. TOTAL: every input, including `""`, produces a
/// `ParsedQuery` — never throws.
export function parse(raw: string): ParsedQuery {
  const trimmed = raw.trim();
  let out: ParsedQuery;
  if (trimmed.startsWith("~~")) {
    out = finish(["transcripts"], extract(trimmed.slice(2).replace(/^\s+/, "")), "literal");
  } else if (trimmed.startsWith("~")) {
    out = finish(["sessions"], extract(trimmed.slice(1).replace(/^\s+/, "")), "literal");
  } else if (trimmed.startsWith("@")) {
    out = finish(["symbols"], extract(trimmed.slice(1).replace(/^\s+/, "")), "literal");
  } else if (trimmed.startsWith("#")) {
    out = finish(["files"], extract(trimmed.slice(1).replace(/^\s+/, "")), "literal");
  } else if (trimmed.startsWith("/")) {
    return parseTextPrefix(trimmed.slice(1));
  } else if (
    trimmed.startsWith("?nl") &&
    (trimmed.length === 3 || /\s/.test(trimmed.charAt(3)))
  ) {
    out = finish(["semantic"], extract(trimmed.slice(3).replace(/^\s+/, "")), "literal");
  } else {
    // "?nlfoo" — not a valid `?nl` prefix (no word boundary); the whole
    // string routes to every lane, verbatim.
    out = finish([...LANE_ORDER], extract(trimmed), "literal");
  }
  out.normalized = normalize(out);
  return out;
}

function quoteIfNeeded(v: string): string {
  return v.includes(" ") ? `"${v}"` : v;
}

function renderTerms(terms: QueryTerm[]): string {
  return terms.map((t) => (t.quoted || t.text.includes(" ") ? `"${t.text}"` : t.text)).join(" ");
}

/// Canonical re-rendering — `grammar.rs`'s `normalize`. Terms in author
/// order, then filters in `FILTER_SPECS` order (positives before negatives),
/// so two queries that MEAN the same thing render identically. Parsing the
/// result yields the same query (a fixed point).
export function normalize(p: ParsedQuery): string {
  let out = "";
  const single = p.lanes.length === 1 ? p.lanes[0] : null;
  if (single === "text") {
    out += "/" + p.query;
    // Only a REGEX query closes the delimiter: the delimited form is ALWAYS
    // regex, so closing it on a literal would re-parse differently.
    if (p.text_mode === "regex") {
      out += "/";
      if (p.filters.case === false) out += "i";
    }
  } else if (single) {
    out += LANE_PREFIX[single] ?? "";
    out += renderTerms(p.terms);
  } else {
    out += renderTerms(p.terms);
  }

  const parts: string[] = [];
  const pushMulti = (key: string, values: string[]) => {
    if (values.length > 0) parts.push(`${key}:${values.map(quoteIfNeeded).join("|")}`);
  };
  for (const spec of FILTER_SPECS) {
    switch (spec.key) {
      case "lang":
        if (p.filters.lang !== null) parts.push(`lang:${quoteIfNeeded(p.filters.lang)}`);
        pushMulti("-lang", p.filters.not_lang);
        break;
      case "path":
        if (p.filters.path !== null) parts.push(`path:${quoteIfNeeded(p.filters.path)}`);
        pushMulti("-path", p.filters.not_path);
        break;
      case "repo":
        if (p.filters.repo !== null) parts.push(`repo:${quoteIfNeeded(p.filters.repo)}`);
        break;
      case "case":
        if (p.filters.case === true) parts.push("case:yes");
        else if (p.filters.case === false && !(single === "text" && p.text_mode === "regex"))
          parts.push("case:no");
        break;
      case "ext":
        pushMulti("ext", p.filters.ext);
        pushMulti("-ext", p.filters.not_ext);
        break;
      case "kind":
        pushMulti("kind", p.filters.kind);
        pushMulti("-kind", p.filters.not_kind);
        break;
      case "sort":
        if (p.sort === "path") parts.push("sort:path");
        else if (p.sort === "relevance") parts.push("sort:relevance");
        break;
      case "explain":
        if (p.explain) parts.push("explain:1");
        break;
      case "group":
        if (p.group !== null) parts.push(`group:${p.group}`);
        break;
      case "facets":
        if (p.facets) parts.push("facets:1");
        break;
      case "model":
        if (p.filters.model !== null) parts.push(`model:${quoteIfNeeded(p.filters.model)}`);
        break;
      case "controller":
        if (p.filters.controller !== null)
          parts.push(`controller:${quoteIfNeeded(p.filters.controller)}`);
        break;
      case "action":
        if (p.filters.action !== null) parts.push(`action:${quoteIfNeeded(p.filters.action)}`);
        break;
      case "route":
        if (p.filters.route !== null) parts.push(`route:${quoteIfNeeded(p.filters.route)}`);
        break;
      case "job":
        if (p.filters.job !== null) parts.push(`job:${quoteIfNeeded(p.filters.job)}`);
        break;
      case "rails":
        if (p.filters.rails !== null) parts.push(`rails:${quoteIfNeeded(p.filters.rails)}`);
        break;
      case "branch":
        if (p.filters.branch !== null) parts.push(`branch:${quoteIfNeeded(p.filters.branch)}`);
        break;
      case "touches":
        if (p.filters.touches !== null) parts.push(`touches:${quoteIfNeeded(p.filters.touches)}`);
        break;
      case "by":
        if (p.filters.by !== null) parts.push(`by:${quoteIfNeeded(p.filters.by)}`);
        break;
      case "agent":
        if (p.filters.agent !== null) parts.push(`agent:${quoteIfNeeded(p.filters.agent)}`);
        break;
      default:
        break;
    }
  }
  for (const part of parts) {
    if (out !== "" && !out.endsWith(" ")) out += " ";
    out += part;
  }
  return out.trim();
}
