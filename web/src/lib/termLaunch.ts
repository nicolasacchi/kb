// FUWEBLAUNCH — the pure rules of launching a kb-term session from the web.
//
// The harness set is the closed set the runner's profile table knows
// (docs/kb-term.md); `cwd` is validated exactly as `POST /api/terms` does
// (`routes/terms.rs` `free_text` + absolute): non-empty, at most 512 bytes,
// no control character, absolute. The SPA never builds an argv and never
// browses the host's directories: suggestions come only from data it already
// holds.

import type { LaunchBody } from "../api/terms";

export type LaunchHarness = { id: string; label: string; experimental: boolean };

export const LAUNCH_HARNESSES: readonly LaunchHarness[] = [
  { id: "claude", label: "Claude Code", experimental: false },
  { id: "codex", label: "Codex", experimental: false },
  { id: "omp", label: "omp", experimental: false },
  { id: "opencode", label: "OpenCode", experimental: true },
  { id: "grok", label: "Grok", experimental: true },
];

export const DEFAULT_LAUNCH_HARNESS = "claude";
export const CWD_MAX_BYTES = 512;

export function isLaunchHarness(h: string | null | undefined): h is string {
  return !!h && LAUNCH_HARNESSES.some((x) => x.id === h);
}

/// Resolve the stored last choice to a launchable harness (default `claude`).
export function pickLaunchHarness(stored: string | null | undefined): string {
  return isLaunchHarness(stored) ? stored : DEFAULT_LAUNCH_HARNESS;
}

/// `null` when `cwd` is acceptable, else the reason to show. Mirrors the
/// server's check so a request the server would refuse is never sent.
export function validateCwd(cwd: string): string | null {
  if (cwd === "") return "enter a working directory";
  if (new TextEncoder().encode(cwd).length > CWD_MAX_BYTES) {
    return `the path is longer than ${CWD_MAX_BYTES} bytes`;
  }
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f]/.test(cwd)) return "the path has a control character";
  if (!cwd.startsWith("/")) return "the path must be absolute (start with /)";
  return null;
}

/// The exact `POST /api/terms` body. A New terminal carries no `resume_sid`;
/// a Resume carries exactly the session id (never a bare continue).
export function launchBody(harness: string, cwd: string, resumeSid?: string): LaunchBody {
  const body: LaunchBody = { harness, cwd };
  if (resumeSid !== undefined) body.resume_sid = resumeSid;
  return body;
}

/// Suggestion list: recent launch cwds first, then the cwds of sessions the
/// page already holds; valid, unique, capped.
export function cwdSuggestions(
  recent: readonly string[],
  sessionCwds: readonly (string | null | undefined)[],
  max = 12,
): string[] {
  const out: string[] = [];
  for (const c of [...recent, ...sessionCwds]) {
    if (!c || validateCwd(c) !== null || out.includes(c)) continue;
    out.push(c);
    if (out.length >= max) break;
  }
  return out;
}

/// A terms row is bound once it carries a session id.
export const BOUND_POLL_MS = 2_000;
export const BOUND_DEADLINE_MS = 120_000;
