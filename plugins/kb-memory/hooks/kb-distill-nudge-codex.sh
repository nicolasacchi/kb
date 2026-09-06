#!/usr/bin/env bash
# kb-distill-nudge-codex.sh — Stop hook for OpenAI Codex CLI: the same
# nudge as kb-distill-nudge.sh (git commit without a successful kb
# remember -> one systemMessage naming /kb-distill), reading the CODEX
# rollout JSONL encoding instead of Claude Code's transcript shape — see
# kb-capture-codex.sh's header for the verified rollout -> Claude-shape
# mapping this reuses (MI-W0.3, codex nudge parity).
#
# Deterministic and LLM-free: two greps over the rollout file named by
# .transcript_path, no daemon round-trip. A codex tool-call line
# (`function_call`, name exec_command/shell/local_shell) encodes its shell
# command as an escaped-JSON string nested inside "arguments" — rather
# than replicate that nesting in a regex, this greps for the tool-call
# line first (on `"name"`) then greps THOSE lines for the literal
# substring `git commit` (a shell command's own text isn't itself further
# JSON-escaped, so it survives intact regardless of nesting depth or
# whether codex used a `cmd` string or a `command` array). The success
# marker (`remembered <12-hex-id>`, `kb remember`'s own non-JSON stdout
# line, captured into a `function_call_output`) is plain text the same
# way, so it's the exact same regex as the Claude nudge — SUCCESS-aware
# from the start here (no legacy presence-check to fix, unlike
# kb-distill-nudge.sh). A `--json` remember success prints no such line,
# so a spurious nudge is possible — acceptable; matching command presence
# instead would silently swallow a FAILED remember, which loses a fact.
#
# Session id comes from the rollout's own session_meta.payload.id (the
# "id rung 1" kb-capture-codex.sh's TRANSLATE program relies on), not the
# hook payload's own session_id field. A marker file (its own
# "-codex" namespace, distinct from the Claude nudge's markers) caps this
# at once per session. Every failure path exits 0 — a nudge must never
# block.
#
# Gated like kb-capture-codex.sh on KB_SESSIONS_DIR (no sessions corpus ->
# nothing to distill from) and on `kb` being installed (the skill needs it).
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v kb >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0

input="$(cat)"
tpath="$(printf '%s' "$input" | jq -r '.transcript_path // empty' 2>/dev/null)"
[ -n "$tpath" ] && [ -f "$tpath" ] || exit 0

meta="$(head -1 "$tpath" 2>/dev/null | jq -c 'select(.type == "session_meta") | .payload' 2>/dev/null)"
sid="$(jq -r '.id // empty' <<<"$meta" 2>/dev/null)"
[ -n "$sid" ] || exit 0

marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
marker="$marker_dir/distill-nudged-codex-$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
[ -f "$marker" ] && exit 0

grep -E '"name":"(exec_command|shell|local_shell)"' "$tpath" 2>/dev/null \
  | grep -qE 'git commit' || exit 0
grep -qE 'remembered [0-9a-f]{12}' "$tpath" 2>/dev/null && exit 0

mkdir -p "$marker_dir" 2>/dev/null && : >"$marker" 2>/dev/null
jq -n --arg sid "$sid" '{systemMessage:
  ("kb: this session has commits but no curated memory — run /kb-distill "
   + $sid + " to keep what was decided/shipped (--dry-run to preview).")}' \
  2>/dev/null || exit 0
