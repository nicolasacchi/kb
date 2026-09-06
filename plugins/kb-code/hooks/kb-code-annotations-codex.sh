#!/usr/bin/env bash
# kb-code-annotations-codex.sh — Codex CLI PreToolUse adapter for
# kb-code-annotations.sh (V71-X1, recon `cli-agent-surface.md` open
# question 9: "should plugins/kb-code ship a Codex/omp adapter for the
# annotations hook, so the human->agent flag dialogue isn't Claude-Code-
# only?").
#
# Structurally IDENTICAL to kb-code-why-codex.sh — see that file's own
# header for the full verified-against-Codex-hooks citation. The same two
# facts hold here: `tool_name` is unconditionally "apply_patch", and the
# touched path(s) have to be extracted out of `tool_input.command`'s raw
# apply_patch envelope text rather than read off a pre-parsed field. This
# adapter re-dispatches EACH extracted path through kb-code-annotations.sh
# UNMODIFIED (still the single source of truth for the open-flags lookup +
# repos-cache + per-session/per-file cap + dedupe logic — see that
# script's own header) as a synthetic Claude-Code-shaped {tool_name:
# "Edit", tool_input:{file_path}, session_id} payload, and merges every
# non-empty additionalContext it returns into ONE PreToolUse response —
# Codex expects a single hook result per call, not one per file. A
# multi-file patch can therefore consume more than one unit of
# kb-code-annotations.sh's 6-per-session / 2-per-file injection cap in a
# single call; same accepted consequence `kb-code-why-codex.sh` already
# documents for its own cap.
#
# SessionStart has NO adapter here, on purpose: unlike Claude Code, this
# repo has no verified Codex hook event shaped like a one-shot "session
# started" summary point (`kb-code-why-codex.sh`/this file only ever
# adapt Codex's PreToolUse-equivalent) — inventing one without a citation
# would be a guess this unit's evidence bar doesn't allow. The per-file
# PreToolUse injection below still surfaces every open flag eventually
# (the very first Edit/Write of a flagged file discloses it), so Codex
# users lose the one-shot landing summary, never the underlying flags
# themselves.
#
# Fails open exactly like kb-code-annotations.sh: any parse miss, zero
# extracted paths, or zero non-empty responses -> exit 0, print nothing.
# All of kb-code-annotations.sh's own env vars (KB_CODE_ANNOTATIONS_HOOK,
# KB_CODE_DAEMON_URL, KB_CODE_TOKEN/KB_CODE_TOKEN_FILE) pass through
# unchanged — this script sets none of its own.

set -u
command -v jq >/dev/null 2>&1 || exit 0

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 0
delegate="$script_dir/kb-code-annotations.sh"
[ -x "$delegate" ] || exit 0

input="$(cat)" || exit 0

tool_name="$(printf '%s' "$input" | jq -r '.tool_name // empty' 2>/dev/null)"
[ "$tool_name" = "apply_patch" ] || exit 0

session_id="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
[ -n "$session_id" ] || exit 0

command_text="$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)"
[ -n "$command_text" ] || exit 0

# Same fixed line-prefix grammar `kb-code-why-codex.sh` greps — see that
# file's own comment for why grep-then-strip (not a JSON/AST parse) is the
# right posture for apply_patch's envelope.
paths="$(printf '%s\n' "$command_text" |
  grep -E '^\*\*\* (Add|Update|Delete) File: |^\*\*\* Move to: ' |
  sed -E 's/^\*\*\* (Add|Update|Delete) File: //; s/^\*\*\* Move to: //' |
  awk '!seen[$0]++')"
[ -n "$paths" ] || exit 0

blocks=""
while IFS= read -r file_path; do
  [ -n "$file_path" ] || continue
  synthetic="$(jq -cn --arg sid "$session_id" --arg fp "$file_path" \
    '{session_id: $sid, tool_name: "Edit", tool_input: {file_path: $fp}}')"
  out="$(printf '%s' "$synthetic" | "$delegate" 2>/dev/null)" || continue
  [ -n "$out" ] || continue
  ctx="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty' 2>/dev/null)"
  [ -n "$ctx" ] || continue
  if [ -n "$blocks" ]; then
    blocks="$blocks

$ctx"
  else
    blocks="$ctx"
  fi
done <<PATHS
$paths
PATHS

[ -n "$blocks" ] || exit 0

jq -n --arg ctx "$blocks" \
  '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $ctx}}'
