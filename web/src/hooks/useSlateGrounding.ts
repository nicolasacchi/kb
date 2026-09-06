// D29 (v0.42) — the slate board's groundedness captions, the FETCH half.
//
// One query per distinct (kb, path, line) a card cites, run through
// `useQueries` so a card with four refs is four cache entries and not one
// bespoke batch endpoint nobody else can reuse (`AddToListButton`'s
// `useQueries` fan-out is the precedent).
//
// #23's no-SSE-tie exception, the same one `useDocLens` documents: kb-code
// is a SEPARATE daemon with no wiring into kb's `sse` facade, so there is no
// event that could invalidate this. Finite `staleTime`, `retry: false` (a
// CORS/offline failure will not spontaneously succeed on one more attempt,
// and an immediate failure gets the honest `unknown` on screen faster), and
// nothing is ever persisted — the caption is recomputed from the projection
// and this cache on every render.
//
// Without a `code_url` NOTHING is fetched (`enabled: false`) and the board
// renders no caption at all: an absent kb-code is not an ungrounded ref.
//
// SL7f (v0.42 amendment) adds two params this hook plumbs straight through
// to `fetchPathLens`, never interpreting either itself:
//   - `repo` — the board route's own slug, sent on EVERY call. A production
//     kb-code serving 2+ repos 400s `repo_required` with no `?repo=`; the
//     slate slug IS the kb-code repo name by design (D29/SL7e), so this is
//     sent unconditionally rather than only on a prior failure — there is
//     no "retry without repo" path to accidentally reintroduce (`retry:
//     false` below already forbids any retry at all).
//   - `line` (the CALLER's post text, `card.line`) — becomes `?context=`,
//     but ONLY for a ref that itself carries a line (`ref.line !== null`):
//     a bare `path:` ref asks no line question, so it has no context to
//     answer with, and sending one would be a question this ref never
//     asked. This also means `line` must ride the query key WHENEVER
//     `ref.line !== null` (two different cards citing the same (path,
//     line) with different post text are two different questions).

import { useQueries } from "@tanstack/react-query";
import { fetchPathLens, type PathLensOut } from "../api/doclens";
import {
  groundednessOf,
  parsePathRef,
  type Groundedness,
  type PathRef,
} from "../lib/slateGrounding";

/// Same 30 s window `useDocLens` uses — one number, one rationale (a live
/// git-tree read the SSE bridge structurally cannot invalidate).
const GROUNDING_STALE_MS = 30_000;

/// `raw` ref string -> caption, for every `path:` ref that has settled.
/// A ref still in flight is ABSENT from the map (the caller renders no
/// caption yet); a ref that failed is present as `unknown`.
export type GroundingMap = ReadonlyMap<string, Groundedness>;

const EMPTY: GroundingMap = new Map();

/// `repo` and `context` (SL7f) both ride the key: a different repo or a
/// different post citing the same (path, line) is a genuinely different
/// question to kb-code, not a cache hit on the old one. `context` is
/// omitted from the key entirely when `ref.line` is null — a bare path
/// never asks about a line, so the post's own text is not part of the
/// question and must not fragment that ref's cache entry across cards.
export function groundingKey(
  kb: string,
  repo: string | null,
  ref: PathRef,
  context: string | null,
) {
  return [
    "slateGrounding",
    kb,
    repo ?? "",
    ref.path,
    ref.line ?? 0,
    ref.line !== null ? (context ?? "") : "",
  ] as const;
}

/// `raws` are the ref strings exactly as the projection carries them; only
/// the `path:` ones are queried, and each distinct (path, line) once.
/// `repo` is the slate slug (sent as `?repo=` on every call, D29/SL7e/SL7f
/// — see the module doc). `line` is the CARD's own one-line text
/// (`SlateBoardCard.line`), sent as `?context=` only for a ref that carries
/// a line.
export function useSlateGrounding(
  codeUrl: string | null | undefined,
  kb: string | null | undefined,
  repo: string | null | undefined,
  raws: readonly string[],
  line: string,
): GroundingMap {
  const enabled = !!codeUrl && !!kb;
  // Deduped in ref order, so the query list is stable across renders of the
  // same card (a reordered list would re-key every query). `line` (the
  // context text) is the SAME string for every ref on one card, so deduping
  // on (path, line-number) alone stays correct — see the module doc.
  const wanted: { raw: string; ref: PathRef }[] = [];
  const seen = new Set<string>();
  if (enabled) {
    for (const raw of raws) {
      const ref = parsePathRef(raw);
      if (!ref) continue;
      const k = `${ref.path}#${ref.line ?? 0}`;
      if (seen.has(k)) continue;
      seen.add(k);
      wanted.push({ raw, ref });
    }
  }
  const repoParam = repo || null;

  const results = useQueries({
    queries: wanted.map(({ ref }) => {
      const context = ref.line !== null ? line : null;
      return {
        queryKey: groundingKey(kb!, repoParam, ref, context),
        queryFn: ({ signal }: { signal: AbortSignal }) =>
          fetchPathLens(codeUrl!, kb!, ref.path, ref.line, repoParam, context, signal),
        staleTime: GROUNDING_STALE_MS,
        retry: false,
      };
    }),
  });

  if (wanted.length === 0) return EMPTY;

  // One pass, no memo: the map is O(refs on one card) and rebuilding it is
  // cheaper than the dependency array that would keep it stable.
  const out = new Map<string, Groundedness>();
  wanted.forEach(({ raw }, i) => {
    const r = results[i];
    if (!r || r.isPending) return;
    // An ERROR is `unknown`, never `ungrounded` — "kb-code did not answer"
    // and "kb-code says the path is gone" are different facts and the board
    // must not merge them.
    const g = r.isError ? "unknown" : groundednessOf(r.data as PathLensOut | undefined);
    out.set(raw, g);
  });
  // A DIFFERENT ref string naming the same (path, line) — `path:a.rs:141`
  // beside `path:a.rs:141-160` — shares the one answer rather than firing a
  // second identical query.
  for (const raw of raws) {
    if (out.has(raw)) continue;
    const ref = parsePathRef(raw);
    if (!ref) continue;
    const twin = wanted.find((w) => w.ref.path === ref.path && w.ref.line === ref.line);
    if (twin && out.has(twin.raw)) out.set(raw, out.get(twin.raw)!);
  }
  return out;
}
