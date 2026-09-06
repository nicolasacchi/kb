// kb-slate/1 wire types — HAND-MIRRORED from the design's §9 "Wire shapes"
// and from the landed engine (`crates/kb-core/src/slate.rs`), field for
// field including every serde attribute (`rename_all = "snake_case"` on the
// enums; `skip_serializing_if = "Option::is_none"` ⇒ the field is OPTIONAL
// on the wire, not `T | null`; a bare `Option<T>` with no skip ⇒ `T | null`,
// always present).
//
// REPLACE THIS FILE with `web/src/api/generated/*` once SL2's ts-rs export
// lands — the generated bindings are definitionally in sync with the Rust
// structs; this mirror is not. Until then `api/slates.ts` imports from here
// and nothing else in the SPA reaches for these names.
//
// The route-level ENVELOPES (AppendResponse, SlateSummary, DigestResponse,
// DeltaResponse, BoardResponse, BoardCard) have no kb-core struct at all —
// they exist only in §9 and in SL2's `routes/slates.rs`. They are marked as
// such below so the reconciliation pass knows which half came from where.

// ── enums (kb-core, `rename_all = "snake_case"`) ─────────────────────────

/// The twelve kinds. Four are surface ops that never render as a top-level
/// entry (`drop`, `mark`, `done`, `answer` — `Kind::is_surface_op`).
export type SlateKind =
  | "now"
  | "warn"
  | "take"
  | "done"
  | "hand"
  | "ask"
  | "answer"
  | "found"
  | "idea"
  | "tried"
  | "drop"
  | "mark";

/// Client-declared, never verified (one trust tier). `unattributed` is the
/// ONE value a client cannot send — the daemon stamps it in place of
/// `agent` when no session id resolves. Every SPA post is `human`.
export type SlateOrigin = "agent" | "human" | "import" | "unattributed";

/// Take-lease liveness. `live | stale` are BOTH "live" for every friction
/// rule in the design (`Liveness::is_live`); `expired` no longer blocks.
export type SlateLiveness = "live" | "stale" | "expired";

/// Whether the liveness label came from a beat/transcript (`known`) or from
/// posts alone (`presumed`).
export type SlateTakeConfidence = "known" | "presumed";

/// Two card sizes, earned by kind+status/pin/marks — never self-rated.
export type SlateTier = "whole" | "folded";

export type SlateHideReason = "superseded" | "dropped" | "done";

/// `?mode=` on the digest read. Not a serde enum in kb-core (`Mode` derives
/// no Serialize) — it is a query-param string.
export type SlateMode = "full" | "hybrid";

// ── records (kb-core) ────────────────────────────────────────────────────

/// `slate::Prov` — the beat tuple every harness hook already carries.
/// `origin` has `#[serde(default)]` and NO skip, so it is always present.
export type SlateProv = {
  harness: string;
  session_id?: string;
  model?: string;
  cwd?: string;
  origin: SlateOrigin;
  /// Minted server-side from the resolved identity; never sent by a client.
  user?: string;
  job_id?: string;
};

/// `slate::PostBody` — the POST body. `seq`, `id`, `at` and `prov.user` are
/// minted server-side.
export type SlatePostBody = {
  kind: SlateKind;
  line: string;
  body?: string;
  topic?: string;
  subject?: string;
  /// ≤ 8 (`too-many-refs`).
  refs?: string[];
  re?: number;
  supersedes?: number;
  /// Non-null only on kind `mark`; a pin/unpin toggle.
  pin?: boolean;
  /// The one friction escape (drop/edit of a live other session's
  /// coordination post). Never an escape for `supersedes` on a live take.
  anyway?: boolean;
  over?: number;
  abandoned?: string;
  failed?: string;
  to?: string;
  prov: SlateProv;
};

/// `slate::Post` — the stored ledger record: `PostBody`'s fields plus the
/// server-minted `seq`/`id`/`at`.
export type SlatePost = {
  seq: number;
  id: string;
  /// Unix seconds.
  at: number;
  kind: SlateKind;
  line: string;
  body?: string;
  topic?: string;
  subject?: string;
  refs?: string[];
  re?: number;
  supersedes?: number;
  pin?: boolean;
  anyway?: boolean;
  over?: number;
  abandoned?: string;
  failed?: string;
  to?: string;
  prov: SlateProv;
};

