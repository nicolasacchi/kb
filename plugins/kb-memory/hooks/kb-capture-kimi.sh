#!/usr/bin/env bash
# kb-capture-kimi — capture adapter: Kimi Code wire.jsonl → kb
# session-capture HTML (Claude-Code-shaped JSONL inside a <pre>).
# Sibling of kb-capture-codex.sh / kb-capture-grok.sh. Kimi Code's hook
# system is NOT Claude-shaped: hooks are [[hooks]] entries in
# ~/.kimi-code/config.toml, the stdin payload carries only
# {hook_event_name, session_id, session_title, client_type, cwd} — NO
# transcript_path — so the wire path is derived from .session_id + .cwd.
# Two modes:
#   hook mode : stdin carries the hook payload (register as a kimi Stop hook)
#   CLI mode  : kb-capture-kimi.sh <wire.jsonl>...   (backfill)
#
# The transcript lives at
#   $KIMI_CODE_HOME/sessions/<workDirKey>/<session_id>/agents/main/wire.jsonl
# with KIMI_CODE_HOME defaulting to ~/.kimi-code and
#   workDirKey = "wd_" + basename(cwd) + "_" + sha256(cwd)[0:12]
# (sha256 of the cwd string, no trailing newline). In CLI mode the session
# id is the parent directory name three levels up (the session_<uuid> dir —
# the wire itself has no sessionId field, so the dir name IS the id).
#
# Deterministic and LLM-free, mirroring kb-capture-codex.sh's envelope +
# atomic mv contract, and kb-capture-grok.sh's dual write path: the
# PREFERRED writer is `kb sessions capture` (the Rust engine — atomic
# writes, one-file-per-sid reuse, git commit resolution against cwd); a
# bash hand-rolled-HTML fallback runs when `kb` is absent or the capture
# call fails.
#
# wire.jsonl → Claude-shape mapping (drives kb's parse_session_activity;
# verified against a live wire file):
#   metadata                       → adapter-meta first line (created_at ms
#                                    → ISO; wire path recorded for recovery)
#   profile.bind.modelAlias        → message.model on assistant lines
#   turn.prompt (origin.kind=user) → user text line (.input is a content-
#                                    parts array — or its JSON string form;
#                                    text parts joined, empty skipped)
#   content.part (text / think)    → assistant text line / {type:"thinking"}
#                                    block (grok's shape)
#   tool.call                      → tool_use (names are already Claude-
#                                    shaped — Bash/Read/Write/Edit pass
#                                    through as-is; Bash .args.command
#                                    drives git-commit/SHA detection;
#                                    Write/Edit .args.path → file_path,
#                                    the key parse_session_activity reads
#                                    for the edited-set)
#   tool.result                    → tool_result (output string capped at
#                                    2000 chars like codex; the wire carries
#                                    no error flag, so is_error is omitted)
#   last usage.record              → exactly ONE trailing assistant usage
#                                    line (inputOther + inputCacheRead +
#                                    inputCacheCreation → input_tokens,
#                                    output → output_tokens)
#   Write/Edit tool.call paths     → one trailing file-history-snapshot
#   context.append_message, step.begin/end, llm.*, plan_mode.*, ... → dropped
# Timestamps: wire .time is epoch MILLIS — converted to ISO (todate).
set -u
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

