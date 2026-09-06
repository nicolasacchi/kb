#!/usr/bin/env bash
# kb-capture-opencode — capture adapter: opencode export JSON →
# kb session-capture HTML (Claude-Code-shaped JSONL inside a <pre>).
#
# Modes:
#   CLI : kb-capture-opencode.sh <sessionID>|<.json export>...
#   hook: stdin is ignored; pass session IDs as args (the opencode plugin
#         shells this after session.idle with the idle sessionID).
#
# Deterministic and LLM-free. Translates message/part JSON into the same
# Claude-shaped JSONL kb-capture.sh / kb-capture-codex.sh emit so the
# digest, kb why, and kb recollect work across harnesses. Lossy on purpose
# (tool outputs capped, reasoning dropped). The export path (when a file)
# or session id rides an adapter-meta line.
set -u
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0
command -v opencode >/dev/null 2>&1 || true

# Loopback daemon not needed here (file write only), but keep env clean
# for any nested tools that might call kb later.
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy

OUT_CAP=2000

translate_export() {
  local src="$1"  # path to export JSON
  local sid cwd model title

  # 2026-08-21 ci-host incident hardening — refuse an oversized export before
  # jq ever reads it (defense in depth: an opencode export is not the
  # bare Codex-rollout shape the incident hit, but the same "refuse, don't
  # choke on it" posture applies to anything entering a capture whole).
  local tsize
  tsize="$(stat -c %s "$src" 2>/dev/null || wc -c <"$src" 2>/dev/null)"
  if [ -n "$tsize" ] && [ "$tsize" -gt 50331648 ]; then
    echo "kb-capture-opencode.sh: skipping oversized export ($tsize bytes > 48MiB cap): $src" >&2
    return 0
  fi

  sid="$(jq -r '.info.id // empty' "$src" 2>/dev/null)"
  [ -n "$sid" ] || return 0
  cwd="$(jq -r '.info.directory // empty' "$src" 2>/dev/null)"
  model="$(jq -r '.info.model.id // .info.model.modelID // "opencode"' "$src" 2>/dev/null)"
  title="$(jq -r '.info.title // empty' "$src" 2>/dev/null)"

  local safe_sid ts out tmp jsonl
  safe_sid="$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
  ts="$(date -u +%Y%m%dT%H%M%SZ)"
  out=""
  for f in "$KB_SESSIONS_DIR"/session-*-"$safe_sid.html"; do
    [ -f "$f" ] && out="$f"
  done
  [ -n "$out" ] || out="$KB_SESSIONS_DIR/session-$ts-$safe_sid.html"

  jsonl="$(jq -c --arg sid "$sid" --arg cwd "$cwd" --arg model "$model" \
    --arg title "$title" --arg src "$src" --argjson cap "$OUT_CAP" '
    def cap_str:
      if type != "string" then .|tostring
      elif length > $cap then .[0:$cap] + "…[truncated]"
      else . end;
    def tool_name:
      if .tool == "bash" then "Bash"
      elif .tool == "edit" then "Edit"
      elif .tool == "write" then "Write"
      elif .tool == "read" then "Read"
      elif .tool == "glob" then "Glob"
      elif .tool == "grep" then "Grep"
      elif .tool == "webfetch" then "WebFetch"
      elif .tool == "websearch" then "WebSearch"
      else (.tool // "tool") end;
    def tool_input:
      (.state.input // {}) as $in |
      if .tool == "bash" then {command: ($in.command // "")}
      elif .tool == "edit" then {file_path: ($in.filePath // $in.path // ""), old_string: ($in.oldString // ""), new_string: ($in.newString // "")}
      elif .tool == "write" then {file_path: ($in.filePath // $in.path // ""), content: ($in.content // "")}
      elif .tool == "read" then {file_path: ($in.filePath // $in.path // "")}
      elif .tool == "glob" then {pattern: ($in.pattern // "")}
      elif .tool == "grep" then {pattern: ($in.pattern // ""), path: ($in.path // "")}
      elif .tool == "webfetch" then {url: ($in.url // "")}
      else $in end;
    # adapter-meta first
    {sessionId: $sid, type: "adapter-meta", adapter: "kb-capture-opencode/1",
     harness: "opencode", export_path: $src, cwd: $cwd, title: $title,
     timestamp: (now | todateiso8601)},
    # then messages
    (.messages // [])[] |
    . as $m |
    ($m.info.role // "user") as $role |
    ($m.info.time.created // null) as $tms |
    (if $tms != null then (($tms / 1000) | todateiso8601) else (now | todateiso8601) end) as $ts |
    if $role == "user" then
      ([$m.parts[]? | select(.type == "text") | .text // empty] | join("\n")) as $text |
      if $text == "" then empty else
      {sessionId: $sid, timestamp: $ts, cwd: $cwd, type: "user",
       message: {role: "user", content: [{type: "text", text: $text}]}}
      end
    elif $role == "assistant" then
      # text
      ([$m.parts[]? | select(.type == "text") | .text // empty] | join("\n")) as $text |
      (if $text == "" then empty else
       {sessionId: $sid, timestamp: $ts, type: "assistant",
        message: {role: "assistant", model: $model,
                  content: [{type: "text", text: $text}]}}
       end),
      # tools → tool_use + tool_result pairs
      ($m.parts[]? | select(.type == "tool") |
        (.callID // .id // "call") as $cid |
        (.state.status // "") as $st |
        {sessionId: $sid, timestamp: $ts, type: "assistant",
         message: {role: "assistant", model: $model, content: [
           {type: "tool_use", id: $cid, name: tool_name, input: tool_input}]}},
        {sessionId: $sid, timestamp: $ts, type: "user",
         message: {role: "user", content: [
           {type: "tool_result", tool_use_id: $cid,
            content: ((.state.output // "") | cap_str),
            is_error: ($st == "error" or $st == "failed")}]}}
      )
    else empty end
  ' "$src" 2>/dev/null)" || return 0

  [ -n "$jsonl" ] || return 0

  mkdir -p "$KB_SESSIONS_DIR" || return 0
  tmp="$out.tmp"
  {
    printf '%s\n' '<!DOCTYPE html>'
    printf '%s\n' '<html lang="en"><head><meta charset="utf-8">'
    printf '<title>Session transcript %s</title>\n' "$ts"
    printf '%s\n' '<meta name="kb-category" content="memory-session">'
    printf '%s\n' '<meta name="kb-decay" content="fast">'
    printf '<meta name="kb-session" content="%s">\n' "$sid"
    printf '%s\n' '<meta name="kb-harness" content="opencode">'
    printf '%s\n' '</head><body>'
    printf '<h1>Session transcript %s</h1>\n' "$ts"
    printf '%s\n' '<pre>'
    # HTML-escape the JSONL
    printf '%s\n' "$jsonl" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
    printf '%s\n' '</pre></body></html>'
  } >"$tmp" || { rm -f "$tmp"; return 0; }
  mv -f "$tmp" "$out" 2>/dev/null || rm -f "$tmp"
}

capture_one() {
  local arg="$1"
  local tmp
  if [ -f "$arg" ]; then
    translate_export "$arg"
    return
  fi
  # Treat as session id — export via opencode CLI
  command -v opencode >/dev/null 2>&1 || return 0
  tmp="$(mktemp "${TMPDIR:-/tmp}/kb-oc-export.XXXXXX.json")" || return 0
  if env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
       opencode export "$arg" >"$tmp" 2>/dev/null \
     && [ -s "$tmp" ]; then
    translate_export "$tmp"
  fi
  rm -f "$tmp"
}

if [ "$#" -eq 0 ]; then
  # No args: nothing to do (plugin always passes the session id)
  exit 0
fi

for arg in "$@"; do
  capture_one "$arg" || true
done
exit 0
