import { useEffect, useRef, useState } from "react";
import { fetchSearch, fetchSearchSemantic, isAbortError } from "../api/client";
import type { LaneSection, UnifiedSearchResponse } from "../api/types";
import { mergeSemanticFollowup, needsSemanticFollowup } from "../lib/omniSearch";

/// Debounce is short enough to feel reactive while coalescing rapid typing
/// into one fetch - the task brief's own number (kb's own `web/src/hooks/
/// useSearch.ts` uses 80ms against a hybrid BM25+vector daemon query;
/// kb-code's box fans out to six lanes per keystroke, so a slightly wider
/// window trims wasted round-trips without feeling laggy).
const DEBOUNCE_MS = 120;

export interface OmniSearchState {
  sections: LaneSection[];
  queryEcho: string;
  loading: boolean;
  error: string | null;
  /// V71-D2 — the WHOLE response, for the surfaces that need more than the
  /// sections: `normalized`, `diagnostics`, `facets` and `stale`. The
  /// Omnibox ignores it; the results page renders all four. `null` until
  /// the first response lands (and after an error), so a consumer can tell
  /// "nothing yet" from "a response with no facets".
  response: UnifiedSearchResponse | null;
}

const EMPTY_STATE: OmniSearchState = {
  sections: [],
  queryEcho: "",
  loading: false,
  error: null,
  response: null,
};

/// Debounced `GET /api/search` (`crates/kb-code-server/src/search/
/// unified.rs`) plus the semantic lane's staged follow-up: when the
/// response's semantic section comes back `pending: true` (the embed
/// round-trip missed `SEMANTIC_STAGE_BUDGET` - see that module's "Semantic
/// staging" doc), this hook immediately re-queries
/// `GET /api/search/semantic` for the SAME query and splices the result
/// into the already-rendered section list once it lands.
///
/// One generation counter guards BOTH requests, mirroring kb's own
/// `web/src/hooks/useSearch.ts`: a superseded query's late primary OR
/// follow-up response can never clobber a newer query's state. An EMPTY
/// query still fetches (deliberately NOT short-circuited client-side,
/// unlike kb's own `useSearch`) — `search::unified::run`'s own doc: a
/// blank `q` short-circuits SERVER-side to exactly one section (`files`,
/// via frecency-ranked recents), which is the box's documented empty-query
/// front door (item 3 of the design brief) — so the omnibox/search page
/// show recent files the instant they open, before anyone types anything.
export function useOmniSearch(
  q: string,
  repo: string | undefined,
  limit: number,
): OmniSearchState {
  const [state, setState] = useState<OmniSearchState>(EMPTY_STATE);
  const generation = useRef(0);

  useEffect(() => {
    const gen = ++generation.current;
    const ctl = new AbortController();
    const timer = setTimeout(() => {
      setState((s) => ({ ...s, loading: true, error: null }));
      fetchSearch({ q, repo, limit, signal: ctl.signal })
        .then((resp) => {
          if (gen !== generation.current) return;
          setState({
            sections: resp.sections,
            queryEcho: resp.query_echo,
            loading: false,
            error: null,
            response: resp,
          });
          if (needsSemanticFollowup(resp.sections)) {
            fetchSearchSemantic({ q, repo, limit, signal: ctl.signal })
              .then((semResp) => {
                if (gen !== generation.current) return;
                setState((s) => ({ ...s, sections: mergeSemanticFollowup(s.sections, semResp.hits) }));
              })
              .catch((e) => {
                // Best-effort - a failed follow-up leaves that ONE section
                // `pending` (its skeleton row keeps rendering) rather than
                // surfacing a box-wide error for the other five lanes.
                if (gen !== generation.current || isAbortError(e)) return;
              });
          }
        })
        .catch((e) => {
          if (gen !== generation.current) return;
          if (isAbortError(e)) return;
          setState({ sections: [], queryEcho: "", loading: false, error: String(e), response: null });
        });
    }, DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      ctl.abort();
    };
  }, [q, repo, limit]);

  return state;
}
