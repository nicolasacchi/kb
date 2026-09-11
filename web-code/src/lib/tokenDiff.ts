// V76-R2c — intra-line token diff for suggestion previews.
//
// Suggestions used to paint whole added/removed lines red/green. The
// operator's ruling is that a suggestion must show the CHANGE: LCS over
// tokens (identifiers, punctuation, whitespace runs), unchanged lines
// dimmed, changed TOKENS emphasised. Large suggestions (> 200 lines) fall
// back to line-level with a caption that says so.
//
// Pure, LLM-free. The CM6 suggestion EDITOR is a different surface and
// does not import this module.

export const TOKEN_DIFF_LINE_FALLBACK = 200;

/// Same split `splitSuggestionLines` / `splitContentLines` use — a trailing
/// `\n` is a terminator, not an extra empty line. Kept local so this module
/// stays a leaf (the suggestion editor must not import it).
function splitLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

export type TokenKind = "ident" | "punct" | "ws";

export interface Token {
  text: string;
  kind: TokenKind;
}

export type TokenOpKind = "eq" | "add" | "del";

export interface TokenOp {
  kind: TokenOpKind;
  text: string;
  tokenKind: TokenKind;
  /// True when this op is a suffix-only edit on its line (`foo` → `foo #test`).
  trailing?: boolean;
}

export type SuggestionLineKind = "eq" | "add" | "del" | "replace";

export interface SuggestionDiffLine {
  kind: SuggestionLineKind;
  oldText: string | null;
  newText: string | null;
  ops: TokenOp[];
  /// At least one op on this line is a trailing suffix edit.
  trailing: boolean;
}

/// One painted row of a suggestion preview. A `replace` line becomes TWO
/// rows (old then new) so each row's text is the full line verbatim —
/// word-diff over full lines, never a spliced line.
export type SuggestionRenderSide = "eq" | "old" | "new";

export interface SuggestionRenderRow {
  side: SuggestionRenderSide;
  text: string;
  ops: TokenOp[];
  trailing: boolean;
}

/// Split a logical suggestion line into the rows the preview paints.
/// Old rows keep eq+del tokens; new rows keep eq+add tokens; `ops`
/// concatenated equal `text`.
export function suggestionRenderRows(line: SuggestionDiffLine): SuggestionRenderRow[] {
  if (line.kind === "eq") {
    const text = line.newText ?? line.oldText ?? "";
    return [
      {
        side: "eq",
        text,
        ops: line.ops.filter((o) => o.kind === "eq"),
        trailing: false,
      },
    ];
  }
  if (line.kind === "add") {
    const text = line.newText ?? "";
    const ops = line.ops.filter((o) => o.kind !== "del");
    return [{ side: "new", text, ops, trailing: ops.some((o) => o.trailing) }];
  }
  if (line.kind === "del") {
    const text = line.oldText ?? "";
    const ops = line.ops.filter((o) => o.kind !== "add");
    return [{ side: "old", text, ops, trailing: ops.some((o) => o.trailing) }];
  }
  const oldOps = line.ops.filter((o) => o.kind !== "add");
  const newOps = line.ops.filter((o) => o.kind !== "del");
  return [
    {
      side: "old",
      text: line.oldText ?? "",
      ops: oldOps,
      trailing: oldOps.some((o) => o.trailing),
    },
    {
      side: "new",
      text: line.newText ?? "",
      ops: newOps,
      trailing: newOps.some((o) => o.trailing),
    },
  ];
}

export type SuggestionDiffMode = "token" | "line";

export interface SuggestionDiffView {
  mode: SuggestionDiffMode;
  lines: SuggestionDiffLine[];
  linesChanged: number;
  tokensChanged: number;
  added: number;
  deleted: number;
  caption: string;
}

const IDENT = /[A-Za-z_][A-Za-z0-9_]*|[0-9]+/y;
const WS = /[ \t]+/y;

/// Split `text` into identifier / whitespace-run / single-char punctuation
/// tokens. Newlines are not expected (callers split lines first); a `\n`
/// is punctuation so a slipped newline still round-trips.
export function tokenize(text: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < text.length) {
    IDENT.lastIndex = i;
    const ident = IDENT.exec(text);
    if (ident && ident.index === i) {
      out.push({ text: ident[0], kind: "ident" });
      i += ident[0].length;
      continue;
    }
    WS.lastIndex = i;
    const ws = WS.exec(text);
    if (ws && ws.index === i) {
      out.push({ text: ws[0], kind: "ws" });
      i += ws[0].length;
      continue;
    }
    out.push({ text: text[i], kind: "punct" });
    i += 1;
  }
  return out;
}