/// `slate::RefDisplay` — a ref as the digest shows it.
export type SlateRefDisplay = {
  raw: string;
  display: string;
  resolved: boolean;
};

/// `slate::Who` — who wrote a post, as the wire and the board chip read it.
/// `tag` is the ONE place the author string is composed, so CLI, wire and
/// board never disagree.
export type SlateWho = {
  origin: SlateOrigin;
  harness: string;
  session_short: string;
  user?: string;
  job_id?: string;
  tag: string;
};

// ── projection (kb-core) ─────────────────────────────────────────────────

/// `slate::Projected` — one post as the digest shows it: the record plus
/// everything derived at read time. NONE of the derived fields is ever
/// written.
export type SlateProjected = {
  seq: number;
  id: string;
  kind: SlateKind;
  line: string;
  topic?: string;
  subject?: string;
  /// Absent when empty (`skip_serializing_if = "Vec::is_empty"`).
  refs?: SlateRefDisplay[];
  re?: number;
  supersedes?: number;
  who: SlateWho;
  age_secs: number;
  tier: SlateTier;
  /// Marks by OTHER sessions; dropped marks excluded.
  marks: number;
  pinned: boolean;
  contested: boolean;
  liveness?: SlateLiveness;
  confidence?: SlateTakeConfidence;
  /// Hands only: a `take #n` on the hand acknowledges it.
  acknowledged?: boolean;
  /// Asks only.
  answers?: number;
  /// The superseded ancestor — rendered `(was #n)`.
  was?: number;
  /// Takes only: silence since the holder's last beat/post.
  silence_secs?: number;
  /// `take --over` on a stalled/presumed-ended take.
  taken_over_by?: string;
  /// The author's session is PresumedEnded (drop/edit is free).
  author_ended?: boolean;
  /// D27 (v0.42) — the `session_short`s whose reported cursor is at or past
  /// this post's seq, the AUTHOR excluded. Derived per read from
  /// `meta.json`'s `cursors`; never written on a read (a cursor is REPORTED
  /// by `POST …/cursor`, never minted by a GET). Absent when empty
  /// (`skip_serializing_if = "Vec::is_empty"`), so a pre-v0.42 daemon and a
  /// post nobody has been served are the same honest "no chip".
  ///
  /// It is ATTRIBUTION, not acknowledgement: the digest says "served", and
  /// so does the board's hover.
  seen_by?: string[];
};

/// `slate::TopicNow` — one NOW-band row. Both fields are bare `Option`s
/// with NO skip, so both are ALWAYS present and may be null (`topic: null`
/// is the general lane, rendered `NOW  —`).
export type SlateTopicNow = {
  topic: string | null;
  now: SlateProjected | null;
};

/// `slate::Header`.
export type SlateHeader = {
  seen_from?: number;
  topics: SlateTopicNow[];
  context?: string;
  who?: string;
  first_read?: boolean;
};

/// `slate::Sections` — the seven digest sections. FOUND and IDEA share one.
export type SlateSections = {
  now: SlateProjected[];
  warn: SlateProjected[];
  hand: SlateProjected[];
  ask: SlateProjected[];
  take: SlateProjected[];
  found_idea: SlateProjected[];
  tried: SlateProjected[];
};

/// `slate::DroppedCounts` — per-section dropped tallies for the headers.
export type SlateDroppedCounts = {
  now: number;
  warn: number;
  hand: number;
  ask: number;
  take: number;
  found_idea: number;
  tried: number;
};

/// `slate::HideEntry` — one `hide` on a delta.
export type SlateHideEntry = {
  hide: number;
  by: number;
  reason: SlateHideReason;
  who: string;
  why?: string;
};

/// `slate::Displaced` — one post the newest append pushed off the default
/// digest. Never a NOW, WARN, pinned post or unacknowledged HAND.
export type SlateDisplaced = {
  seq: number;
  kind: SlateKind;
  line: string;
  age_secs: number;
};

/// `slate::HistoryRow` — a dropped or superseded post with the post that hid
/// it. `at` is the HIDING post's timestamp (when the board changed).
export type SlateHistoryRow = {
  post: SlatePost;
  hidden_by: number;
  reason: SlateHideReason;
  who: string;
  why?: string;
  at: number;
};

// ── route envelopes (§9 only — no kb-core struct) ────────────────────────

