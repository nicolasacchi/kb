// V74-L3c — pure URL state for the `kbc-recipe/1` recipe HOME
// (`/r/:repo/~recipes[?slug=&intent=&scope=&limit=&view=&p.<name>=&ctx.<field>=]`
// and `/r/:repo/~recipes/runs/:id`). A SEPARATE grammar from the old, frozen
// `lib/recipesUrl.ts` (`?recipe=&since=&limit=`) — the two never share a
// query key, so a bookmarked old-style link and a new-style link can sit on
// the same route without either parser misreading the other's params.
//
// One builder (`recipeRunUrl`) + one parser (`parseRecipeSearch`), same
// "golden-pinned outputs" discipline as `codeUrl.ts` (root CLAUDE.md
// invariant #35's kb-side twin). Param order is always: `slug`, `intent`,
// `scope`, `limit`, `view`, then every `p.<name>=` sorted by name, then every
// `ctx.<field>=` sorted by name — deterministic regardless of object key
// insertion order, so a reload's URL never silently reshuffles.

import { codeBasePath } from "./codeUrl";
import type { KbcParamSpec, KbcParamType } from "../api/types";

const RECIPES_SEGMENT = "~recipes";
const RUNS_SEGMENT = "runs";

/// `/r/{repo}/~recipes` — the recipe home / form / live-run URL base.
/// (`lib/codeUrl.ts`'s `recipesPageUrl` already exports this same string for
/// the OLD grammar's callers; this local copy avoids a cross-import cycle
/// and is byte-identical by construction — both are `codeBasePath + "/~recipes"`.)
export function recipeHomeUrl(repo: string): string {
  return `${codeBasePath(repo, "")}/${RECIPES_SEGMENT}`;
}

/// `/r/{repo}/~recipes/runs/{id}` — a materialised run replay.
export function recipeReplayUrl(repo: string, id: string): string {
  return `${recipeHomeUrl(repo)}/${RUNS_SEGMENT}/${encodeURIComponent(id)}`;
}

export interface RecipeUrlState {
  /// Which recipe is selected. Absent ⇒ the home (intent groups only).
  slug?: string;
  /// Which intent group section is expanded on the home. Purely a display
  /// preference — never sent to the server.
  intent?: string;
  /// A kbc-scope/1 expression OVERRIDING the recipe's own `scope` field.
  /// Absent ⇒ the recipe's own default applies server-side.
  scope?: string;
  limit?: number;
  /// The currently displayed `KbcViewRun.id` (client-side selection over an
  /// already-fetched run — see `Recipes.tsx`'s doc for why this is never
  /// re-fetched from the server on a switch).
  view?: string;
  /// Typed param overrides, `name -> raw string value` (server does the
  /// typed parse; `recipeParamCoerce`/`recipeParamValidate` below mirror its
  /// range checks client-side so a bad value never round-trips to a 400
  /// the user could have caught locally).
  params: Record<string, string>;
  /// `$context` field overrides (`repo|path|symbol|ref|scope`), `ctx.<field>=`
  /// on the wire. Deliberately NOT the same key space as `scope` above —
  /// `scope=` replaces the recipe's scope expression wholesale, `ctx.scope=`
  /// only supplies the VALUE a recipe step's own `$context.scope` resolves
  /// to when the recipe's `scope` field is literally `"$context.scope"`
  /// (docs/kb-code.md's Recipes section).
  ctx: Record<string, string>;
}

export function emptyRecipeUrlState(): RecipeUrlState {
  return { params: {}, ctx: {} };
}

const PARAM_PREFIX = "p.";
const CTX_PREFIX = "ctx.";