interface LcsStep<T> {
  op: "eq" | "add" | "del";
  a?: T;
  b?: T;
}

function lcsSteps<T>(a: T[], b: T[], eq: (x: T, y: T) => boolean): LcsStep<T>[] {
  const n = a.length;
  const m = b.length;
  const dp: number[][] = new Array(n + 1);
  for (let i = 0; i <= n; i++) {
    dp[i] = new Array(m + 1).fill(0);
  }
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      dp[i][j] = eq(a[i - 1], b[j - 1]) ? dp[i - 1][j - 1] + 1 : Math.max(dp[i - 1][j], dp[i][j - 1]);
    }
  }
  const steps: LcsStep<T>[] = [];
  let i = n;
  let j = m;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && eq(a[i - 1], b[j - 1])) {
      steps.push({ op: "eq", a: a[i - 1], b: b[j - 1] });
      i -= 1;
      j -= 1;
    } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
      steps.push({ op: "add", b: b[j - 1] });
      j -= 1;
    } else {
      steps.push({ op: "del", a: a[i - 1] });
      i -= 1;
    }
  }
  steps.reverse();
  return steps;
}

function tokenEq(a: Token, b: Token): boolean {
  return a.kind === b.kind && a.text === b.text;
}

function markTrailing(ops: TokenOp[]): TokenOp[] {
  if (ops.length === 0) return ops;
  let lastEq = -1;
  for (let i = 0; i < ops.length; i++) {
    if (ops[i].kind === "eq") lastEq = i;
  }
  if (lastEq < 0) return ops;
  let trailing = false;
  for (let i = lastEq + 1; i < ops.length; i++) {
    if (ops[i].kind !== "eq") {
      trailing = true;
      break;
    }
  }
  if (!trailing) return ops;
  return ops.map((op, i) => (i > lastEq && op.kind !== "eq" ? { ...op, trailing: true } : op));
}

/// Token LCS of two strings (one line each). Empty vs empty is `[]`.
export function tokenDiff(oldText: string, newText: string): TokenOp[] {
  const a = tokenize(oldText);
  const b = tokenize(newText);
  const steps = lcsSteps(a, b, tokenEq);
  const ops: TokenOp[] = steps.map((s) => {
    if (s.op === "eq" && s.a) return { kind: "eq", text: s.a.text, tokenKind: s.a.kind };
    if (s.op === "add" && s.b) return { kind: "add", text: s.b.text, tokenKind: s.b.kind };
    return { kind: "del", text: s.a?.text ?? "", tokenKind: s.a?.kind ?? "punct" };
  });
  return markTrailing(ops);
}

function lineOfKind(kind: SuggestionLineKind, oldText: string | null, newText: string | null, mode: SuggestionDiffMode): SuggestionDiffLine {
  let ops: TokenOp[] = [];
  if (mode === "token") {
    if (kind === "eq" && newText != null) {
      ops = tokenize(newText).map((t) => ({ kind: "eq" as const, text: t.text, tokenKind: t.kind }));
    } else if (kind === "add" && newText != null) {
      ops = tokenize(newText).map((t) => ({ kind: "add" as const, text: t.text, tokenKind: t.kind }));
    } else if (kind === "del" && oldText != null) {
      ops = tokenize(oldText).map((t) => ({ kind: "del" as const, text: t.text, tokenKind: t.kind }));
    } else if (kind === "replace") {
      ops = tokenDiff(oldText ?? "", newText ?? "");
    }
  } else {
    // Line-level fallback: one op covering the whole line, no intra-line split.
    if (kind === "eq" && newText != null) ops = [{ kind: "eq", text: newText, tokenKind: "ident" }];
    else if (kind === "add" && newText != null) ops = [{ kind: "add", text: newText, tokenKind: "ident" }];
    else if (kind === "del" && oldText != null) ops = [{ kind: "del", text: oldText, tokenKind: "ident" }];
    else {
      const del = oldText ?? "";
      const add = newText ?? "";
      ops = [];
      if (del) ops.push({ kind: "del", text: del, tokenKind: "ident" });
      if (add) ops.push({ kind: "add", text: add, tokenKind: "ident" });
    }
  }
  return {
    kind,
    oldText,
    newText,
    ops,
    trailing: ops.some((o) => o.trailing),
  };
}

