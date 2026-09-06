// V70-A5 — ONE dispatcher for kb-code's whole keyboard surface.
//
// Before this module there were 23 hand-rolled keyboard surfaces and THREE
// chord machines with three different timeout policies (the CM6 buffer:
// never expires; `ReviewDiff.tsx`: 500 ms; kb's own SPA: 800 ms). This file
// is the pure half of the replacement: a key-sequence resolver and a chord
// state machine, both DOM-free, timer-free and unit-testable. The impure
// half is `CommandRoot.tsx` — one window listener that owns the single
// pending-chord state and the which-key timer.
//
// TWO-LAYER OWNERSHIP, ported from kb's `web/src/lib/keymap.ts` (whose own
// module doc says it was modeled on web-code's `editor/vimKeys.ts` — the
// idea finally flows back): only rows with `dispatch: "central"` are fired
// by `CommandRoot`. Every other row is declared here for the `?` sheet, the
// palette, the which-key overlay and the conflicts gate, while a
// route/component-local handler (or the CM6 vim layer, whose `VimCommand`
// kinds map 1:1 onto registry ids — see `vimParity.test.ts`) still owns real
// execution. That is why `resolve()` is scope-parameterised rather than
// hard-wired to `global`: the CLI's `kb-code commands explain` runs THIS
// algorithm over any scope, and the two implementations are pinned against
// each other by the goldens in `dispatch.test.ts` and the CLI's own tests.
//
// CHORD POLICY (one, finally): a prefix NEVER expires by itself — vim's own
// answer, and the one the reader already had. `Escape` cancels. The which-key
// overlay is what a 400 ms pause buys you, not a lost keystroke.

import {
  KBC_COMMANDS,
  KBC_DEFAULT_PRESET,
  KBC_LEADER,
  KBC_SCOPES,
  type KbcCommand,
  type KbcPreset,
  type KbcScope,
} from "./registry.gen";

// ── the context-key bag ────────────────────────────────────────────────────

/// Values a `when` predicate can compare against. Booleans are the common
/// case (`help.open`); strings carry the enumerated ones (`mode`, `board`).
export type CtxValue = string | number | boolean | undefined;
export type Ctx = Readonly<Record<string, CtxValue>>;

/// One atom of a `when` predicate: `key`, `!key`, `key == value`,
/// `key != value`. Atoms are joined by `&&` — there is deliberately no `||`
/// and no parentheses. A predicate that needs disjunction is a sign the row
/// should be two rows, and keeping the grammar this small is what lets the
/// conflicts gate reason about DISJOINTNESS mechanically (below) instead of
/// asking a human whether two conditions can hold at once.
interface Atom {
  key: string;
  op: "truthy" | "falsy" | "eq" | "ne";
  value?: string;
}

export function parseWhen(when: string | undefined): Atom[] {
  if (!when) return [];
  const out: Atom[] = [];
  for (const raw of when.split("&&")) {
    const part = raw.trim();
    if (!part) continue;
    const eq = part.split("==");
    if (eq.length === 2) {
      out.push({ key: eq[0].trim(), op: "eq", value: eq[1].trim() });
      continue;
    }
    const ne = part.split("!=");
    if (ne.length === 2) {
      out.push({ key: ne[0].trim(), op: "ne", value: ne[1].trim() });
      continue;
    }
    if (part.startsWith("!")) {
      out.push({ key: part.slice(1).trim(), op: "falsy" });
      continue;
    }
    out.push({ key: part, op: "truthy" });
  }
  return out;
}

function atomHolds(a: Atom, ctx: Ctx): boolean {
  const v = ctx[a.key];
  switch (a.op) {
    case "truthy":
      return v === true || (typeof v === "string" && v.length > 0 && v !== "false");
    case "falsy":
      return !(v === true || (typeof v === "string" && v.length > 0 && v !== "false"));
    case "eq":
      return String(v) === a.value;
    case "ne":
      return String(v) !== a.value;
  }
}

/// True when every atom of `when` holds in `ctx`. An UNKNOWN key reads as
/// absent, so an unset bag makes only unconditional rows available — the
/// honest degrade: a surface that has not published its context can never
/// accidentally fire a gated command.
export function evalWhen(when: string | undefined, ctx: Ctx): boolean {
  return parseWhen(when).every((a) => atomHolds(a, ctx));
}

