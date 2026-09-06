// DCB W1.D — deep-link builders into kb-code's reader/search routes.
//
// kb's web/ and kb-code's web-code/ are separate npm trees with no shared
// package boundary, so this is a HAND-COPIED MIRROR of web-code's own
// encoding rules (web-code/src/lib/codeUrl.ts's `encodePathSegments`/
// `codeBasePath`/`formatLineParam`, plus Search.tsx's `?q=&repo=` shape).
// If web-code's encoding ever changes, THIS FILE must be updated by hand —
// the accepted coupling (13-w1d-kb-spa.md §7.3); a shared npm package is
// out of scope for a v1 milestone.

function encodePathSegments(path: string): string {
  return path
    .split("/")
    .filter((s) => s !== "")
    .map(encodeURIComponent)
    .join("/");
}

/// `/r/{repo}/{path}[?line=N]` — the ONE deep-link builder for a resolved
/// code reference. `line` is omitted whenever it's absent/non-finite/<=0 —
/// a `reader.line` from codelens/1 is nullable (a `present` path with no
/// line hint at all still carries a `reader`, just with `line: null`).
/// v1 never builds a range: a `path_range`/`path_list` ref deep-links to
/// its resolved START line only (kb-code's own `line=` format could carry
/// "42-50" identically to the reader's own, so this could be extended
/// later; kept to a start-line MVP here, matching 13-w1d-kb-spa.md §7.3).
export function codeReaderUrl(
  codeUrl: string,
  repo: string,
  path: string,
  line?: number | null,
): string {
  const base = codeUrl.replace(/\/+$/, "");
  const repoSeg = encodeURIComponent(repo);
  const pathSeg = encodePathSegments(path);
  const url = `${base}/r/${repoSeg}/${pathSeg}`;
  return line !== undefined && line !== null && Number.isFinite(line) && line > 0
    ? `${url}?line=${line}`
    : url;
}

/// `/search?q={query}[&repo={repo}]` — the >3-candidate and 0-candidate
/// escape hatch (Decision 3). `query`/`repo` come straight from the ref's
/// own server-computed `search: {q, repo}` field — never re-derived from
/// `path_hint` client-side.
///
/// CT-B5 — `repo` is optional (mirrors web-code's own `Search.tsx`, which
/// reads `?repo=` from the URL as `string | undefined` and searches
/// unscoped when absent): a freeform prose citation (a memory/comment/
/// session-decision mention of a path or sha) carries no resolved repo at
/// all — `crates/kb-core/src/config.rs`'s `KbSection.code_url` doc comment
/// is explicit that a singular repo name is deliberately NOT pinned in
/// config ("the repo LIST comes from kb-code's own scorecard, picked per
/// read-time by a human, never pinned in config"), so guessing one here
/// would be exactly the fabricated resolution Decision 3 forbids.
/// `linkifyCitations.ts` is the only caller that omits it.
export function codeSearchUrl(
  codeUrl: string,
  repo: string | undefined,
  query: string,
): string {
  const base = codeUrl.replace(/\/+$/, "");
  const params: Record<string, string> = { q: query };
  if (repo) params.repo = repo;
  const q = new URLSearchParams(params);
  return `${base}/search?${q}`;
}