/// `GET /api/slates` row. `counts` drives the board chip:
/// `hand_unack + ask_open + take_contested + take_stale`, summed CLIENT-side
/// (`lib/slateLanes.ts`'s `attentionCount`) — the daemon never scores.
export type SlateSummary = {
  slug: string;
  head_seq: number;
  generation: number;
  updated_unix: number;
  closed: boolean;
  topics: string[];
  counts: SlateCounts;
  /// D27 (v0.42) — how many distinct sessions have reported a cursor on this
  /// slate. A count of who has been SERVED, never of who read anything, and
  /// never a rank term (`orderSlates` does not consult it). Optional here on
  /// purpose: a pre-v0.42 daemon omits it and the list renders no chip
  /// rather than a confident `0`.
  sessions_served?: number;
};

export type SlateCounts = {
  now: number;
  warn: number;
  hand_unack: number;
  ask_open: number;
  take_live: number;
  take_stale: number;
  take_contested: number;
  found: number;
  idea: number;
  tried: number;
};

/// `GET /api/slates/{slug}` — `slate::SlateDigest` plus the route's
/// `generation`. `text` is byte-identical to the CLI render; `hidden` is
/// `#[serde(skip)]` in kb-core and is NOT on the wire.
export type SlateDigestResponse = {
  slug: string;
  head_seq: number;
  generation: number;
  text: string;
  header: SlateHeader;
  sections: SlateSections;
  now_total: number;
  warn_total: number;
  hand_total: number;
  ask_total: number;
  take_total: number;
  found_idea_total: number;
  tried_total: number;
  hand_truncated: boolean;
  ask_truncated: boolean;
  take_truncated: boolean;
  found_idea_truncated: boolean;
  tried_truncated: boolean;
  dropped: SlateDroppedCounts;
  budget_exceeded: boolean;
  /// Every NOW line repeated verbatim, author- and age-less.
  echo: string[];
};

/// `GET /api/slates/{slug}/delta` — `slate::DeltaBatch` plus `slug`.
export type SlateDeltaResponse = {
  slug: string;
  head_seq: number;
  posts: SlateProjected[];
  hides: SlateHideEntry[];
  text: string;
  truncated: boolean;
  /// D27/v0.42 — `true` when the request carried `?kinds=` and the server
  /// applied it (the push adapters' hybrid subset). Absent ⇒ the response is
  /// the unfiltered one every pre-v0.42 caller already got.
  filtered?: boolean;
};

/// `POST /api/slates/{slug}/posts` → 201 (or 200 on an idempotent repeat
/// mark). `displaced` carries the first five, section-then-oldest.
export type SlateAppendResponse = {
  post: SlatePost;
  displaced: SlateDisplaced[];
  displaced_total: number;
  nudge: string | null;
  head_seq: number;
};

/// `Projected` + the three fields only the board reads. Never
/// budget-truncated — the board scrolls.
export type SlateBoardCard = SlateProjected & {
  body: string | null;
  /// Author tags of the sessions that marked this post (hover reveal).
  marks_by: string[];
  has_sketch: boolean;
};

/// `GET /api/slates/{slug}?view=board[&topic=]`. The five COLUMNS are
/// derived from these seven sections client-side (`lib/slateLanes.ts`).
export type SlateBoardSections = {
  now: SlateBoardCard[];
  warn: SlateBoardCard[];
  hand: SlateBoardCard[];
  ask: SlateBoardCard[];
  take: SlateBoardCard[];
  found_idea: SlateBoardCard[];
  tried: SlateBoardCard[];
};

export type SlateBoardResponse = {
  slug: string;
  head_seq: number;
  topics: string[];
  sections: SlateBoardSections;
};

// ── errors ───────────────────────────────────────────────────────────────

/// The slate's problem+json extension members. `code` is an RFC 7807
/// extension set by the slate's own response builder; the `detail` prefix
/// carries the same string, so a reader prefers `code` and falls back to
/// the prefix.
export type SlateProblemCode =
  | "slate-taken"
  | "slate-live-author"
  | "slate-closed"
  | "slate-full"
  | "slate-rate"
  | "pin-is-human"
  | "no-ascii-art"
  | "already-done"
  | "kind-mismatch"
  | "bad-ref";

/// `slate::HolderInfo` — who holds the take a 409 refused.
export type SlateHolder = {
  seq: number;
  line: string;
  harness: string;
  session_short: string;
  age_secs: number;
  liveness: SlateLiveness;
};
