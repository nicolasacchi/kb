// A5 — kb's SPA fetch of kb-code's `kbc-session-card/1`
// (`crates/kb-code-server/src/session_card.rs`). Hand-written types
// mirroring that Rust wire struct-for-struct (kb-code's presence and
// doc-lens types are not ts-exported either — same precedent, see
// `api/doclens.ts`'s header). A committed golden
// (`tests/fixtures/session-card-v1.json`) pins both sides: the Rust unit
// test `card_matches_the_committed_golden` builds the card in Rust, and
// this module's own test parses the fixture into `SessionCardOut`.
//
// No kb -> kb-code call exists outside this module's doclens client: the
// fetch reuses `getCrossOrigin` (credentials `include`, 8 s timeout,
// compound failure message) — no breaker, no second cross-origin client.

import { getCrossOrigin, trimBase } from "./doclens";

export type CommitConfidence = "trailer" | "exact" | "fuzzy" | "none";

export interface CommitItem {
  repo: string;
  sha: string;
  subject?: string;
  visible: boolean;
  caption?: string;
  confidence: CommitConfidence;
  via: string;
  attributed_here: boolean;
}

export interface CommitsLane {
  available: boolean;
  caption?: string;
  total: number;
  shown: number;
  truncated: boolean;
  items: CommitItem[];
}

export interface ReviewItem {
  id: number;
  repo: string;
  title: string;
  state: string;
  verdict?: string;
  verdict_ps?: number;
  pr_number?: number;
  updated_at: number;
  basis: "session_id" | "doc_author";
}

export interface ReviewsLane {
  available: boolean;
  caption?: string;
  total: number;
  shown: number;
  items: ReviewItem[];
}

export interface SeverityCounts {
  blocker: number;
  concern: number;
  ok: number;
}

export interface FindingsLane {
  available: boolean;
  caption?: string;
  total: number;
  by_severity: SeverityCounts;
  basis: "author";
}

export interface PresenceLane {
  available: boolean;
  caption?: string;
  files: number;
  newest_age_secs: number;
}

export interface SessionCardOut {
  schema: "kbc-session-card/1";
  sid: string;
  now_unix: number;
  commits: CommitsLane;
  reviews: ReviewsLane;
  findings: FindingsLane;
  presence: PresenceLane;
}

export function fetchSessionCard(
  codeUrl: string,
  sid: string,
  repo?: string,
  signal?: AbortSignal,
): Promise<SessionCardOut> {
  const base = trimBase(codeUrl);
  const q = new URLSearchParams({ sid });
  if (repo) q.set("repo", repo);
  return getCrossOrigin<SessionCardOut>(`${base}/api/session-card?${q}`, signal);
}

/// The one grammar for a kb-code commit permalink
/// (`/r/<repo>/~commit/<sha>`, `web-code/src/app.tsx`). No builder exists
/// for it in `web/src/lib/codeDoors.ts`, so the pure function lives here
/// and is unit-tested.
export function commitHref(codeUrl: string, repo: string, sha: string): string {
  return `${trimBase(codeUrl)}/r/${encodeURIComponent(repo)}/~commit/${encodeURIComponent(sha)}`;
}