TRANSLATE='
  def msts($t): if ($t | type) == "number" then (($t / 1000) | todate) else $created end;
  . as $l |
  if $l.type == "metadata" then
    {sessionId: $sid, type: "adapter-meta", adapter: "kb-capture-kimi/1",
     harness: "kimi", wire_path: $wire, cwd: $cwd,
     timestamp: msts($l.created_at)}
  elif $l.type == "turn.prompt" and ($l.origin.kind // "") == "user" then
    (if ($l.input | type) == "string"
     then (try ($l.input | fromjson) catch [])
     else ($l.input // []) end) as $parts |
    ([$parts[]? | select(.type == "text") | .text] | join("\n")) as $t |
    if $t == "" then empty else
    {sessionId: $sid, timestamp: msts($l.time), cwd: $cwd, type: "user",
     message: {role: "user", content: [{type: "text", text: $t}]}}
    end
  elif $l.type == "context.append_loop_event" and $l.event.type == "content.part" then
    ($l.event.part // {}) as $p |
    if $p.type == "text" and ($p.text // "") != "" then
      {sessionId: $sid, timestamp: msts($l.time), type: "assistant",
       message: {role: "assistant", model: $model,
                 content: [{type: "text", text: $p.text}]}}
    elif $p.type == "think" and ($p.think // "") != "" then
      {sessionId: $sid, timestamp: msts($l.time), type: "assistant",
       message: {role: "assistant", model: $model,
                 content: [{type: "thinking", thinking: $p.think}]}}
    else empty end
  elif $l.type == "context.append_loop_event" and $l.event.type == "tool.call" then
    ($l.event.args // {}) as $a |
    {sessionId: $sid, timestamp: msts($l.time), type: "assistant",
     message: {role: "assistant", content: [
       {type: "tool_use", id: ($l.event.toolCallId // ""),
        name: ($l.event.name // "tool"),
        input: (if (($l.event.name == "Write") or ($l.event.name == "Edit"))
                   and ($a | has("path"))
                then ($a + {file_path: $a.path} | del(.path))
                else $a end)}]}}
  elif $l.type == "context.append_loop_event" and $l.event.type == "tool.result" then
    (($l.event.result.output // "") | if type == "string" then . else tojson end) as $o |
    {sessionId: $sid, timestamp: msts($l.time), type: "user",
     message: {role: "user", content: [
       {type: "tool_result", tool_use_id: ($l.event.toolCallId // ""),
        content: [{type: "text", text: ($o | .[0:2000])}]}]}}
  else empty end
'

# capture_one <wire.jsonl> <session_id> <cwd>
capture_one() {
  local tpath="$1" sid="$2" cwd="$3"
  [ -f "$tpath" ] || return 0
  [ -n "$sid" ] || return 0
  [ -n "$cwd" ] || cwd="unknown"

  local created_ms created model usage edited cts
  created_ms="$(grep -m1 '"metadata"' "$tpath" | jq -r 'select(.type == "metadata") | .created_at // empty' 2>/dev/null)"
  if [ -n "$created_ms" ]; then
    created="$(jq -rn --argjson ms "$created_ms" '($ms / 1000) | todate' 2>/dev/null)"
    cts="$(date -u -d "@$((created_ms / 1000))" +%Y%m%dT%H%M%SZ 2>/dev/null)"
  fi
  [ -n "${created:-}" ] || created="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  [ -n "${cts:-}" ] || cts="$(date -u +%Y%m%dT%H%M%SZ)"
  model="$(grep -m1 '"profile.bind"' "$tpath" | jq -r 'select(.type == "profile.bind") | .modelAlias // "kimi"' 2>/dev/null)"
  [ -n "$model" ] || model="kimi"
  usage="$(grep '"usage.record"' "$tpath" | tail -1 \
    | jq -c 'select(.type == "usage.record") | .usage // empty' 2>/dev/null)"
  edited="$(grep '"tool.call"' "$tpath" \
    | jq -c 'select(.event.name == "Write" or .event.name == "Edit") | [.event.args.path // empty]' 2>/dev/null \
    | jq -s -c 'add // [] | unique')"
  [ -n "$edited" ] || edited='[]'

  local tmpjsonl
  tmpjsonl="$(mktemp)" || return 0
  jq -c --arg sid "$sid" --arg cwd "$cwd" --arg model "$model" \
     --arg wire "$tpath" --arg created "$created" "$TRANSLATE" "$tpath" \
     >"$tmpjsonl" 2>/dev/null
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
                  usage: {input_tokens: (($u.inputOther // 0)
                                         + ($u.inputCacheRead // 0)
                                         + ($u.inputCacheCreation // 0)),
                          output_tokens: ($u.output // 0)},
                  content: []}}' >>"$tmpjsonl" 2>/dev/null
  fi

  # Preferred writer: the shared Rust engine (same invocation grok uses).
  if command -v kb >/dev/null 2>&1; then
    if kb sessions capture \
         --transcript "$tmpjsonl" \
         --session-id "$sid" \
         --cwd "$cwd" \
         --out "$KB_SESSIONS_DIR" \
         >/dev/null 2>&1; then
      rm -f "$tmpjsonl"
      return 0
    fi
  fi

  # Bash fallback — hand-rolled envelope, same contract as
  # kb-capture-codex.sh (atomic tmp+mv, one file per session, overwritten
  # on re-capture; the original start timestamp survives in the name).
  mkdir -p "$KB_SESSIONS_DIR" || { rm -f "$tmpjsonl"; return 0; }
  local safe_sid out f esc tmp
  safe_sid="$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
  out=""
  for f in "$KB_SESSIONS_DIR"/session-*-"$safe_sid.html"; do
    [ -f "$f" ] && out="$f"
  done
  [ -n "$out" ] || out="$KB_SESSIONS_DIR/session-$cts-$safe_sid.html"

  # 2026-08-21 ci-host incident hardening — refuse an oversized translated
  # transcript in this bash fallback path (the Rust `kb sessions capture`
  # call above already refuses one on its own — this guards the exact case
  # where that refusal is why we're down here at all).
  local tsize
  tsize="$(stat -c %s "$tmpjsonl" 2>/dev/null || wc -c <"$tmpjsonl" 2>/dev/null)"
  if [ -n "$tsize" ] && [ "$tsize" -gt 50331648 ]; then
    echo "kb-capture-kimi.sh: skipping oversized translated transcript ($tsize bytes > 48MiB cap) for session $sid" >&2
    rm -f "$tmpjsonl"
    return 0
  fi

  esc="$(sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' "$tmpjsonl")" \
    || { rm -f "$tmpjsonl"; return 0; }
  rm -f "$tmpjsonl"
  tmp="$out.tmp"
  cat >"$tmp" <<EOF || { rm -f "$tmp"; return 0; }
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Kimi session transcript $cts</title>
<meta name="kb-category" content="memory-session">
<meta name="kb-decay" content="fast">
<meta name="kb-session" content="$safe_sid">
<meta name="kb-harness" content="kimi">
</head><body>
<h1>Kimi session transcript $cts</h1>
<pre>$esc</pre>
</body></html>
EOF
  mv -f "$tmp" "$out" 2>/dev/null || rm -f "$tmp"
}

# Resolve the wire path from a hook payload's session_id + cwd.
kimi_wire_path() {
  local sid="$1" cwd="$2"
  local home wdkey
  home="${KIMI_CODE_HOME:-$HOME/.kimi-code}"
  wdkey="wd_$(basename -- "$cwd")_$(printf '%s' "$cwd" | sha256sum | cut -c1-12)"
  printf '%s\n' "$home/sessions/$wdkey/$sid/agents/main/wire.jsonl"
}

if [ "$#" -gt 0 ]; then
  # CLI mode (backfill): session id is the session dir name — the wire path
  # is <session_dir>/agents/main/wire.jsonl, so the id sits three levels up.
  for arg in "$@"; do
    wire_dir="$(dirname -- "$arg")"
    if [ "$(basename -- "$wire_dir")" = "main" ] \
       && [ "$(basename -- "$(dirname -- "$wire_dir")")" = "agents" ]; then
      sid="$(basename -- "$(dirname -- "$(dirname -- "$wire_dir")")")"
    else
      sid="$(basename -- "$wire_dir")"
    fi
    capture_one "$arg" "$sid" ""
  done
else
  input="$(cat)"
  sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
  cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"
  [ -n "$sid" ] && [ -n "$cwd" ] || exit 0
  tpath="$(kimi_wire_path "$sid" "$cwd")"
  [ -f "$tpath" ] && capture_one "$tpath" "$sid" "$cwd"
fi
exit 0
