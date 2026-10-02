// RS-U11 (+ RS-U9's `state_json` additions) — pure parsing of the review
// store's `state_json` blob for the Home dashboard's "Review store" card
// (`components/home/ReviewStoreSection.tsx`).
//
// `GET /api/repos/{name}/store` (`review_store/routes.rs::store_card`)
// sends `state_json` VERBATIM as an opaque JSON value — no route turns
// `last_maint`/`last_gc_dry_run`/`last_gc_apply` (`review_store/maint.rs`'s
// own doc: "review_stores.state_json.last_maint records the last run of
// each cadence", "state_json.last_gc_dry_run") into a typed field or a
// doctor finding, so this module is where that summary gets read. Every
// parser is DEFENSIVE by construction: a missing key, a malformed shape,
// or `state_json` being `null` all degrade to `null` (nothing to show),
// never a crash and never a guess at a value that isn't really there —
// same posture `BaseStatus::parse` (`review_base.rs`) takes server-side
// for its own JSON blob.

export interface ReviewStoreLastMaint {
  daily: number | null;
  weekly: number | null;
  monthly: number | null;
}

/// `state_json.last_gc_dry_run` — written by EITHER the scheduled
/// report-only pass (`maint::run_maintenance`, weekly/monthly) or an
/// explicit `store gc` with no `--yes` (`maint::run_gc_now`). Plural
/// `reasons` on the wire (an array), even though today's server only
/// ever writes one element into it (`[report.gc.reason]`) — this reads
/// the whole array rather than assuming a single entry, so a future
/// multi-reason write is not silently truncated to the first one.
export interface ReviewStoreLastGcDryRun {
  at: number;
  candidates: number;
  reasons: string[];
  partial: boolean;
  member_problems: string[];
}

/// `state_json.last_gc_apply` — written ONLY by `store gc --yes`
/// (`maint::run_gc_now`'s `apply_requested` branch). Singular `reason`
/// on the wire, unlike the dry-run shape above — the two are genuinely
/// different objects, not the same shape reused.
export interface ReviewStoreLastGcApply {
  at: number;
  candidates: number;
  reason: string;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function numOrNull(v: unknown): number | null {
  return typeof v === "number" ? v : null;
}

function stringArray(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/// `null` when NONE of the three cadences have ever run (a fresh store,
/// or `state_json` predating RS-U9) — distinct from "ran, but a while
/// ago," which is a real `{daily: <ts>, ...}` with some fields still
/// `null`.
export function parseLastMaint(stateJson: unknown): ReviewStoreLastMaint | null {
  if (!isRecord(stateJson)) return null;
  const m = stateJson.last_maint;
  if (!isRecord(m)) return null;
  const daily = numOrNull(m.daily);
  const weekly = numOrNull(m.weekly);
  const monthly = numOrNull(m.monthly);
  if (daily == null && weekly == null && monthly == null) return null;
  return { daily, weekly, monthly };
}

export function parseLastGcDryRun(stateJson: unknown): ReviewStoreLastGcDryRun | null {
  if (!isRecord(stateJson)) return null;
  const g = stateJson.last_gc_dry_run;
  if (!isRecord(g) || typeof g.at !== "number") return null;
  return {
    at: g.at,
    candidates: typeof g.candidates === "number" ? g.candidates : 0,
    reasons: stringArray(g.reasons),
    partial: g.partial === true,
    member_problems: stringArray(g.member_problems),
  };
}

export function parseLastGcApply(stateJson: unknown): ReviewStoreLastGcApply | null {
  if (!isRecord(stateJson)) return null;
  const g = stateJson.last_gc_apply;
  if (!isRecord(g) || typeof g.at !== "number") return null;
  return {
    at: g.at,
    candidates: typeof g.candidates === "number" ? g.candidates : 0,
    reason: typeof g.reason === "string" ? g.reason : "",
  };
}

// ── failure-class + GC-reason hints (A9.f4 / N5) ─────────────────────────────
// ONE slug-to-hint table for `FailureClass` (`review_store/classify.rs`,
// the stable `urn:kb:errors:<slug>` names, also persisted as the store
// `state_code`) so the Home store card says what to DO instead of printing a
// bare slug. The same table, in prose, is docs/kb-code.md's failure-class
// section — keep them in step.
export const FAILURE_CLASS_HINTS: Record<string, string> = {
  vanished: "the requested ref is gone from the remote",
  offline: "the network was unreachable — retry later",
  timeout: "the call hit its deadline — retry later",
  "credential-rejected": "the remote refused the credential kb-code sent",
  "credential-wrong-repo": "the deploy key belongs to a different repository",
  "repo-not-found": "the remote says the repository does not exist (no credential was sent)",
  "auth-no-access": "the token cannot see this repository",
  "host-key-unknown": "ssh has no pinned host key for the host",
  "host-key-mismatch": "the ssh host key CHANGED — never auto-repaired; verify it by hand",
  "auth-required": "the remote wants credentials and none were supplied",
  tls: "TLS / certificate failure talking to the remote",
  "disk-full": "no space left while writing objects or refs",
  shallow: "a shallow-repository constraint refused the operation",
  "protocol-refused": "git refused the transport (GIT_ALLOW_PROTOCOL)",
  "url-rejected": "the URL failed the store's allowlist",
  "credential-account-mismatch": "the gh account answering is not the pinned one",
  "credential-unavailable": "a credential source exists but cannot be read now (locked keyring, gh timeout)",
  "no-credentials": "no credential rung applies",
  "spawn-failed": "the git subprocess could not be started (binary missing?)",
  "dubious-ownership": "git's safe.directory check refused the store dir — it is owned by another uid than the daemon",
  failed: "an unclassified git failure — see the daemon log",
};

/// The hint for a persisted failure slug, or the slug itself when it names
/// no known class (never invented prose for an unknown code).
export function failureClassHint(slug: string | null | undefined): string | null {
  if (!slug) return null;
  return FAILURE_CLASS_HINTS[slug] ?? slug;
}

/// A recorded GC outcome the operator must NOT read as a plain dry run:
/// `maint.rs::apply_gc_candidates` writes a REFUSED `store gc --yes` into the
/// `last_gc_dry_run` slot with the refusal as `reasons[0]`.
export const GC_REFUSAL_HINTS: Record<string, string> = {
  "restore-guard": "refused: the restore guard is armed — a restored store may not expire objects",
  "restore-suspected": "refused: the database looks restored from a backup (review high-water mismatch)",
  "backup-failed": "refused: the pre-GC backup bundle could not be written",
};

/// `null` for an ordinary dry run / nothing-to-do; the refusal text otherwise.
export function gcRefusal(reasons: readonly string[]): string | null {
  for (const r of reasons) {
    const hint = GC_REFUSAL_HINTS[r];
    if (hint) return hint;
  }
  return null;
}
