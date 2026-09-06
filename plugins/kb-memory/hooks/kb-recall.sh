#!/usr/bin/env bash
# UserPromptSubmit hook — inject recalled memories into the turn.
#
# Deterministic and LLM-free: it just runs `kb recall` (a search) and
# formats the hits as additional context. It NEVER blocks the prompt —
# any failure (daemon down, jq hiccup, no hits) exits 0 with nothing
# injected. Requires `kb` on PATH and a running kb daemon.
input="$(cat)"
# Kimi Code's UserPromptSubmit payload carries .prompt as a content-parts
# ARRAY ([{"type":"text","text":...}], verified live) — join its text
# parts. Claude/Codex send a plain string; `.input` is a fallback alias.
prompt="$(printf '%s' "$input" | jq -r '
  def texts($v): if ($v | type) == "array" then ([$v[]? | .text // empty] | join("\n"))
                 elif ($v | type) == "string" then $v
                 else "" end;
  (texts(.prompt) // "") as $p | if $p != "" then $p else texts(.input) end
' 2>/dev/null)"
[ -n "$prompt" ] || exit 0

# Loopback daemon must not ride HTTP(S)_PROXY (e.g. opencode's VPN alias
# routes everything via 127.0.0.1:8892, which breaks 127.0.0.1:4000).
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy

# v0.14 S1 — drop the current session_id into a marker file so
# `kb remember` can stamp every memory it writes during this session
# with `<meta name="kb-session">`. Best-effort: any failure here is
# silent and never blocks the hook.
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
# repo-keyed marker just below, and CT-D1's scent call further down.
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

# kb-cli defaults to http://127.0.0.1:4000 — KB_DAEMON_URL lets a project
# (via .claude/settings.json `env`) point this hook at a non-default port.
extra=()
[ -n "${KB_DAEMON_URL:-}" ] && extra+=(--daemon "$KB_DAEMON_URL")

# CT-D1 — the SCENT branch, first UserPromptSubmit of a session ONLY.
#
# On turn 1 the hook has what nothing else in kb has: the real task text AND
# the cwd. That is the caller `kb context` was deferred for. But injecting
# the PACK here would auto-inject episodic material into every session's
# first turn, which is exactly what invariant #11's R0/R3 forbid ("pulled on
# demand, never auto-injected"). So turn 1 injects the COUNTS line only —
# "3 prior sessions · 2 open comments · 5 memories" — and names the verb.
# The agent decides whether to pull. Scent, not substance: that is what lets
# this ship without re-litigating R0/R3.
#
# "First turn" proxy: an ATOMIC create of a per-session marker
# (`context-scent-<sid>`) under the same cache dir the session markers above
# use. `set -o noclobber` makes `: > file` fail if the file exists, so the
# create succeeds exactly once per session id — no lock, no race, no state
# machine. Every honest failure mode falls back to today's recall behaviour:
#   * no session id (a harness that doesn't send one)  -> recall
#   * cache dir unwritable (marker can't be created)   -> recall
#   * a RESUMED session (`claude -r` reuses the sid)   -> recall
#   * daemon too old to serve /api/context, or down    -> recall
#   * an empty corpus ("no prior context")             -> recall
# Turns 2..n never enter this branch at all, so the recall block below is
# byte-identical to its pre-CT-D1 behaviour on every one of them.
first_turn=0
if [ -n "$sid" ]; then
  marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
  mkdir -p "$marker_dir" 2>/dev/null || true
  if (set -o noclobber; : >"$marker_dir/context-scent-$sid") 2>/dev/null; then
    first_turn=1
  fi
fi

scent_line=""
if [ "$first_turn" = "1" ]; then
  # ONE call. The whole point of the acceptance criterion: the hook does not
  # hand-chain recall + recollect + inbox + code-refs, it asks the one verb
  # that composes them. `--session "$sid"` keeps the pack from reporting the
  # caller's own in-flight session back at it (#11 multi-capture).
  #
  # Args go in an ARRAY, not through `${cwd:+…}` word-splitting: a cwd with a
  # space in it must not silently become two argv slots.
  ctx_args=()
  [ -n "$cwd" ] && ctx_args+=(--cwd "$cwd")
  [ -n "$sid" ] && ctx_args+=(--session "$sid")
  scent="$(kb context "$prompt" "${extra[@]}" "${ctx_args[@]}" --json 2>/dev/null \
    | jq -r '.scent // empty' 2>/dev/null)" || scent=""
  # "no prior context" is the route's honest empty-corpus answer — injecting
  # it would be a nag, so it is treated as no scent at all.
  if [ -n "$scent" ] && [ "$scent" != "no prior context" ]; then
    scent_line="kb has prior context for this task — $scent.
Counts only (nothing episodic is auto-injected). Run \`kb context \"<your task>\"\` to pull the pack: prior-session pointers, open comments on matching artifacts, and the code paths those artifacts cite."
  fi
fi

# Project-aware recall: `--cwd` lets the CLI derive its default "auto"
# scope (global corpora + the caller repo's own memory-<slug> corpus) from
# the payload cwd rather than this hook's own process cwd, which some
# harnesses set to something unrelated to the working project. `--scope
# all` is DROPPED here on purpose — it forced the old fleet-wide view on
# every recall; the CLI degrades to that same behaviour on its own when
# run outside a repo or against a daemon too old to know "auto".
recall_args=()
[ -n "$cwd" ] && recall_args+=(--cwd "$cwd")
hits="$(kb recall "$prompt" "${extra[@]}" "${recall_args[@]}" --limit 5 --json 2>/dev/null)" || exit 0

# CT-A3 — alongside the human-readable line, append ONE machine-readable
# marker per hit (`<!--kb-recall/1 kb=<kb-name> id=<hex12>[ pos=<n>]-->`),
# folded into that hit's own block (same fate as the `↳` summary line below
# — `crates/kb-core/src/sessions/view.rs`'s `ingest_attachment` treats both
# as continuation lines, never a standalone item). The capture pipeline's
# `derive_memory_recalls` PREFERS this marker over parsing the free-text
# line, so a future reformat of the human-readable block can no longer
# silently zero out the `memory_recalls` ledger.
# CT-C1/CT-C3 prefix composition (pinned by test-recall-warns.sh): a
# flagged hit renders "⚠ disputed: ", a failed-outcome hit (warns) renders
# "✗ didn't work: ", and a hit that is BOTH composes disputed-first:
# "- ⚠ disputed: ✗ didn't work: <title>". Ordinary hits stay
# byte-identical to the pre-CT-C1 "- <title>" shape.
# CT-C4 drift SUFFIX (pinned by test-recall-drift.sh): a hit with
# drift_open>0 appends " [⚠ N drift-flagged citation(s)]" after the line
# tail — composing with BOTH prefixes above; at most one /kb-verify sweep
# stale, and rendered from the recall JSON alone (this hook NEVER calls
# kb-code — the red-team killed a cross-daemon call in UserPromptSubmit).
# code_hints is deliberately NOT rendered (json consumers only) — the
# hook line stays lean.
#
# MR1 (SL6) — KB_RECALL_LAYOUT picks the SHAPE of that block. It is
# orthogonal to KB_HOOK_FMT, which only picks the envelope:
#
#   v1       today's shape, byte-identical, kept one release for anyone
#            pinning it: the `(id <hex12>, read N% — stopped at …)` /
#            `(id <hex12>, unread)` parenthetical and a marker with no
#            `pos=`; every summary capped at 220.
#   v2       THE DEFAULT. The parenthetical is gone — the id's one home is
#            the marker, and the reading percentage is noise on a one-liner
#            (it stays available through `kb memory expand`). `[kb]` stays,
#            the drift suffix now follows it directly, and depth is by RANK:
#            hits 1-2 keep up to 320 summary chars, hit 3 up to 200, hits
#            4-5 the title alone. Rank decides; nothing self-rates.
#   v2-last  v2 with the hit list REVERSED so rank 1 prints last. `pos`
#            still carries the rank, so the ledger is layout-independent.
#            For the MR2 order probe (plugins/kb-memory/bench/) only.
#
# An unknown value falls back to v2 with ONE stderr warning — a hook that
# refused or silently emitted nothing would cost the turn its memories.
layout="${KB_RECALL_LAYOUT:-v2}"
case "$layout" in
  v1 | v2 | v2-last) ;;
  *)
    printf 'kb-recall.sh: unknown KB_RECALL_LAYOUT %s — using v2\n' "$layout" >&2
    layout="v2"
    ;;
esac

block="$(printf '%s' "$hits" | jq -r --arg layout "$layout" '
  def pfx: (if (.flagged // false) then "⚠ disputed: " else "" end)
         + (if (.warns // false) then "✗ didn'\''t work: " else "" end);
  def drift: (if ((.drift_open // 0) > 0)
              then " [⚠ \(.drift_open) drift-flagged citation(s)]" else "" end);
  def summary_line($cap):
    (if $cap > 0 and (.summary // "") != ""
     then "\n    ↳ " + (.summary[0:$cap]) else "" end);
  # v1 — frozen. Every byte here is pinned by the pre-MR1 fixture
  # (tests/fixtures/recall-layout-v1.txt); never "tidy" it.
  def v1line:
    "- " + pfx + "\(.title)  [\(.kb)]  (id \(.id)"
    + (if .read_pct != null
       then ", read \(.read_pct)%"
            + (if .stopped_at != null then " — stopped at \(.stopped_at)" else "" end)
       else ", unread" end)
    + ")" + drift + summary_line(220)
    + "\n<!--kb-recall/1 kb=\(.kb) id=\(.id)-->";
  # v2 — depth by rank; the id lives only in the marker.
  def v2line($rank):
    (if $rank <= 2 then 320 elif $rank == 3 then 200 else 0 end) as $cap
    | "- " + pfx + "\(.title)  [\(.kb)]" + drift + summary_line($cap)
    + "\n<!--kb-recall/1 kb=\(.kb) id=\(.id) pos=\($rank)-->";
  (.hits // [])
  | to_entries
  | map(. as $e | ($e.key + 1) as $rank
        | $e.value
        | if $layout == "v1" then v1line else v2line($rank) end)
  | (if $layout == "v2-last" then reverse else . end)
  | if length == 0 then empty
    else "Relevant memories from kb (recall — these persist across sessions):\n" + join("\n")
    end
' 2>/dev/null)" || exit 0

# CT-D1 (orchestrator ruling, 2026-08-22) — the turn-1 scent is ADDITIVE,
# never a replacement. R0/R3 governs EPISODIC material (transcripts stay
# pull-only, which is why the scent carries counts and pointers rather than
# session bodies); memories were never covered by it — recall has pushed
# titles on every turn since v0.9 and that is the memory feature itself.
# An earlier shape had turn 1 emit the scent INSTEAD of the recall block,
# which silently cost the agent its memory titles on exactly the turn the
# task is being framed. So: recall renders as always, and the scent is
# appended below it. Either half may be empty — scent-only (an empty
# recall window but prior sessions/comments exist) is still worth saying,
# and both-empty exits silently as before.
if [ -n "$block" ] && [ -n "$scent_line" ]; then
  block="$block

$scent_line"
elif [ -z "$block" ]; then
  block="$scent_line"
fi

# SL3 — the slate per-prompt lane. With a cursor file (seeded by kb-wake.sh /
# kb-wake-kimi.sh, or by this hook on an earlier prompt) it is the DELTA
# since that cursor; without one it SEEDS: the hybrid block (`kb slate open
# --hybrid`), once, and writes the cursor — Codex has no session-start hook
# and reads the slate through this lane alone (design §12). The cursor is
# client-side only (rules matrix "Cursor"): the CLI's open/delta and this
# hook write the same head_seq. Session id ladder mirrors kb-wake.sh's
# (payload -> env -> marker). Never blocks the prompt: `timeout 2` (delta) /
# `timeout 4` (seed) cap the added wall time, and any failure — kb missing,
# non-zero exit, malformed JSON, no git repo — is silent and leaves the
# output byte-identical to today.
slate_sid="$sid"
[ -n "$slate_sid" ] || slate_sid="${KB_SESSION_ID:-}"
if [ -z "$slate_sid" ]; then
  cs_marker="${XDG_CACHE_HOME:-$HOME/.cache}/kb/current-session"
  [ -f "$cs_marker" ] && slate_sid="$(cat "$cs_marker" 2>/dev/null)"
fi

slate_delta=""
if [ -n "$slate_sid" ]; then
  cursor_file="${XDG_CACHE_HOME:-$HOME/.cache}/kb/slate-cursor-$slate_sid"
  cursor=""
  [ -f "$cursor_file" ] && cursor="$(cat "$cursor_file" 2>/dev/null)"
  if [ -n "$cursor" ]; then
    slate_json="$(timeout 2 kb slate delta --since "$cursor" \
      --session-id "$slate_sid" --cwd "$cwd" "${extra[@]}" --budget 1500 --json 2>/dev/null)" || slate_json=""
  else
    slate_json="$(timeout 4 kb slate open --hybrid --budget 2000 \
      --session-id "$slate_sid" --cwd "$cwd" "${extra[@]}" --json 2>/dev/null)" || slate_json=""
  fi
  if [ -n "$slate_json" ]; then
    slate_delta="$(printf '%s' "$slate_json" | jq -r '.text // empty' 2>/dev/null)" || slate_delta=""
    # Seed or advance the cursor whenever the daemon answered with a head_seq,
    # even when nothing rendered: "seen" means the daemon served this session
    # everything up to head, so the next prompt never re-asks for a range
    # that produced no text.
    slate_head_seq="$(printf '%s' "$slate_json" | jq -r '.head_seq // empty' 2>/dev/null)" || slate_head_seq=""
    if [ -n "$slate_head_seq" ]; then
      mkdir -p "$(dirname "$cursor_file")" 2>/dev/null || true
      printf '%s\n' "$slate_head_seq" >"$cursor_file.tmp" 2>/dev/null \
        && mv "$cursor_file.tmp" "$cursor_file" 2>/dev/null \
        || true
    fi
  fi
fi

if [ -n "$slate_delta" ]; then
  if [ -n "$block" ]; then
    block="$block"$'\n\n'"$slate_delta"
  else
    block="$slate_delta"
  fi
fi

[ -n "$block" ] || exit 0

# Kimi Code appends a hook's PLAIN stdout to the model's context — no
# hookSpecificOutput envelope — so KB_HOOK_FMT=kimi prints the block
# bare. Default stays byte-identical to the Claude/Codex shape.
if [ "${KB_HOOK_FMT:-}" = "kimi" ]; then
  printf '%s\n' "$block"
else
  jq -n --arg ctx "$block" \
    '{hookSpecificOutput: {hookEventName: "UserPromptSubmit", additionalContext: $ctx}}' \
    2>/dev/null || exit 0
fi
