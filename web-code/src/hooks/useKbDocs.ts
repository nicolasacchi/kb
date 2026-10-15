import { useEffect, useRef, useState } from "react";
import { fetchKbSearch, isAbortError } from "../api/client";
import type { KbSearchResponse } from "../api/types";
import { KB_DOCS_LIMIT, shouldQueryKbDocs } from "../lib/kbDocs";

/// A little slower than the code lanes' 120ms: this one crosses to a second
/// daemon, and a half-typed word is the common case.
const DEBOUNCE_MS = 200;

export interface KbDocsState {
  /// False until the query qualifies (`shouldQueryKbDocs`); the lane is not
  /// rendered at all while it is false.
  active: boolean;
  loading: boolean;
  response: KbSearchResponse | null;
}

const IDLE: KbDocsState = { active: false, loading: false, response: null };

/// OL S1 - debounced `GET /api/kb/search` for the omnibox "Documents" lane.
/// A generation counter drops a superseded query's late answer (same shape
/// as `useOmniSearch`). A transport failure to kb-code itself degrades to the
/// same `unavailable` caption the relay uses - the lane never raises an
/// error toast, and the code lanes are unaffected.
export function useKbDocs(q: string): KbDocsState {
  const [state, setState] = useState<KbDocsState>(IDLE);
  const generation = useRef(0);

  useEffect(() => {
    const gen = ++generation.current;
    if (!shouldQueryKbDocs(q)) {
      setState(IDLE);
      return;
    }
    const ctl = new AbortController();
    setState((s) => ({ active: true, loading: true, response: s.response }));
    const timer = setTimeout(() => {
      fetchKbSearch(q.trim(), KB_DOCS_LIMIT, ctl.signal)
        .then((response) => {
          if (gen !== generation.current) return;
          setState({ active: true, loading: false, response });
        })
        .catch((e) => {
          if (gen !== generation.current || isAbortError(e)) return;
          setState({
            active: true,
            loading: false,
            response: { hits: [], kb_public_url: "", unavailable: "kb unreachable" },
          });
        });
    }, DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      ctl.abort();
    };
  }, [q]);

  return state;
}
