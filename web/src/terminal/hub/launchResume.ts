// HUBLAUNCH — the pure rules behind the LaunchDialog's Resume tab and its
// cwd suggestions. No React, no network.

import type { SessionRow } from "../../api/generated/SessionRow";
import { isLaunchHarness, validateCwd } from "../../lib/termLaunch";

/// Mirrors `is_plain_token` in the runner's `profiles/mod.rs`: length 1 to
/// 128, first char ASCII alphanumeric, the rest ASCII alphanumeric or one of
/// `.` `_` `:` `-`. A session id that fails this can never be a resume tail.
export function isPlainResumeToken(s: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(s);
}

/// The fields a launch needs from a session row (list row or detail).
export type ResumeSource = Pick<SessionRow, "session_id" | "harness" | "cwd">;

/// Why a session cannot be resumed from the web, or `null` when it can.
export function resumeRefusal(row: ResumeSource): string | null {
  if (!isLaunchHarness(row.harness)) return "this session's harness cannot be resumed here";
  if (!row.cwd) return "this session has no working directory";
  if (validateCwd(row.cwd) !== null) return "this session's working directory is not launchable";
  if (!isPlainResumeToken(row.session_id)) return "this session's id cannot be resumed";
  return null;
}

/// The rows the Resume tab may offer: a launchable harness (drops legacy
/// `kimi`), a valid cwd, a plain token id. Order is preserved.
export function resumableRows<T extends SessionRow>(rows: readonly T[]): T[] {
  return rows.filter((r) => resumeRefusal(r) === null);
}

export type CwdGroup = { group: string; cwd: string };

/// One suggestion per project: the cwd of the most recent session (largest
/// `started_at`) of each `project_key` (falling back to `folder`). Rows with
/// no usable cwd are ignored. Output is most-recent-first.
export function groupCwdsByProject(rows: readonly SessionRow[]): CwdGroup[] {
  const best = new Map<string, SessionRow>();
  for (const r of rows) {
    if (!r.cwd || validateCwd(r.cwd) !== null) continue;
    const key = r.project_key || r.folder || r.cwd;
    const cur = best.get(key);
    if (!cur || r.started_at > (cur.started_at ?? 0)) best.set(key, r);
  }
  return [...best.entries()]
    .sort((a, b) => b[1].started_at - a[1].started_at)
    .map(([group, r]) => ({ group, cwd: r.cwd as string }));
}