/// Build the query string (no leading `?`) for `RecipeUrlState`, empty
/// fields omitted. Exported separately from `recipeRunUrl` so `Recipes.tsx`
/// can compare the CURRENT `location.search` against a candidate state
/// without a round trip through `URL`.
export function recipeSearchString(state: RecipeUrlState): string {
  const qs = new URLSearchParams();
  if (state.slug) qs.set("slug", state.slug);
  if (state.intent) qs.set("intent", state.intent);
  if (state.scope) qs.set("scope", state.scope);
  if (state.limit !== undefined && Number.isFinite(state.limit) && state.limit > 0) {
    qs.set("limit", String(Math.floor(state.limit)));
  }
  if (state.view) qs.set("view", state.view);
  for (const name of Object.keys(state.params).sort()) {
    const v = state.params[name];
    if (v !== undefined && v !== "") qs.set(`${PARAM_PREFIX}${name}`, v);
  }
  for (const field of Object.keys(state.ctx).sort()) {
    const v = state.ctx[field];
    if (v !== undefined && v !== "") qs.set(`${CTX_PREFIX}${field}`, v);
  }
  return qs.toString();
}

/// `/r/{repo}/~recipes[?…]` — the one builder. `recipeHomeUrl(repo)` alone
/// when `state` is empty (or omitted).
export function recipeRunUrl(repo: string, state: RecipeUrlState = emptyRecipeUrlState()): string {
  const base = recipeHomeUrl(repo);
  const qs = recipeSearchString(state);
  return qs ? `${base}?${qs}` : base;
}

/// Parse `~recipes` search params back into `RecipeUrlState`. Total: junk
/// numeric input (non-finite/zero/negative `limit`) is dropped rather than
/// throwing, matching `lib/recipesUrl.ts`'s `parseRecipesSearch` convention.
export function parseRecipeSearch(search: string | URLSearchParams): RecipeUrlState {
  const sp =
    typeof search === "string"
      ? new URLSearchParams(search.startsWith("?") ? search.slice(1) : search)
      : search;
  const out = emptyRecipeUrlState();
  const slug = sp.get("slug");
  if (slug) out.slug = slug;
  const intent = sp.get("intent");
  if (intent) out.intent = intent;
  const scope = sp.get("scope");
  if (scope) out.scope = scope;
  const limitRaw = sp.get("limit");
  if (limitRaw) {
    const n = Number(limitRaw);
    if (Number.isFinite(n) && n > 0) out.limit = Math.floor(n);
  }
  const view = sp.get("view");
  if (view) out.view = view;
  for (const [key, value] of sp.entries()) {
    if (key.startsWith(PARAM_PREFIX) && key.length > PARAM_PREFIX.length) {
      out.params[key.slice(PARAM_PREFIX.length)] = value;
    } else if (key.startsWith(CTX_PREFIX) && key.length > CTX_PREFIX.length) {
      out.ctx[key.slice(CTX_PREFIX.length)] = value;
    }
  }
  return out;
}

// ── typed param coercion + client-side validation ──────────────────────────
//
// Mirrors the server's own range/required checks (docs/kb-code.md's Recipes
// section: "`min`/`max` are inclusive and a violation is a 400 naming the
// field, the value and the bound") so a bad value is caught before a round
// trip, while the server's own 400 (surfaced verbatim when it happens
// anyway — a stale client copy of the rules is not a source of truth) is
// what actually gates the run.

export interface ParamFieldError {
  name: string;
  message: string;
}

/// Coerce a raw form-input STRING to the wire representation for `p.<name>=`.
/// Bool: `"true"`/`"false"` literal (checkbox state, never `"on"`/`""`).
export function recipeParamToQueryValue(type: KbcParamType, raw: string): string {
  if (type === "bool") return raw === "true" ? "true" : "false";
  return raw;
}

