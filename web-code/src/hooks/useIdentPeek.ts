// "Resolve this identifier" for a surface that is NOT a CM6 buffer (V74-L2).
//
// `routes/Reader.tsx`'s `handleGotoDef` owns the buffer's version of this; a
// board card cannot reuse that function itself (it closes over two panes, a
// vim layer and an inline-peek widget the board has none of), so what IS
// shared is the part that decides WHAT IS ASKED: `lib/identResolve.ts`'s
// `identAtColumn` + `resolveQueryFor`, pinned by `identResolve.test.ts`'s
// golden. Anything else would be a second, quietly-different question about
// the same character.
//
// One rung of the reader's ladder is structurally unavailable here and is
// named rather than faked: `gd`'s SINGLE-candidate case opens an INLINE peek
// (a CM6 block widget under the caret line, V70-A6 §P7). A board card is not
// an editor, so this hook opens the peek PANEL for one candidate too, with the
// panel's own `Enter` as the navigation. Nothing is guessed either way — the
// candidate set on screen is the daemon's, whatever its size.

import { useCallback, useReducer, useRef, useState } from "react";
import { fetchResolve, isAbortError } from "../api/client";
import { identInSnippet, resolveQueryFor, type IdentPos } from "../lib/identResolve";
import {
  initialPeekState,
  peekReducer,
  resolveCandidateToRow,
  type PeekRow,
} from "../lib/peekState";
import type { PeekAnchor } from "../components/peek/PeekPanel";

export interface IdentPeek {
  state: ReturnType<typeof peekReducer>;
  anchor: PeekAnchor | null;
  /// Open the panel for an identifier inside a SNIPPET. `snippetStart` is the
  /// file line the snippet's first line shows.
  openInSnippet(args: {
    repo: string;
    path: string;
    snippetStart: number;
    lineIndex: number;
    lineText: string;
    col: number;
    anchor: PeekAnchor | null;
  }): void;
  move(delta: number): void;
  close(): void;
  /// The row the panel activated, as a location — the caller navigates.
  rowLocation(row: PeekRow): { repo: string; path: string; line: number };
}

export function useIdentPeek(): IdentPeek {
  const [state, dispatch] = useReducer(peekReducer, initialPeekState);
  const [anchor, setAnchor] = useState<PeekAnchor | null>(null);
  const reqRef = useRef(0);

  const openInSnippet = useCallback<IdentPeek["openInSnippet"]>((args) => {
    const pos: IdentPos | null = identInSnippet({
      snippetStart: args.snippetStart,
      lineIndex: args.lineIndex,
      lineText: args.lineText,
      col: args.col,
    });
    // No word under the column is an honest nothing — never a resolve for a
    // nearby identifier the human did not point at.
    if (!pos) return;
    setAnchor(args.anchor);
    dispatch({ type: "OPEN", mode: "defs", word: pos.word });
    const reqId = ++reqRef.current;
    const query = resolveQueryFor(args.repo, args.path, pos);
    void fetchResolve(query)
      .then((out) => {
        if (reqId !== reqRef.current) return;
        dispatch({
          type: "SET_ROWS",
          rows: out.candidates.map(resolveCandidateToRow),
          approximate: false,
          note: out.note,
        });
      })
      .catch((e: unknown) => {
        if (reqId !== reqRef.current || isAbortError(e)) return;
        dispatch({
          type: "SET_ERROR",
          message: e instanceof Error ? e.message : "resolve failed",
        });
      });
  }, []);

  const move = useCallback((delta: number) => dispatch({ type: "MOVE", delta }), []);
  const close = useCallback(() => dispatch({ type: "CLOSE" }), []);
  const rowLocation = useCallback(
    (row: PeekRow) => ({ repo: row.repo, path: row.path, line: row.line }),
    [],
  );

  return { state, anchor, openInSnippet, move, close, rowLocation };
}
