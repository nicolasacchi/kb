#!/usr/bin/env bash
# kb-code-why-codex.sh — Codex CLI PreToolUse adapter for kb-code-why.sh.
#
# VERIFIED against developers.openai.com/codex/hooks (2026-07-21) + cross-
# checked against `strings` on the installed codex binary: Codex's
# PreToolUse hook is Claude-Code-shaped (same `hookSpecificOutput.
# additionalContext` response envelope kb-code-why.sh already emits) but
# its `tool_name` is UNCONDITIONALLY "apply_patch" — even though the
# hooks.json `matcher` accepts "Edit"/"Write"/"apply_patch" as interchange-
# able aliases, the payload itself never says "Edit"/"Write". There is also
# no `tool_input.file_path`: `tool_input.command` carries the raw
# apply_patch patch envelope text (`*** Begin Patch` / `*** Update File:
# <path>` / `*** Add File: <path>` / `*** Delete File: <path>` /
# `*** Move to: <path>` / ... / `*** End Patch`), which can touch several
# files in one call — there is no pre-parsed path or file list on this
# event (that's a different, unrelated `patch_apply_end` transcript event).
#
# This adapter extracts every file path out of that patch text, then
# re-dispatches EACH one through kb-code-why.sh UNMODIFIED (still the single
# source of truth for the provenance lookup/cache/session-cap/confidence-
# gate logic) as a synthetic Claude-Code-shaped {tool_name:"Edit",
# tool_input:{file_path}} payload, and merges every non-empty
# additionalContext it returns into ONE PreToolUse response — Codex expects
# a single hook result per call, not one per file. A multi-file patch can
# therefore consume more than one unit of kb-code-why.sh's 3-per-session
# injection cap in a single call; that's an accepted consequence of Codex's
# multi-file-per-call semantics, not a bug.
#
# Fails open exactly like kb-code-why.sh: any parse miss, zero extracted
# paths, or zero non-empty responses -> exit 0, print nothing. All of
# kb-code-why.sh's own env vars (KB_CODE_WHY_HOOK, KB_CODE_DAEMON_URL,
# KB_DAEMON_URL) pass through unchanged — this script sets none of its own.

set -u
command -v jq >/dev/null 2>&1 || exit 0

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 0
delegate="$script_dir/kb-code-why.sh"
[ -x "$delegate" ] || exit 0

input="$(cat)" || exit 0

tool_name="$(printf '%s' "$input" | jq -r '.tool_name // empty' 2>/dev/null)"
[ "$tool_name" = "apply_patch" ] || exit 0

session_id="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
[ -n "$session_id" ] || exit 0

command_text="$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)"
[ -n "$command_text" ] || exit 0

# "*** {Add,Update,Delete} File: <path>" plus a trailing "*** Move to:
# <path>" rename target (follows an Update File line in the same section) —
# order-preserving, de-duplicated. A plain string match (not a JSON/AST
# parse) is deliberate: apply_patch's envelope is a fixed line-prefix
# grammar, and grep-then-strip is the same "don't overbuild a parser for a
# fixed marker format" posture kb-capture-codex.sh's JQ translate table
# already takes with this same envelope.
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
