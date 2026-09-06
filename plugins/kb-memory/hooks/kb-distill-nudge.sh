#!/usr/bin/env bash
# Stop hook — nudge, not capture: when the finished session ran git commits
# but wrote no kb memory, surface a one-line suggestion to run /kb-distill
# (the episodic→semantic bridge). 9 of the 12 July-2026 kb sessions matched
# exactly that pattern — real work, zero curated facts.
#
# Deterministic and LLM-free: two greps on the local transcript JSONL (plus
# any Task-tool subagent sidecars, MI-W0.3) — no daemon round-trip, so it
# fires on the very first Stop and with the daemon down. Matches the Bash
# tool's serialized input (`"command":"…git commit`), not prose mentions. A
# marker file caps the nudge at once per session. Every failure path exits
# 0 — a nudge must never block.
#
# MI-W0.3 — suppression is now SUCCESS-aware: a session only suppresses the
# nudge when the transcript shows `kb remember` actually SUCCEEDED (its own
# stdout line, `remembered <12-hex-id>  (<path>)`), not merely that the
# command was invoked. The old command-presence check silently swallowed a
# FAILED remember (daemon down, 400, …) — the fact was lost and the nudge
# never fired again for that session. Deliberate bias: a `kb remember
# --json` success prints no such line, so this can now emit a spurious
# nudge on a session that DID successfully remember via --json —
# acceptable; losing a fact silently was the worse failure mode.
#
# MI-W0.3 — both greps (the git-commit trigger and the success-marker
# suppression) also scan Task-tool subagent sidecar transcripts, which live
# at "$(dirname "$tpath")/<session-id>/subagents/"*.jsonl — a commit (or a
# `kb remember`) made entirely inside a subagent was previously invisible
# to this script (parent-transcript-only grep). No sidecar dir → the file
# list is just the parent transcript, byte-identical to the prior behavior.
#
# Gated like kb-capture.sh on KB_SESSIONS_DIR (no sessions corpus → nothing
# to distill from) and on `kb` being installed (the skill needs it).
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v kb >/dev/null 2>&1 || exit 0

input="$(cat)"
tpath="$(printf '%s' "$input" | jq -r '.transcript_path // empty' 2>/dev/null)"
sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
[ -n "$tpath" ] && [ -f "$tpath" ] && [ -n "$sid" ] || exit 0

marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
marker="$marker_dir/distill-nudged-$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
[ -f "$marker" ] && exit 0

# Parent transcript + any subagent sidecars (find-guarded — a literal
# unexpanded glob must never reach grep as a bogus filename when there are
# no sidecars).
files=("$tpath")
sidecar_dir="$(dirname "$tpath")/$sid/subagents"
if [ -d "$sidecar_dir" ]; then
  while IFS= read -r -d '' f; do
    files+=("$f")
  done < <(find "$sidecar_dir" -maxdepth 1 -type f -name '*.jsonl' -print0 2>/dev/null)
fi

# The `([^"\\]|\\.)*` prefix walks escaped quotes/backslashes inside the JSON
# string, so `echo "x" && git commit …` still matches.
grep -qE '"command":"([^"\\]|\\.)*git commit' "${files[@]}" 2>/dev/null || exit 0
grep -qE 'remembered [0-9a-f]{12}' "${files[@]}" 2>/dev/null && exit 0

mkdir -p "$marker_dir" 2>/dev/null && : >"$marker" 2>/dev/null
jq -n --arg sid "$sid" '{systemMessage:
  ("kb: this session has commits but no curated memory — run /kb-distill "
   + $sid + " to keep what was decided/shipped (--dry-run to preview).")}' \
  2>/dev/null || exit 0
