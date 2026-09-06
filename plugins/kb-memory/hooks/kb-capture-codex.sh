#!/usr/bin/env bash
# kb-capture-codex — capture adapter: OpenAI Codex CLI rollout JSONL →
# kb session-capture HTML (Claude-Code-shaped JSONL inside a <pre>).
#
# Codex's hook system is Claude-Code-shaped (hook_event_name, Stop,
# transcript_path, …), so this script runs in two modes:
#   hook mode : stdin carries a hooks payload with .transcript_path
#               (register it as a codex Stop hook)
#   CLI mode  : kb-capture-codex.sh <rollout.jsonl>...   (backfill)
#
# Deterministic and LLM-free, mirroring kb-capture.sh's envelope + atomic
# mv contract. The translation is a lossy, digest-oriented projection —
# tool outputs are capped, reasoning/event chatter dropped — because the
# indexed surface is the R1 digest, not the raw bytes. The rollout under
# ~/.codex/sessions stays the byte-true record; its path travels on an
# adapter-meta JSONL line so future codex-aware features can find it.
#
# Rollout → Claude-shape mapping (drives kb's parse_session_activity):
#   session_meta.payload.id            → sessionId on every line (id rung 1)
#   response_item message role=user    → user text line (wrappers → isMeta)
#   response_item message role=assist. → assistant text line
#   function_call exec_command/shell   → tool_use Bash {command}  (git commit
#                                        / push / tag detection + SHA pairing)
#   function_call_output               → tool_result (is_error from exit code)
#   patch_apply_end.changes            → tool_use Write/Edit {file_path} per file
#                                        + one trailing file-history-snapshot
#   web_search_call.action.query       → tool_use WebSearch {query}
#   custom_tool_call (apply_patch)     → generic tool_use (files ride the patch line)
#   last token_count.total_token_usage → one trailing assistant usage line
#   turn_context.model                 → message.model
set -u
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