function pairLineSteps(oldLines: string[], newLines: string[]): SuggestionDiffLine[] {
  const steps = lcsSteps(oldLines, newLines, (x, y) => x === y);
  const out: SuggestionDiffLine[] = [];
  let i = 0;
  while (i < steps.length) {
    const s = steps[i];
    if (s.op === "eq" && s.a !== undefined) {
      out.push(lineOfKind("eq", s.a, s.b ?? s.a, "token"));
      i += 1;
      continue;
    }
    const dels: string[] = [];
    const adds: string[] = [];
    while (i < steps.length && steps[i].op === "del") {
      dels.push(steps[i].a ?? "");
      i += 1;
    }
    while (i < steps.length && steps[i].op === "add") {
      adds.push(steps[i].b ?? "");
      i += 1;
    }
    const n = Math.min(dels.length, adds.length);
    for (let k = 0; k < n; k++) {
      out.push(lineOfKind("replace", dels[k], adds[k], "token"));
    }
    for (let k = n; k < dels.length; k++) {
      out.push(lineOfKind("del", dels[k], null, "token"));
    }
    for (let k = n; k < adds.length; k++) {
      out.push(lineOfKind("add", null, adds[k], "token"));
    }
  }
  return out;
}

function countTokensChanged(lines: SuggestionDiffLine[]): number {
  let n = 0;
  for (const line of lines) {
    for (const op of line.ops) {
      if (op.kind !== "eq") n += 1;
    }
  }
  return n;
}

function countAddedDeleted(lines: SuggestionDiffLine[]): { added: number; deleted: number } {
  let added = 0;
  let deleted = 0;
  for (const line of lines) {
    if (line.kind === "add") added += 1;
    else if (line.kind === "del") deleted += 1;
    else if (line.kind === "replace") {
      added += 1;
      deleted += 1;
    }
  }
  return { added, deleted };
}

/// Caption for a suggestion preview. Exported so the derivation is pinned
/// independently of the LCS (a caption bug must not hide inside a golden
/// of the ops).
export function suggestionCaption(input: {
  mode: SuggestionDiffMode;
  linesChanged: number;
  tokensChanged: number;
  added: number;
  deleted: number;
  totalLines: number;
}): string {
  if (input.mode === "line") {
    return `changes ${input.linesChanged} lines · line-level (suggestion is ${input.totalLines}+ lines) · +${input.added} −${input.deleted}`;
  }
  if (input.linesChanged === 0 && input.tokensChanged === 0) return "identical";
  return `changes ${input.linesChanged} lines · ${input.tokensChanged} tokens · +${input.added} −${input.deleted}`;
}

/// Align `original` vs `replacement` (suggestion text, `\n`-joined) into a
/// renderable view. `> TOKEN_DIFF_LINE_FALLBACK` lines skip the token LCS.
export function suggestionDiff(original: string, replacement: string): SuggestionDiffView {
  const oldLines = splitLines(original);
  const newLines = splitLines(replacement);
  const totalLines = Math.max(oldLines.length, newLines.length);
  const mode: SuggestionDiffMode = totalLines > TOKEN_DIFF_LINE_FALLBACK ? "line" : "token";

  let lines: SuggestionDiffLine[];
  if (mode === "line") {
    const steps = lcsSteps(oldLines, newLines, (x, y) => x === y);
    lines = [];
    for (const s of steps) {
      if (s.op === "eq" && s.a !== undefined) lines.push(lineOfKind("eq", s.a, s.b ?? s.a, "line"));
      else if (s.op === "del") lines.push(lineOfKind("del", s.a ?? "", null, "line"));
      else lines.push(lineOfKind("add", null, s.b ?? "", "line"));
    }
  } else {
    lines = pairLineSteps(oldLines, newLines);
  }

  const linesChanged = lines.filter((l) => l.kind !== "eq").length;
  const tokensChanged = mode === "token" ? countTokensChanged(lines) : 0;
  const { added, deleted } = countAddedDeleted(lines);
  return {
    mode,
    lines,
    linesChanged,
    tokensChanged,
    added,
    deleted,
    caption: suggestionCaption({
      mode,
      linesChanged,
      tokensChanged,
      added,
      deleted,
      totalLines: TOKEN_DIFF_LINE_FALLBACK,
    }),
  };
}
