// Phase G4 — the "fetched PR head" ref grammar (`refs/kbc/pr/<n>`, minted by
// `POST /api/prs/fetch` — see `github::fetch_pr_ref`'s Rust doc for why this
// lives OUTSIDE `refs/heads/*`). The Compare page's PR-comments side strip
// (`routes/Compare.tsx`) needs to recognize when its own `to` param names
// one of these refs so it knows which PR's comments to fetch — kept as its
// own tiny pure parser/builder pair rather than an inline regex so the
// grammar has exactly ONE definition, mirroring this crate's general
// "one builder, one parser" discipline (`lib/codeUrl.ts`'s own header
// comment).

const PR_REF_PATTERN = /^refs\/kbc\/pr\/(\d+)$/;

/// Extract the PR number from a `refs/kbc/pr/<n>` ref string, or `null` for
/// anything else (an ordinary branch/tag/sha `to` value — the overwhelming
/// common case, so this is checked on every Compare render).
export function prNumberFromRef(ref: string): number | null {
  const m = ref.match(PR_REF_PATTERN);
  if (!m) return null;
  const n = Number(m[1]);
  return Number.isFinite(n) ? n : null;
}

/// Build the `refs/kbc/pr/<n>` ref string a fetched PR's head lives at —
/// the inverse of `prNumberFromRef`, used by `Prs.tsx`'s "Fetch & review"
/// action to build the Compare `to` param once `POST /api/prs/fetch`
/// succeeds.
export function prRef(number: number): string {
  return `refs/kbc/pr/${number}`;
}
