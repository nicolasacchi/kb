#!/usr/bin/env bash
# kb-distill-nudge-omp — the Oh My Pi twin of kb-distill-nudge.sh /
# kb-distill-nudge-codex.sh / kb-distill-nudge-kimi.sh: when the session ran
# `git commit` but `kb remember` never SUCCEEDED, surface a one-line
# suggestion to distill the session into durable memory.
#
# Deterministic and LLM-free: two greps over the omp session JSONL named by
# .session_file, no daemon round-trip. An omp assistant toolCall block carries
# its shell command as plain text inside .arguments.command, so this selects
# bash toolCalls via jq and greps those commands for `git commit`; the
# `remembered <12-hex-id>` success marker is plain text either way, so it's
# the same regex every sibling uses.
#
# Output channel differs from Claude/codex: omp session_stop stdout reaches
# nobody, so the CALLER (plugins/kb-memory/hooks/kb-omp.ts) shows this
# script's stdout via ctx.ui.notify. Independently of that, a hit appends
# `omp <sid> <epoch>` to the shared $XDG_CACHE_HOME/kb/distill-pending ledger,
# so the next interactive session's wake (any harness) surfaces it too —
# same relay grok uses for its headless sessions.
# A hit also posts one slate ask, `Distill session <sid> (omp)?`, via
# `kb slate ask` with the session cwd when this hook has one (payload
# `.cwd`, else the session header). Best-effort: a missing `kb`, a daemon
# error, or a hang must not block the notify line or the ledger append.
# A suppressed run posts nothing.
#
# Modes:
#   hook mode : stdin carries {session_file, session_id, cwd}   (from kb-omp.ts)
#   CLI mode  : kb-distill-nudge-omp.sh <session.jsonl>         (backfill/tests;
#               sid read from the file's own session header)
set -u

# Call only after the commit-without-remember check. Slate stdout is
# discarded so it cannot corrupt the notify line. Never blocks.
post_distill_ask() {
  local sid="$1" harness="$2" cwd="${3:-}"
  command -v kb >/dev/null 2>&1 || return 0
  local args=(
    slate ask "Distill session ${sid} (${harness})?"
    --harness "$harness"
    --session-id "$sid"
    --ref "session:${sid}"
  )
  [ -n "$cwd" ] && args+=(--cwd "$cwd")
  # Loopback must not ride HTTP(S)_PROXY (same reason as kb-wake-kimi.sh).
  (
    export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
    export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
    unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy
    if command -v timeout >/dev/null 2>&1; then
      timeout 4 kb "${args[@]}" >/dev/null 2>&1 || true
    else
      kb "${args[@]}" >/dev/null 2>&1 || true
    fi
  )
}
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
ledger="$marker_dir/distill-pending"

check_one() {
  local tpath="$1" sid="$2" cwd="${3:-}"
  [ -f "$tpath" ] || return 0
  [ -n "$sid" ] || return 0

  # Once per session, like every sibling nudge.
  mkdir -p "$marker_dir" 2>/dev/null || return 0
  local marker="$marker_dir/distill-nudged-omp-$sid"
  [ -e "$marker" ] && return 0

  # 1) Did the session run git commit through a Bash tool call?
  #    (arguments.command is plain text inside the omp toolCall block)
  if ! jq -r 'select(.type == "message") | .message.content[]?
              | select(.type == "toolCall"
                       and ((.name // "") | ascii_downcase) == "bash")
              | .arguments.command // empty' "$tpath" 2>/dev/null \
      | grep -q 'git commit'; then
    return 0
  fi
  if grep -qE 'remembered [0-9a-f]{12}' "$tpath" 2>/dev/null; then
    return 0
  fi

  printf '%s\n' \
    "This omp session committed code but kept no curated memory — consider: kb remember \"<the durable fact/decision>\" (supersede with --supersedes <id> when it changes)."

  touch "$marker" 2>/dev/null

  # Shared cross-harness relay: next interactive wake surfaces + consumes.
  printf 'omp %s %s\n' "$sid" "$(date +%s)" >>"$ledger" 2>/dev/null || true
  # Payload cwd wins; CLI mode and a cwd-less payload fall back to the
  # session header this hook already reads for the sid.
  if [ -z "$cwd" ]; then
    cwd="$(head -c 262144 "$tpath" 2>/dev/null \
      | jq -r 'select(.type == "session") | .cwd // empty' 2>/dev/null \
      | head -1)"
  fi
  post_distill_ask "$sid" omp "$cwd"
  return 0
}

if [ "$#" -gt 0 ]; then
  for arg in "$@"; do
    hdr="$(head -c 262144 "$arg" 2>/dev/null \
      | jq -c 'select(.type == "session") | {id: (.id // ""), cwd: (.cwd // "")}' 2>/dev/null \
      | head -1)"
    sid="$(jq -r '.id // empty' <<<"$hdr" 2>/dev/null)"
    cwd="$(jq -r '.cwd // empty' <<<"$hdr" 2>/dev/null)"
    check_one "$arg" "$sid" "$cwd"
  done
else
  input="$(cat)"
  tpath="$(printf '%s' "$input" | jq -r '.session_file // .transcript_path // empty' 2>/dev/null)"
  sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
  cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"
  check_one "$tpath" "$sid" "$cwd"
fi
exit 0
