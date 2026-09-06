#!/usr/bin/env bash
# kb-wake-kimi.sh — UserPromptSubmit hook for Kimi Code: the kimi-side
# twin of kb-wake.sh (SessionStart). A live hook probe showed Kimi's
# SessionStart and Stop hook stdout NEVER reaches the model — only
# UserPromptSubmit stdout is injected (wrapped as a user-origin
# <hook_result> message) — so the memory protocol + recent-memories index
# + distill-pending surfacing ride the FIRST prompt of the session here
# instead of SessionStart. Plain stdout text, no hookSpecificOutput
# envelope (Kimi appends stdout verbatim).
#
# Once per session (marker ~/.cache/kb/waked-kimi-<sid>) — kb-recall.sh
# already runs on every UserPromptSubmit with KB_HOOK_FMT=kimi and
# handles per-turn recall, so this script's job is only the
# once-per-session wake payload. It does NOT write the current-session /
# current-session-repo-<slug> marker files: kb-recall.sh is registered on
# the same event and already writes both on every prompt (duplicating
# that here would just race the same tmp+mv targets).
#
# Emits, on the first prompt only:
#   (a) the memory-protocol block — shared verbatim with kb-wake.sh via
#       memory-protocol.txt next to these scripts (one source of truth);
#   (b) the most-recent-memories index (same `kb recall '' --limit 10`
#       rendering kb-wake.sh does);
#   (c) the distill-pending ledger surfacing + consume-on-read (same
#       logic as kb-wake.sh, harness-labeled — queued by
#       kb-distill-nudge-kimi.sh / kb-capture-grok.sh).
#
# Deterministic, LLM-free, fail-open: a down daemon just means no index;
# any failure exits 0 with nothing injected.
input="$(cat)"
sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
# No session id -> no way to gate once-per-session; stay silent rather
# than spam the payload on every prompt.
[ -n "$sid" ] || exit 0

marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
marker="$marker_dir/waked-kimi-$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
[ -f "$marker" ] && exit 0

# The caller's working directory, with the harness's own `$PWD` as the
# fallback — same pattern as kb-recall.sh/kb-wake.sh, needed for the
# project-aware recall call below.
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"
[ -n "$cwd" ] || cwd="$PWD"

# Loopback daemon must not ride HTTP(S)_PROXY (e.g. opencode's VPN alias).
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy

# (a) — shared with kb-wake.sh; $(cat) strips the file's trailing
# newline, matching kb-wake.sh's read byte-for-byte.
protocol_file="$(cd "$(dirname "$0")" && pwd)/memory-protocol.txt"
protocol=""
if [ -f "$protocol_file" ]; then
  protocol="$(cat "$protocol_file" 2>/dev/null)"
fi

# (b) — same rendering as kb-wake.sh. kb-cli defaults to
# http://127.0.0.1:4000 — KB_DAEMON_URL overrides.
extra=()
[ -n "${KB_DAEMON_URL:-}" ] && extra+=(--daemon "$KB_DAEMON_URL")

# Project-aware recall: `--cwd` lets the CLI derive its default "auto"
# scope (global corpora + the caller repo's own memory-<slug> corpus) from
# the payload cwd. `--scope all` is DROPPED — the CLI falls back to that
# same fleet-wide behaviour on its own outside a repo or against an old
# daemon.
recall_args=()
[ -n "$cwd" ] && recall_args+=(--cwd "$cwd")
index="$(kb recall '' "${extra[@]}" "${recall_args[@]}" --limit 10 --json 2>/dev/null \
  | jq -r '(.hits // [])
      | map("- \(.title)  [\(.kb)]"
          + (if (.summary // "") != "" then "\n    ↳ " + (.summary[0:160]) else "" end))
      | if length == 0 then empty else "Recent memories:\n" + join("\n") end' \
  2>/dev/null)" || index=""

# (c) — surface + consume the distill-pending ledger (drop entries older
# than 14 days, surface up to the 3 newest, rewrite the ledger to hold
# only what wasn't surfaced). Same logic as kb-wake.sh; the label names
# the harness(es) of the surfaced entries (ledger field 1). Any failure
# -> no block, ledger left untouched.
pending_block=""
ledger="$marker_dir/distill-pending"
if [ -f "$ledger" ]; then
  now="$(date +%s)" || now=""
  if [ -n "$now" ]; then
    cutoff=$((now - 14 * 86400))
    fresh="$(awk -v c="$cutoff" 'NF==3 && $3+0>=c' "$ledger" 2>/dev/null \
      | sort -k3,3nr 2>/dev/null)" || fresh=""
    if [ -n "$fresh" ]; then
      surfaced="$(printf '%s\n' "$fresh" | head -3)"
      remaining="$(printf '%s\n' "$fresh" | tail -n +4)"
      n="$(printf '%s\n' "$surfaced" | wc -l | tr -d ' ')"
      ids="$(printf '%s\n' "$surfaced" \
        | awk '{ if (NR>1) printf " · "; printf "%s", $2 }')"
      harnesses="$(printf '%s\n' "$surfaced" \
        | awk '{ print $1 }' | sort -u | paste -sd, -)"
      if [ -n "$ids" ]; then
        pending_block="Pending distill ($harnesses): $n session(s) with commits but no curated memory — kb: /kb-distill $ids"
        tmp="$ledger.tmp"
        if [ -n "$remaining" ]; then
          printf '%s\n' "$remaining" >"$tmp" 2>/dev/null && mv "$tmp" "$ledger" 2>/dev/null || rm -f "$tmp"
        else
          : >"$tmp" 2>/dev/null && mv "$tmp" "$ledger" 2>/dev/null || rm -f "$tmp"
        fi
      fi
    fi
  fi
fi

# SL3 — the slate HYBRID block (same call kb-wake.sh makes; see its comment
# for the design references). `sid` is guaranteed non-empty here (the
# once-per-session gate above already exited otherwise), so no ladder is
# needed. Slug derivation is entirely server-side (`--cwd`) — this hook
# never re-implements it. Any failure is silent and byte-identical to today.
slate_text=""
slate_json="$(timeout 4 kb slate open --hybrid --budget 2000 \
  --session-id "$sid" --cwd "$cwd" "${extra[@]}" --json 2>/dev/null)" || slate_json=""
if [ -n "$slate_json" ]; then
  slate_text="$(printf '%s' "$slate_json" | jq -r '.text // empty' 2>/dev/null)" || slate_text=""
  slate_head_seq="$(printf '%s' "$slate_json" | jq -r '.head_seq // empty' 2>/dev/null)" || slate_head_seq=""
  if [ -n "$slate_head_seq" ]; then
    mkdir -p "$marker_dir" 2>/dev/null \
      && printf '%s\n' "$slate_head_seq" >"$marker_dir/slate-cursor-$sid.tmp" 2>/dev/null \
      && mv "$marker_dir/slate-cursor-$sid.tmp" "$marker_dir/slate-cursor-$sid" 2>/dev/null \
      || true
  fi
fi

ctx="$protocol"
[ -n "${index:-}" ] && ctx="$ctx"$'\n\n'"$index"
[ -n "${pending_block:-}" ] && ctx="$ctx"$'\n\n'"$pending_block"
[ -n "${slate_text:-}" ] && ctx="$ctx"$'\n\n'"$slate_text"
[ -n "$ctx" ] || exit 0

# Mark only when we actually emitted — a missing protocol file / down
# daemon / empty ledger must not burn the once-per-session chance.
mkdir -p "$marker_dir" 2>/dev/null && : >"$marker" 2>/dev/null
printf '%s\n' "$ctx"
exit 0