/// Client-side mirror of the server's required/range/enum checks. Returns
/// `null` when the value is acceptable to SEND (an empty, non-required
/// field is acceptable — it is simply omitted from the query). Never
/// mutates; the caller decides what to do with a returned message.
export function recipeParamValidate(spec: KbcParamSpec, raw: string): string | null {
  const trimmed = raw.trim();
  if (trimmed === "") {
    return spec.required ? `${spec.name} is required` : null;
  }
  switch (spec.type) {
    case "int": {
      const n = Number(trimmed);
      if (!Number.isFinite(n) || !Number.isInteger(n)) return `${spec.name} must be a whole number`;
      if (spec.min !== undefined && n < spec.min) return `${spec.name} must be ≥ ${spec.min}`;
      if (spec.max !== undefined && n > spec.max) return `${spec.name} must be ≤ ${spec.max}`;
      return null;
    }
    case "float": {
      const n = Number(trimmed);
      if (!Number.isFinite(n)) return `${spec.name} must be a number`;
      if (spec.min !== undefined && n < spec.min) return `${spec.name} must be ≥ ${spec.min}`;
      if (spec.max !== undefined && n > spec.max) return `${spec.name} must be ≤ ${spec.max}`;
      return null;
    }
    case "bool":
      return trimmed === "true" || trimmed === "false" ? null : `${spec.name} must be true or false`;
    case "enum":
      return (spec.values ?? []).includes(trimmed)
        ? null
        : `${spec.name} must be one of ${(spec.values ?? []).join(", ")}`;
    case "string":
    case "path":
    case "symbol":
    case "ref":
    default:
      return null;
  }
}

/// Validate every param in `params` against `specs`, returning one error
/// per failing field (empty array ⇒ safe to run).
export function recipeValidateAll(
  specs: readonly KbcParamSpec[],
  params: Record<string, string>,
): ParamFieldError[] {
  const errors: ParamFieldError[] = [];
  for (const spec of specs) {
    const raw = params[spec.name] ?? "";
    const message = recipeParamValidate(spec, raw);
    if (message) errors.push({ name: spec.name, message });
  }
  return errors;
}

/// The default-value seed for a fresh form field, as a raw string (the same
/// representation `parseRecipeSearch`/`recipeParamValidate` expect).
export function recipeParamDefaultString(spec: KbcParamSpec): string {
  if (spec.default === undefined) return "";
  if (typeof spec.default === "boolean") return spec.default ? "true" : "false";
  if (Array.isArray(spec.default)) return spec.default.join(",");
  return String(spec.default);
}

// ── the copyable CLI line (D11) ─────────────────────────────────────────────

const NEEDS_QUOTING = /[\s'"$`\\!*?[\](){}|;&<>#~]/;

/// Single-quote a value for a POSIX shell one-liner IF it needs it —
/// plain identifiers/numbers stay bare so the common case reads cleanly.
function shellArg(v: string): string {
  if (v === "" || NEEDS_QUOTING.test(v)) {
    return `'${v.replace(/'/g, "'\\''")}'`;
  }
  return v;
}

export interface RecipeCliLineArgs {
  slug: string;
  repo: string;
  scope?: string;
  limit?: number;
  params: Record<string, string>;
  ctx: Record<string, string>;
  materialise?: boolean;
  saveAsSet?: string;
}

/// The copyable `kb-code recipe run …` line — always names EXACTLY what
/// would be sent by the current form state (same sort order as
/// `recipeSearchString`), so it is a faithful CLI twin of "press Run" and
/// never a stale approximation. Composed purely from state already in
/// memory — no network round trip, so it updates on every keystroke.
export function recipeCliLine(args: RecipeCliLineArgs): string {
  const parts = ["kb-code", "recipe", "run", shellArg(args.slug), "--repo", shellArg(args.repo)];
  if (args.scope) parts.push("--scope", shellArg(args.scope));
  if (args.limit !== undefined && Number.isFinite(args.limit) && args.limit > 0) {
    parts.push("--limit", String(Math.floor(args.limit)));
  }
  for (const name of Object.keys(args.params).sort()) {
    const v = args.params[name];
    if (v !== undefined && v !== "") parts.push("--p", shellArg(`${name}=${v}`));
  }
  for (const field of Object.keys(args.ctx).sort()) {
    const v = args.ctx[field];
    if (v !== undefined && v !== "") parts.push("--ctx", shellArg(`${field}=${v}`));
  }
  if (args.materialise) parts.push("--materialise");
  if (args.saveAsSet) parts.push("--save-as-set", shellArg(args.saveAsSet));
  return parts.join(" ");
}