/// Two predicates are PROVABLY disjoint when one asserts an atom the other
/// negates (`diff.menu` vs `!diff.menu`, `mode == normal` vs
/// `mode != normal`). Used by the conflicts gate: two rows that can never be
/// available at the same instant do not collide, however identical their
/// keys. Deliberately conservative — "not provably disjoint" is treated as
/// "can collide", so an unratified pair fails the build rather than being
/// waved through by a cleverness the humans did not write down.
export function whenDisjoint(a: string | undefined, b: string | undefined): boolean {
  const A = parseWhen(a);
  const B = parseWhen(b);
  for (const x of A) {
    for (const y of B) {
      if (x.key !== y.key) continue;
      if (x.op === "truthy" && y.op === "falsy") return true;
      if (x.op === "falsy" && y.op === "truthy") return true;
      if (x.op === "eq" && y.op === "eq" && x.value !== y.value) return true;
      if (x.op === "eq" && y.op === "ne" && x.value === y.value) return true;
      if (x.op === "ne" && y.op === "eq" && x.value === y.value) return true;
    }
  }
  return false;
}

// ── key tokens ─────────────────────────────────────────────────────────────

/// A key SEQUENCE is space-separated tokens: `"g d"`, `"Ctrl-w v"`,
/// `"Space g h"`. A token is optional `Ctrl-`/`Alt-`/`Shift-`/`Meta-`
/// modifiers plus one key name (`Escape`, `Enter`, `Space`, `Tab`, `Left`,
/// an F-key) or one literal character (case-significant: `K` means shift-k).
export function tokensOf(sequence: string): string[] {
  return sequence.split(" ").filter(Boolean);
}

/// Wildcard tokens stand for a class of keystrokes: `{a-z}` (a mark letter),
/// `{1-9}`/`{0-9}` (a count or a drawer tab). They match at RESOLVE time and
/// are rendered literally in the sheet — 26 rows for 26 marks would drown the
/// table the sheet exists to make readable.
export function wildcardMatches(token: string, key: string): boolean {
  if (token === "{a-z}") return /^[a-z]$/.test(key);
  if (token === "{1-9}") return /^[1-9]$/.test(key);
  if (token === "{0-9}") return /^[0-9]$/.test(key);
  return false;
}

function tokenMatches(token: string, key: string): boolean {
  return token === key || wildcardMatches(token, key);
}

/// `KeyboardEvent` → one canonical token. Modifier order is fixed
/// (Ctrl, Alt, Shift, Meta) so a registry string and a live keypress can be
/// compared as strings.
///
/// `Shift` is folded into the CHARACTER for printable keys (`K`, not
/// `Shift-k`) — that is how vim writes it and how the registry is authored —
/// but kept as an explicit modifier for named keys (`Shift-Enter`,
/// `Shift-Tab`), where there is no shifted character to fold into.
export function tokenOf(e: {
  key: string;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  metaKey?: boolean;
}): string {
  let name = e.key;
  if (name === " ") name = "Space";
  else if (name === "ArrowLeft") name = "Left";
  else if (name === "ArrowRight") name = "Right";
  else if (name === "ArrowUp") name = "Up";
  else if (name === "ArrowDown") name = "Down";
  const printable = name.length === 1;
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey && !printable) parts.push("Shift");
  if (e.metaKey) parts.push("Meta");
  parts.push(name);
  return parts.join("-");
}

/// The first token's "bare-ness" — a token with no modifier. The typing guard
/// and the reader-buffer carve-out both key on this: inside the read-only CM6
/// buffer the vim layer owns every bare key, so `CommandRoot` only dispatches
/// modified ones there (which is what keeps ⌘K working while browsing code).
export function isBareToken(token: string): boolean {
  return !/^(Ctrl|Alt|Meta)-/.test(token);
}

// ── the resolver ───────────────────────────────────────────────────────────

const SCOPE_BY_ID = new Map(KBC_SCOPES.map((s) => [s.id, s]));

export function scopeDepth(scope: KbcScope): number {
  return SCOPE_BY_ID.get(scope)?.depth ?? 0;
}

/// The scopes whose rows are in play for `scope`: the scope itself plus
/// `global`. Deeper scopes are NOT included — a surface asks about itself,
/// and `CommandRoot` is told which scope is active.
function inPlay(c: KbcCommand, scope: KbcScope): boolean {
  return c.scope === scope || c.scope === "global";
}

