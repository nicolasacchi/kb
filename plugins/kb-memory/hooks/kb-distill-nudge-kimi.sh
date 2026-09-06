#!/usr/bin/env bash
# kb-distill-nudge-kimi.sh — Stop hook for Kimi Code: the same nudge as
# kb-distill-nudge.sh / kb-distill-nudge-codex.sh (git commit without a
# successful kb remember -> one nudge naming /kb-distill), reading the
# KIMI wire.jsonl encoding instead — see kb-capture-kimi.sh's header for
# the verified wire → Claude-shape mapping this mirrors (MI-W0.3, kimi
# nudge parity).
#
# Deterministic and LLM-free: two greps over the wire file, no daemon
# round-trip. Kimi's hook payload has NO transcript_path, so the wire
# path is derived from .session_id + .cwd exactly like kb-capture-kimi.sh
# does. A wire tool.call line carries the shell command as plain text
# inside .event.args (JSON-escaped but the substring survives), so —
# like the codex nudge — this greps tool.call lines by `"name":"Bash"`
# first, then THOSE lines for the literal `git commit` (a shell command's
# own text isn't further escaped), which skips lookalikes in prompts and
# tool outputs. The success marker (`remembered <12-hex-id>`, kb
# remember's non-JSON stdout line, captured into a tool.result output) is
# the exact same regex as every other harness's nudge.
#
# On a hit this does TWO things:
#   1. prints a one-line PLAIN-TEXT nudge to stdout — Kimi appends Stop
#      hook stdout to the model's context (no hookSpecificOutput envelope
#      on this harness), which is exactly where the nudge belongs;
#   2. appends `kimi <sid> <epoch>` to the shared distill-pending ledger
#      kb-wake.sh surfaces at the next interactive SessionStart (same
#      relay as kb-capture-grok.sh's queue_distill_pending, with the same
#      dedup-by-session-id), so the nudge survives even if the Stop-time
#      stdout is never read.
#
# A marker file (its own "-kimi-" namespace, distinct from the Claude and
# codex nudges' markers) caps this at once per session. Every failure
# path exits 0 — a nudge must never block.
#
# Gated like kb-capture-kimi.sh on KB_SESSIONS_DIR (no sessions corpus ->
# nothing to distill from) and on jq (payload parsing).
set -u
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

input="$(cat)"
sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"
[ -n "$sid" ] && [ -n "$cwd" ] || exit 0

home="${KIMI_CODE_HOME:-$HOME/.kimi-code}"
wdkey="wd_$(basename -- "$cwd")_$(printf '%s' "$cwd" | sha256sum | cut -c1-12)"
tpath="$home/sessions/$wdkey/$sid/agents/main/wire.jsonl"
[ -f "$tpath" ] || exit 0

marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
marker="$marker_dir/distill-nudged-kimi-$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
[ -f "$marker" ] && exit 0

grep '"tool.call"' "$tpath" 2>/dev/null \
  | grep '"name":"Bash"' \
  | grep -q 'git commit' || exit 0
grep -qE 'remembered [0-9a-f]{12}' "$tpath" 2>/dev/null && exit 0

mkdir -p "$marker_dir" 2>/dev/null || exit 0
: >"$marker" 2>/dev/null

# Shared ledger relay (dedup by session id — mirrors queue_distill_pending).
ledger="$marker_dir/distill-pending"
if [ -f "$ledger" ] && grep -qF "kimi $sid " "$ledger" 2>/dev/null; then
  :
else
  printf 'kimi %s %s\n' "$sid" "$(date +%s)" >>"$ledger" 2>/dev/null
fi

printf 'kb: this session has commits but no curated memory — run /kb-distill %s or ask the agent to distill.\n' "$sid"
exit 0
