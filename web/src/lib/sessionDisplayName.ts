// A2/A3 — the readable name for a session row, shared by the /sessions list,
// the inspector, and the SessionPicker so every surface agrees. The daemon
// already computes `display_name` server-side (SessionOut::from_row); this is
// the client mirror + fallback for any older payload that predates it.
import type { SessionRow } from "../api/sessions";

/// W3.C/R4 — a `substance:"trivial"` row with no real user prompt behind it
/// (a `/clear` husk) whose title/display_name is still the generic
/// server-computed fallback ("Session transcript 2026-…" or a bare short id)
/// promises content that doesn't exist. Gallery keeps husks visible but
/// dimmed (S7) — this override makes the LABEL honest too, everywhere
/// `sessionDisplayName` is called (list rows, inspector, SessionPicker).
/// Deterministic: gated on `substance` + the absence of `first_user_prompt`
/// alone, never on string-sniffing the title (no "looks generic" heuristic).
export const HUSK_DISPLAY_NAME = "(empty session)";

function isHuskWithNoRealPrompt(row: SessionRow): boolean {
  return (
    row.substance === "trivial" &&
    !(row.first_user_prompt && row.first_user_prompt.trim())
  );
}

export function sessionDisplayName(row: SessionRow): string {
  if (isHuskWithNoRealPrompt(row)) return HUSK_DISPLAY_NAME;
  if (row.display_name && row.display_name.trim()) return row.display_name;
  if (row.title && row.title.trim()) return row.title;
  if (row.first_user_prompt && row.first_user_prompt.trim())
    return row.first_user_prompt;
  return `session ${row.session_id.slice(0, 8)}`;
}
