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