export function keysFor(c: KbcCommand, preset: KbcPreset): readonly string[] {
  return c.keys[preset] ?? [];
}

/// The key the sheet and the palette DISPLAY for a command in a preset — the
/// first column entry, or `null` when the preset leaves it palette-only. An
/// empty column is a real answer ("no key here"), never a silent fall back to
/// vim: presets are columns, not inheritance (§P2).
export function displayKey(c: KbcCommand, preset: KbcPreset): string | null {
  return keysFor(c, preset)[0] ?? null;
}

/// Every command available in `scope` under `ctx` — the palette's row set and
/// the sheet's contents. Ordered by the registry's own iteration order, which
/// is authored reading-order.
export function commandsForScope(scope: KbcScope, ctx: Ctx = {}): KbcCommand[] {
  return KBC_COMMANDS.filter((c) => inPlay(c, scope) && evalWhen(c.when, ctx));
}

/// THE pure resolver. `sequence` is a full key sequence (`"g d"`); the result
/// is the one command it names in `scope` under `ctx`, or `null`.
///
/// Precedence, in order:
///   1. scope-specific over `global` (a surface may not redeclare a global
///      key — the shadowing lint enforces that — but `Escape` is exempt, and
///      the dismiss stack resolves by `dismissOrder` instead, below);
///   2. for `Escape`, the LOWEST `dismissOrder` whose `when` holds, i.e.
///      innermost first. Esc never navigates, so this is always a dismissal;
///   3. registry order.
export function resolve(
  sequence: string,
  scope: KbcScope,
  ctx: Ctx = {},
  preset: KbcPreset = KBC_DEFAULT_PRESET,
): KbcCommand | null {
  const seq = tokensOf(sequence);
  if (seq.length === 0) return null;
  const hits = KBC_COMMANDS.filter(
    (c) =>
      inPlay(c, scope) &&
      evalWhen(c.when, ctx) &&
      keysFor(c, preset).some((k) => {
        const t = tokensOf(k);
        return t.length === seq.length && t.every((tok, i) => tokenMatches(tok, seq[i]));
      }),
  );
  if (hits.length === 0) return null;
  if (seq.length === 1 && seq[0] === "Escape") {
    return hits.reduce((best, c) =>
      (c.dismissOrder ?? Number.MAX_SAFE_INTEGER) < (best.dismissOrder ?? Number.MAX_SAFE_INTEGER)
        ? c
        : best,
    );
  }
  return hits.find((c) => c.scope === scope) ?? hits[0];
}

/// Every command whose key sequence EXTENDS `prefix` — what the which-key
/// overlay lists, and what tells the chord machine to keep waiting. Sorted
/// group-then-key so the overlay reads the same way every time.
export function continuations(
  prefix: readonly string[],
  scope: KbcScope,
  ctx: Ctx = {},
  preset: KbcPreset = KBC_DEFAULT_PRESET,
): Array<{ command: KbcCommand; next: string; rest: string[] }> {
  const out: Array<{ command: KbcCommand; next: string; rest: string[] }> = [];
  for (const c of KBC_COMMANDS) {
    if (!inPlay(c, scope) || !evalWhen(c.when, ctx)) continue;
    for (const k of keysFor(c, preset)) {
      const t = tokensOf(k);
      if (t.length <= prefix.length) continue;
      if (!prefix.every((p, i) => tokenMatches(t[i], p))) continue;
      out.push({ command: c, next: t[prefix.length], rest: t.slice(prefix.length) });
      break;
    }
  }
  out.sort((a, b) =>
    a.command.group === b.command.group
      ? a.next.localeCompare(b.next)
      : a.command.group.localeCompare(b.command.group),
  );
  return out;
}

// ── the chord machine ──────────────────────────────────────────────────────

export interface ChordState {
  /// Tokens typed so far in the in-flight sequence (`[]` = idle).
  readonly pending: readonly string[];
  /// A vim count typed before a motion, kept as a STRING so a bare `0`
  /// (line-start) falls out naturally — `vimKeys.ts`'s own trick.
  readonly count: string;
}

export const IDLE: ChordState = { pending: [], count: "" };

export type StepResult =
  | { kind: "matched"; command: KbcCommand; count: number | null; state: ChordState }
  | { kind: "pending"; state: ChordState }
  | { kind: "cancelled"; state: ChordState }
  | { kind: "none"; state: ChordState };

