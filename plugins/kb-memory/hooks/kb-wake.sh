#!/usr/bin/env bash
# SessionStart hook — re-inject the memory protocol + a compact index of
# recent memories at the start of every session.
#
# Deterministic and LLM-free. This is the load-bearing nudge: curated
# capture depends on the agent actually calling `kb remember`, so we
# remind it each session that the verb exists and when to use it. Never
# blocks; a down daemon just means no index.
input="$(cat)"

# Loopback daemon must not ride HTTP(S)_PROXY (e.g. opencode's VPN alias).
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy

# v0.14 S1 — stash the session_id where `kb remember` can find it. The
# marker lets every in-session `kb remember` stamp its memory with
# `<meta name="kb-session">` so the /sessions visualization can group
# memories by the conversation that produced them. Best-effort.
sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
if [ -n "$sid" ]; then
  marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
  mkdir -p "$marker_dir" 2>/dev/null \
    && printf '%s\n' "$sid" > "$marker_dir/current-session.tmp" 2>/dev/null \
    && mv "$marker_dir/current-session.tmp" "$marker_dir/current-session" 2>/dev/null \
    || true
fi

# The caller's working directory, with the harness's own `$PWD` as the
# fallback. Read ONCE up here because two consumers need it: the W0.3
# repo-keyed marker just below, and the project-aware recall call further
# down.
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"
[ -n "$cwd" ] || cwd="$PWD"

# W0.3 — also drop a REPO-KEYED marker (session id + timestamp), read by
# the git-dispatch prepare-commit-msg hook to stamp `Kb-Session:` commit
# trailers when CLAUDE_CODE_SESSION_ID isn't set in the commit's own env
# (e.g. a plain shell in the repo). Best-effort, never blocks. The slug
# algorithm MUST match git-dispatch/trailer-logic.sh exactly — kept in
# lockstep by hand rather than a shared lib, so this file still works
# copied standalone (see README.md "Mode 1: manual").
if [ -n "$sid" ]; then
  kb_slugify() {
    local s
    s="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9' '-')"
    s="${s#-}"
    s="${s%-}"
    printf '%s' "$s"
  }
  root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null)" || root=""
  repo_key="$(kb_slugify "${root:-$cwd}")"
  if [ -n "$repo_key" ]; then
    marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
    repo_marker="$marker_dir/current-session-repo-$repo_key"
    mkdir -p "$marker_dir" 2>/dev/null \
      && printf '%s\n%s\n' "$sid" "$(date +%s)" > "$repo_marker.tmp" 2>/dev/null \
      && mv "$repo_marker.tmp" "$repo_marker" 2>/dev/null \
      || true
  fi
fi

# The protocol text lives in memory-protocol.txt next to this script so
# kb-wake-kimi.sh (the Kimi Code UserPromptSubmit variant) shares one
# source of truth. $(cat) strips the file's trailing newline — exactly
# matching the old inline `read -r -d ''` heredoc, so this hook's output
# stays byte-identical. A missing sidecar (standalone copy) degrades
# fail-open to no protocol.
protocol_file="$(cd "$(dirname "$0")" && pwd)/memory-protocol.txt"
if [ -f "$protocol_file" ]; then
  protocol="$(cat "$protocol_file" 2>/dev/null)"
else
  protocol=""
fi

# kb-cli defaults to http://127.0.0.1:4000 — KB_DAEMON_URL lets a project
# (via .claude/settings.json `env`) point this hook at a non-default port.
extra=()
[ -n "${KB_DAEMON_URL:-}" ] && extra+=(--daemon "$KB_DAEMON_URL")

