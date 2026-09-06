// W2.11 — a small, pure LCS line-diff for comparing two artifacts' stored
// generation prompts. There is no client-side diff library in this SPA
// (recon §3: the daemon's `similar`-crate diff façade — VersionsPanel.tsx,
// `GET /api/kb/{kb}/artifacts/{id}/diff` — compares VERSIONS of the SAME
// artifact, not two different artifacts' prompts), so rather than add a
// new server endpoint for what's a tiny, already-fetched pair of strings,
// this does the line diff locally. O(n·m) DP table — fine at the 8 KiB
// prompt cap (`kb_core::parser::PROMPT_MAX_BYTES`), at most a few hundred
// lines a side.
//
// Wire-compatible naming with the server's own diff shape (`DiffTag`/
// `DiffLine` in web/src/api/generated/) so PromptPanel's rendering can
// mirror VersionsPanel's `.vp__line--{tag}` styling convention — this type
// is NOT the generated one (a different diff engine, a different input
// shape: two whole strings, no hunk/context windowing since prompts are
// small enough to render in full), so it stays local.

export type PromptDiffTag = "equal" | "insert" | "delete";

export type PromptDiffLine = {
  tag: PromptDiffTag;
  text: string;
};

// Split on `\n` without the phantom trailing "" a plain `.split("\n")`
// produces for a string ending in a newline (e.g. "a\nb\n".split("\n") ===
// ["a", "b", ""]) — that empty final line isn't a real line the author
// wrote, so a diff over it would be pure noise.
function toLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split("\n");
  if (lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/**
 * Line-level LCS diff between `oldText` and `newText`. Deterministic: a tie
 * in the DP backtrack always resolves to "delete-before-insert" (never
 * "insert-before-delete"), so the same two inputs always render
 * identically — no re-derivation, no randomness, no clock.
 */
export function diffLines(oldText: string, newText: string): PromptDiffLine[] {
  const a = toLines(oldText);
  const b = toLines(newText);
  const n = a.length;
  const m = b.length;

  // dp[i][j] = LCS length of a[i..] and b[j..].
  const dp: number[][] = Array.from({ length: n + 1 }, () =>
    new Array<number>(m + 1).fill(0),
  );
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] =
        a[i] === b[j]
          ? dp[i + 1][j + 1] + 1
          : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }

  const out: PromptDiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ tag: "equal", text: a[i] });
      i += 1;
      j += 1;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.push({ tag: "delete", text: a[i] });
      i += 1;
    } else {
      out.push({ tag: "insert", text: b[j] });
      j += 1;
    }
  }
  while (i < n) {
    out.push({ tag: "delete", text: a[i] });
    i += 1;
  }
  while (j < m) {
    out.push({ tag: "insert", text: b[j] });
    j += 1;
  }
  return out;
}
