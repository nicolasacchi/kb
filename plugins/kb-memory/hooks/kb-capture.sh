#!/usr/bin/env bash
# Stop hook — capture the conversation transcript into the [kb.sessions]
# corpus when the agent finishes responding.
#
# Deterministic and LLM-free: NO extraction, NO summarisation. The wrap+write
# is `kb sessions capture` (the Rust engine,
# `kb-cli/src/commands/sessions_capture.rs`): the byte-identical `<pre>`
# envelope (`recover_jsonl_from_capture`'s round-trip invariant), the secrets
# scrub, plus an additive commit-resolution tail block. The daemon's watcher
# indexes the written file like any other artifact. (`Stop` — not
# `SessionEnd` — is the "agent finished" event; it carries `transcript_path`.)
#
# v0.44 X6 — there is NO bash envelope writer any more. The old fallback
# hand-wrapped the RAW transcript in HTML and so put unscrubbed bytes in the
# corpus whenever `kb` was missing or failed. Now a failed (or `kb`-less)
# capture parks the raw transcript in a private spool OUTSIDE every corpus
# (`kb-hook-lib.sh` hook_spool_put: dir 0700, files 0600, never indexed); the
# next successful capture — or `kb sessions capture --replay-spool` — pushes
# it through the scrubbed path and deletes it.
#
# Deadlines: every `kb` call is bounded by the shared hook deadline
# (kb-hook-lib.sh). `KB_CAPTURE_BUDGET_SECS` (default 25) sits below the 30s
# hooks.json timeout, which is the last resort, not the design.
#
# Set KB_SESSIONS_DIR to the [kb.sessions] source path. Unset → no-op.
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0

. "$(dirname "$0")/kb-hook-lib.sh" 2>/dev/null || {
  # Standalone copy without the shared lib: no spool, unbounded kb calls.
  run_to() { shift; "$@"; }
  hook_spool_put() { return 1; }
  hook_spool_pending() { return 1; }
  hook_deadline_init() { :; }
}
KB_HOOK_BUDGET_SECS="${KB_CAPTURE_BUDGET_SECS:-25}"
hook_deadline_init

input="$(cat)"
tpath="$(printf '%s' "$input" | jq -r '.transcript_path // empty' 2>/dev/null)"
[ -n "$tpath" ] && [ -f "$tpath" ] || exit 0

# Raw (un-sanitised) hint values for `kb sessions capture` — it recovers the
# canonical session id from the transcript's own `sessionId` itself
# (invariant #11) and only falls back to `--session-id` when that's absent,
# so this need not be sanitised here.
raw_sid="$(printf '%s' "$input" | jq -r '.session_id // "session"' 2>/dev/null)"
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"

if command -v kb >/dev/null 2>&1; then
  if run_to 20 kb sessions capture \
       --transcript "$tpath" \
       --session-id "$raw_sid" \
       ${cwd:+--cwd "$cwd"} \
       --out "$KB_SESSIONS_DIR" \
       >/dev/null 2>&1; then
    # A prior failure may have left spooled transcripts: land them now.
    if hook_spool_pending; then
      run_to 10 kb sessions capture --replay-spool --out "$KB_SESSIONS_DIR" >/dev/null 2>&1 || true
    fi
    exit 0
  fi
fi

# Capture failed or `kb` is absent: spool the raw transcript privately. A Stop
# hook must never fail the session — a refused/failed spool is a stderr line
# and exit 0, never a raw write into the corpus.
if ! hook_spool_put "$tpath" "$raw_sid" "$cwd"; then
  echo "kb-capture.sh: capture failed and the transcript could not be spooled: $tpath" >&2
fi
exit 0