# Project-aware recall: `--cwd` lets the CLI derive its default "auto"
# scope (global corpora + the caller repo's own memory-<slug> corpus) from
# the payload cwd rather than this hook's own process cwd. `--scope all`
# is DROPPED — the CLI falls back to that same fleet-wide behaviour on its
# own outside a repo or against a daemon too old to know "auto".
# The index uses the same header + `<!--kb-recall/1-->` marker grammar
# kb-recall.sh emits, so a SessionStart capture opens ingest_attachment's
# recall gate. Selection is unchanged (`kb recall '' --limit 10`).
recall_args=()
[ -n "$cwd" ] && recall_args+=(--cwd "$cwd")
# Deadline: hooks.json gives this hook 15s and everything (protocol, pending
# block, slate digest) is emitted at the END, so one hung call must not eat
# the whole budget. recall 5s + slate 4s + skew probe 2s + chores 3s = 14s worst case, capped by the shared 13s budget; a recall timeout
# degrades to protocol-only output (index stays empty), never to no output.
# `timeout` is guarded: without coreutils the call runs unwrapped.
# Shared deadline helpers (kb-hook-lib.sh): every call is capped at
# min(its own cap, what is left of KB_HOOK_BUDGET_SECS, default 13) so the
# lanes together stay under the hooks.json timeout. Standalone copy without
# the lib: unbounded calls, as before H1.
. "$(dirname "$0")/kb-hook-lib.sh" 2>/dev/null || {
  hook_deadline_init() { :; }
  run_to() { shift; "$@"; }
  hook_export_identity() { :; }
}
hook_deadline_init
# v0.44 X4 — attribute every shell `kb` write of this session (also lands in
# $CLAUDE_ENV_FILE so the agent's own Bash tool calls inherit it).
hook_export_identity "$sid" "${KB_HARNESS:-$([ -n "${CLAUDE_PLUGIN_ROOT:-}" ] && echo claude)}"
index="$(run_to 5 kb recall '' "${extra[@]}" "${recall_args[@]}" --limit 10 --json 2>/dev/null \
  | jq -r '(.hits // [])
      | map("- \(.title)  [\(.kb)]"
          + (if (.summary // "") != "" then "\n    ↳ " + (.summary[0:160] | gsub("[\r\n]+"; " ")) else "" end)
          + "\n<!--kb-recall/1 kb=\(.kb) id=\(.id)-->")
      | if length == 0 then empty
        else "Relevant memories from kb (recall — these persist across sessions):\n" + join("\n")
        end' \
  2>/dev/null)" || index=""

# MI-W0.3 — surface + consume the distill-pending ledger (queued by
# kb-capture-grok.sh's queue_distill_pending, kb-distill-nudge-kimi.sh,
# and kb-distill-nudge.sh as `grok|kimi|claude <sid> <epoch>`). A
# Stop-time nudge lands in a session that is already over, so the relay
# lands here, at the next interactive SessionStart. Field 1 is the
# harness; claude lines are surfaced the same way as grok/kimi (no
# allowlist). The label names
# the harness(es) of the surfaced entries (ledger field 1). Drop entries older than 14 days, surface
# up to the 3 newest, then rewrite the ledger to hold only what wasn't
# surfaced (both the surfaced AND the stale entries are gone afterward —
# consume-on-read, same atomic tmp+mv style as the markers above). Any
# failure here -> no block, ledger left untouched, rest of the hook
# unaffected.
pending_block=""
ledger="${XDG_CACHE_HOME:-$HOME/.cache}/kb/distill-pending"
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

# SL3 — the slate HYBRID block. `kb slate` is per-project shared working
# state (who is on what, open questions, dead ends — NOT memory), read at
# session start per design §12's harness-reach table. The session id ladder
# mirrors kb-beat.sh's: the payload's own id first, then KB_SESSION_ID, then
# the current-session marker this same hook (or a prior hook this session)
# already wrote above — a resumed/foreign session still resolves one. Slug
# derivation is entirely server-side (`--cwd`) per §12's "shell slug drift"
# warning: this hook must never re-implement `kb_slugify` for the slate.
# Any failure — kb missing, non-zero exit, the 4s timeout, malformed JSON,
# no git repo — is silent and leaves output byte-identical to today.
slate_sid="$sid"
[ -n "$slate_sid" ] || slate_sid="${KB_SESSION_ID:-}"
if [ -z "$slate_sid" ]; then
  cs_marker="${XDG_CACHE_HOME:-$HOME/.cache}/kb/current-session"
  [ -f "$cs_marker" ] && slate_sid="$(cat "$cs_marker" 2>/dev/null)"
fi

slate_text=""
if [ -n "$slate_sid" ]; then
  slate_json="$(run_to 4 kb slate open --hybrid --budget 2000 \
    --session-id "$slate_sid" --cwd "$cwd" "${extra[@]}" --json 2>/dev/null)" || slate_json=""
  if [ -n "$slate_json" ]; then
    slate_text="$(printf '%s' "$slate_json" | jq -r '.text // empty' 2>/dev/null)" || slate_text=""
    slate_head_seq="$(printf '%s' "$slate_json" | jq -r '.head_seq // empty' 2>/dev/null)" || slate_head_seq=""
    if [ -n "$slate_head_seq" ]; then
      cursor_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
      mkdir -p "$cursor_dir" 2>/dev/null \
        && printf '%s\n' "$slate_head_seq" >"$cursor_dir/slate-cursor-$slate_sid.tmp" 2>/dev/null \
        && mv "$cursor_dir/slate-cursor-$slate_sid.tmp" "$cursor_dir/slate-cursor-$slate_sid" 2>/dev/null \
        || true
    fi
  fi
fi

# v0.44 F8 — name CLI/hook version skew. These hooks run straight from the
# repo while the `kb` binary is whatever was last installed, so a hook that
# leans on newer CLI behaviour silently does nothing against an old binary.
# KB_HOOK_CONTRACT is the integer contract these hooks were written against
# (kept in lock-step with `kb version --contract` by a unit test in
# crates/kb-cli/src/commands/version.rs; bump both when a hook starts needing
# new CLI behaviour). A binary that prints less, or fails the call because it
# predates the flag, gets ONE counts-only line per day. Silence it with
# KB_SKEW_NOTICE=0 (e.g. a CLI pinned on purpose on a second machine).
KB_HOOK_CONTRACT=1
skew_block=""
if [ "${KB_SKEW_NOTICE:-1}" != "0" ] && command -v kb >/dev/null 2>&1; then
  skew_stamp="${XDG_CACHE_HOME:-$HOME/.cache}/kb/skew-notice"
  skew_today="$(date +%Y-%m-%d)"
  if [ "$(cat "$skew_stamp" 2>/dev/null)" != "$skew_today" ]; then
    # rc 0 + empty/unparseable output = unknown (no nag); rc 124 = the probe
    # timed out (no nag); any other non-zero = the binary predates the verb.
    skew_have="$(run_to 2 kb version --contract 2>/dev/null)"
    skew_rc=$?
    if [ "$skew_rc" -eq 124 ]; then
      skew_have=""
    elif [ "$skew_rc" -ne 0 ]; then
      skew_have=0
    else
      case "$skew_have" in *[!0-9]*) skew_have="" ;; esac
    fi
    if [ -n "$skew_have" ] && [ "$skew_have" -lt "$KB_HOOK_CONTRACT" ]; then
      skew_block="kb CLI is older than these hooks (hook contract $KB_HOOK_CONTRACT, CLI reports $skew_have): hook features that need newer CLI code are inert. Fix: install a current kb (copy the binary out of the daemon image, or run scripts/install.sh). Shown once a day; KB_SKEW_NOTICE=0 silences it."
      mkdir -p "$(dirname "$skew_stamp")" 2>/dev/null \
        && printf '%s\n' "$skew_today" >"$skew_stamp" 2>/dev/null || true
    fi
  fi
fi

# v0.44 F10 — the chores line. `kb chores --line` prints ONE counts-only
# line when agent-layer upkeep is due (distill debt, slate tidy/rotate,
# triage, resurface, stale markers, weekly note, CLI skew), at most once per
# UTC day (the CLI keeps the stamp), and exits 0 printing nothing when
# nothing is due or the daemon is down. An old CLI without the verb fails;
# that is swallowed. The hook adds no logic of its own.
chores_block=""
if command -v kb >/dev/null 2>&1; then
  chores_block="$(run_to 3 kb chores --line "${extra[@]}" 2>/dev/null)" || chores_block=""
  # Counts only, one line: never let a misbehaving CLI inject more.
  chores_block="$(printf '%s\n' "$chores_block" | head -n 1)"
fi

ctx="$protocol"
[ -n "${index:-}" ] && ctx="$ctx"$'\n\n'"$index"
[ -n "${pending_block:-}" ] && ctx="$ctx"$'\n\n'"$pending_block"
[ -n "${slate_text:-}" ] && ctx="$ctx"$'\n\n'"$slate_text"
[ -n "${skew_block:-}" ] && ctx="$ctx"$'\n\n'"$skew_block"
[ -n "${chores_block:-}" ] && ctx="$ctx"$'\n\n'"$chores_block"

jq -n --arg ctx "$ctx" \
  '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $ctx}}' \
  2>/dev/null || exit 0