TRANSLATE='
  .timestamp as $ts | .payload as $p |
  if .type == "session_meta" then
    {sessionId: $sid, type: "adapter-meta", adapter: "kb-capture-codex/1",
     harness: "codex", rollout_path: $rollout,
     originator: $p.originator, cli_version: $p.cli_version,
     cwd: $cwd, timestamp: $ts}
  elif .type == "response_item" and $p.type == "message" and $p.role == "user" then
    ([$p.content[]? | select(.type == "input_text") | .text] | join("\n")) as $t |
    if $t == "" then empty else
    {sessionId: $sid, timestamp: $ts, cwd: $cwd, type: "user",
     isMeta: ($t | (startswith("<")
                    or startswith("# AGENTS.md instructions")
                    or startswith("The following is the Codex agent history")
                    or contains(">>> TRANSCRIPT START"))),
     message: {role: "user", content: [{type: "text", text: $t}]}}
    end
  elif .type == "response_item" and $p.type == "message" and $p.role == "assistant" then
    ([$p.content[]? | select(.type == "output_text") | .text] | join("\n")) as $t |
    if $t == "" then empty else
    {sessionId: $sid, timestamp: $ts, type: "assistant",
     message: {role: "assistant", model: $model,
               content: [{type: "text", text: $t}]}}
    end
  elif .type == "response_item" and $p.type == "function_call" then
    (try ($p.arguments | fromjson) catch {}) as $a |
    {sessionId: $sid, timestamp: $ts, type: "assistant",
     message: {role: "assistant", content: [
       (if $p.name == "exec_command" or $p.name == "shell" or $p.name == "local_shell" then
          {type: "tool_use", id: $p.call_id, name: "Bash",
           input: {command: ($a.cmd //
                    (if ($a.command | type) == "array" then ($a.command | join(" "))
                     else ($a.command // "") end))}}
        else
          {type: "tool_use", id: $p.call_id, name: $p.name, input: $a}
        end)]}}
  elif .type == "response_item" and $p.type == "custom_tool_call" then
    {sessionId: $sid, timestamp: $ts, type: "assistant",
     message: {role: "assistant", content: [
       {type: "tool_use", id: $p.call_id, name: ($p.name // "custom_tool"), input: {}}]}}
  elif .type == "response_item"
       and ($p.type == "function_call_output" or $p.type == "custom_tool_call_output") then
    (($p.output // "") | if type == "string" then . else tojson end) as $o |
    {sessionId: $sid, timestamp: $ts, type: "user",
     message: {role: "user", content: [
       {type: "tool_result", tool_use_id: $p.call_id,
        is_error: ($o | test("exited with code [1-9]")),
        content: [{type: "text", text: ($o | .[0:2000])}]}]}}
  elif ($p.type? // "") == "web_search_call" then
    {sessionId: $sid, timestamp: $ts, type: "assistant",
     message: {role: "assistant", content: [
       {type: "tool_use", id: ("ws-" + ($ts // "t")), name: "WebSearch",
        input: {query: ($p.action.query // "")}}]}}
  elif ($p.type? // "") == "patch_apply_end" then
    (($p.changes // {}) | to_entries[]) as $ch |
    {sessionId: $sid, timestamp: $ts, type: "assistant",
     message: {role: "assistant", content: [
       {type: "tool_use", id: (($p.call_id // "patch") + "-f"),
        name: (if $ch.value.type == "add" then "Write" else "Edit" end),
        input: {file_path: $ch.key}}]}}
  else empty end
'

capture_one() {
  local tpath="$1"
  [ -f "$tpath" ] || return 0
  local meta sid ts cwd cts model usage edited
  meta="$(head -1 "$tpath" | jq -c 'select(.type == "session_meta") | .payload' 2>/dev/null)"
  [ -n "$meta" ] || return 0
  sid="$(jq -r '.id // empty' <<<"$meta")"
  [ -n "$sid" ] || return 0
  ts="$(jq -r '.timestamp // empty' <<<"$meta")"
  cwd="$(jq -r '.cwd // "unknown"' <<<"$meta")"
  cts="$(date -u -d "$ts" +%Y%m%dT%H%M%SZ 2>/dev/null)" || cts="$(date -u +%Y%m%dT%H%M%SZ)"
  model="$(grep -m1 '"turn_context"' "$tpath" | jq -r '.payload.model // "codex"' 2>/dev/null)"
  [ -n "$model" ] || model="codex"
  usage="$(grep '"token_count"' "$tpath" | tail -1 \
    | jq -c '.payload.info.total_token_usage // empty' 2>/dev/null)"
  edited="$(grep '"patch_apply_end"' "$tpath" \
    | jq -c '[.payload.changes // {} | keys[]]' 2>/dev/null | jq -s -c 'add // [] | unique')"
  [ -n "$edited" ] || edited='[]'

  local tmpjsonl
  tmpjsonl="$(mktemp)" || return 0
  jq -c --arg sid "$sid" --arg cwd "$cwd" --arg model "$model" \
     --arg rollout "$tpath" "$TRANSLATE" "$tpath" >"$tmpjsonl" 2>/dev/null
  if [ ! -s "$tmpjsonl" ]; then rm -f "$tmpjsonl"; return 0; fi
  # Trailing authoritative-edited-set snapshot + one usage-bearing line
  # (parse_session_activity sums usage per assistant record — emit exactly one).
  jq -n -c --arg sid "$sid" --argjson edited "$edited" \
    'select(($edited | length) > 0) |
     {sessionId: $sid, type: "file-history-snapshot",
      snapshot: {trackedFileBackups: ($edited | map({key: ., value: {}}) | from_entries)}}' \
    >>"$tmpjsonl" 2>/dev/null
  if [ -n "$usage" ]; then
    jq -n -c --arg sid "$sid" --arg model "$model" --argjson u "$usage" \
      '{sessionId: $sid, type: "assistant",
        message: {role: "assistant", model: $model,
                  usage: {input_tokens: ($u.input_tokens // 0),
                          output_tokens: ($u.output_tokens // 0)},
                  content: []}}' >>"$tmpjsonl" 2>/dev/null
  fi

  mkdir -p "$KB_SESSIONS_DIR" || { rm -f "$tmpjsonl"; return 0; }
  # One file per session, overwritten on re-capture (same contract as
  # kb-capture.sh — the original start timestamp survives in the name).
  local safe_sid out f
  safe_sid="$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
  out=""
  for f in "$KB_SESSIONS_DIR"/session-*-"$safe_sid.html"; do
    [ -f "$f" ] && out="$f"
  done
  [ -n "$out" ] || out="$KB_SESSIONS_DIR/session-$cts-$safe_sid.html"

  # 2026-08-21 ci-host incident hardening — refuse an oversized translated
  # transcript before it's streamed into the heredoc below. The jq
  # whitelist above already drops images/screenshots, so this should be
  # rare in practice; defense in depth against a rollout that still
  # balloons the translation (e.g. huge tool outputs).
  local tsize
  tsize="$(stat -c %s "$tmpjsonl" 2>/dev/null || wc -c <"$tmpjsonl" 2>/dev/null)"
  if [ -n "$tsize" ] && [ "$tsize" -gt 50331648 ]; then
    echo "kb-capture-codex.sh: skipping oversized translated transcript ($tsize bytes > 48MiB cap) for session $sid" >&2
    rm -f "$tmpjsonl"
    return 0
  fi

  local esc tmp
  esc="$(sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' "$tmpjsonl")" \
    || { rm -f "$tmpjsonl"; return 0; }
  rm -f "$tmpjsonl"
  tmp="$out.tmp"
  cat >"$tmp" <<EOF || { rm -f "$tmp"; return 0; }
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Codex session transcript $cts</title>
<meta name="kb-category" content="memory-session">
<meta name="kb-decay" content="fast">
<meta name="kb-session" content="$safe_sid">
<meta name="kb-harness" content="codex">
</head><body>
<h1>Codex session transcript $cts</h1>
<pre>$esc</pre>
</body></html>
EOF
  mv -f "$tmp" "$out" 2>/dev/null || rm -f "$tmp"
}

if [ "$#" -gt 0 ]; then
  for arg in "$@"; do capture_one "$arg"; done
else
  input="$(cat)"
  tpath="$(printf '%s' "$input" | jq -r '.transcript_path // empty' 2>/dev/null)"
  [ -n "$tpath" ] && capture_one "$tpath"
fi
exit 0