/// One keystroke through the machine.
///
/// - `matched`   — the sequence named a command; state resets.
/// - `pending`   — a longer binding shares this prefix; hold (and raise
///                 which-key after 400 ms). A prefix NEVER expires on its own.
/// - `cancelled` — `Escape` collapsed an in-flight sequence. Distinct from
///                 `none` so the caller can swallow the key WITHOUT also
///                 firing the dismiss stack: cancelling a chord is what that
///                 Escape did, and dismissing a panel underneath it too would
///                 be one keystroke doing two things.
/// - `none`      — nothing matches and nothing extends; the sequence fizzled.
export function step(
  state: ChordState,
  token: string,
  scope: KbcScope,
  ctx: Ctx = {},
  preset: KbcPreset = KBC_DEFAULT_PRESET,
): StepResult {
  if (token === "Escape" && state.pending.length > 0) {
    return { kind: "cancelled", state: IDLE };
  }
  // A leading digit is a COUNT — unless this scope binds the bare digit as a
  // command, which the review cockpit does (`1`…`5` = its tabs). A count
  // accumulator that silently ate them would make those rows unreachable
  // while looking, from the registry, like they existed: exactly the class of
  // lie this unit is here to remove. So the command wins, and a scope that
  // wants counts simply does not bind bare digits (the reader does not).
  const digitIsCommand =
    state.pending.length === 0 &&
    /^[0-9]$/.test(token) &&
    resolve(token, scope, ctx, preset) !== null;
  if (state.pending.length === 0 && /^[1-9]$/.test(token) && !digitIsCommand) {
    return { kind: "pending", state: { pending: [], count: state.count + token } };
  }
  // `0` is line-start when no count is building — vim's rule, and the reason
  // `count` is kept as a string.
  if (state.pending.length === 0 && token === "0" && state.count !== "") {
    return { kind: "pending", state: { pending: [], count: state.count + token } };
  }

  const seq = [...state.pending, token];
  const exact = resolve(seq.join(" "), scope, ctx, preset);
  if (exact) {
    const leadingCount = state.count === "" ? null : Number(state.count);
    // V71-K3 — `drawer.tab`'s `Space {1-9}` is the first `dispatch:
    // "central"` row to bind a NON-leading wildcard digit. The leading-
    // count accumulator above only ever populates `state.count` when a
    // digit is the SEQUENCE'S OWN FIRST token (`state.pending.length ===
    // 0`); `Space` is typed first here, so `3` in `Space 3` never touches
    // it and `count` would otherwise come back `null` — leaving the
    // handler no way to know which of nine tabs was asked for. Recovered
    // from the WINNING key template itself, never from "the last token
    // happened to be a digit": `wildcardDigit` only returns non-null when
    // that template names `{1-9}`/`{0-9}` at the matching position, so a
    // command bound to a literal digit key (the review cockpit's bare
    // `1`…`5` tabs) is untouched — its template has no wildcard token to
    // match.
    const count = leadingCount ?? wildcardDigit(exact, seq, preset);
    return { kind: "matched", command: exact, count, state: IDLE };
  }
  if (continuations(seq, scope, ctx, preset).length > 0) {
    return { kind: "pending", state: { pending: seq, count: state.count } };
  }
  return { kind: "none", state: IDLE };
}

/// The literal digit the matched command's OWN key template wildcarded at
/// this position, or `null` when the template that matched (for this
/// exact `seq` length) named no `{1-9}`/`{0-9}` token at all. See `step`'s
/// call site for why this exists and what it must never do.
function wildcardDigit(command: KbcCommand, seq: readonly string[], preset: KbcPreset): number | null {
  for (const k of keysFor(command, preset)) {
    const t = tokensOf(k);
    if (t.length !== seq.length) continue;
    if (!t.every((tok, i) => tokenMatches(tok, seq[i]))) continue;
    for (let i = 0; i < t.length; i++) {
      if ((t[i] === "{1-9}" || t[i] === "{0-9}") && /^[0-9]$/.test(seq[i])) return Number(seq[i]);
    }
  }
  return null;
}

/// The human rendering of an in-flight sequence, for the status pip: the
/// count then the tokens, exactly what was typed (`"12g"`, `"Space g"`).
export function pendingLabel(state: ChordState): string {
  const keys = state.pending.map((t) => (t === "Space" ? KBC_LEADER : t)).join(" ");
  return state.count + (state.count && keys ? " " : "") + keys;
}
