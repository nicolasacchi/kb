#!/usr/bin/env bash
# kb-capture-omp — capture adapter: Oh My Pi (omp) session JSONL → kb
# session-capture HTML (Claude-Code-shaped JSONL inside a <pre>).
# Sibling of kb-capture-codex.sh / kb-capture-kimi.sh / kb-capture-grok.sh.
#
# omp sessions live at
#   ~/.omp/agent/sessions/<encoded-cwd>/<timestamp>_<sessionId>.jsonl
# (encoded-cwd: "-<relative>" under home, "--<abs>--" otherwise; the omp
# extension hands us the exact path via ctx.sessionManager.getSessionFile(),
# so nothing is derived here). Two modes:
#   hook mode : stdin carries {session_file, session_id?, cwd?} — written by
#               plugins/kb-memory/hooks/kb-omp.ts on session_stop /
#               session.compacting / session_shutdown
#   CLI mode  : kb-capture-omp.sh <session.jsonl>...   (backfill; sid + cwd
#               are read from the file's own `session` header)
#
# Deterministic and LLM-free, mirroring the kimi adapter's envelope +
# atomic-mv contract and dual write path: the PREFERRED writer is
# `kb sessions capture` (the Rust engine); a bash hand-rolled-HTML fallback
# runs when `kb` is absent or the capture call fails.
#
# omp JSONL → Claude-shape mapping (verified against a live v3 session file):
#   fixed-width 256-byte title slot line    → dropped from the body, but its
#     trimmed value (the freshest title — omp rewrites this line in place on
#     every retitle; the "session" header's own .title is a one-time
#     snapshot from session creation and can go stale) is threaded into the
#     capture as adapter-meta.aiTitle when non-empty (OK4) — the field
#     crates/kb-core/src/sessions.rs and sessions/view.rs both already read
#     generically off ANY JSONL line (last non-empty wins, no type gate), a
#     sanctioned hook, not a workaround. `kb sessions capture` itself has no
#     --title flag and wrap_envelope() hardcodes "Session transcript <ts>";
#     aiTitle is the only sanctioned path.
#   type:"session" header                   → adapter-meta first line
#                                             (id/cwd/title recorded for recovery)
#   LEAF CHAIN ONLY — entries are an append-only tree (id/parentId); a single
#     backward pass from the last entry collects the live branch, so abandoned
#     branch experiments never pollute the captured activity. A trailing
#     reset_boundary (/clear) cuts everything at-or-before it. Every OK4
#     addition below is scoped to $live (leaf-chain only), same as the
#     pre-existing message translation.
#   message role=user                       → user text line (text parts joined)
#   message role=assistant                  → assistant line with the FULL
#     content array translated in place: text→text, thinking→thinking,
#     toolCall{id,name,arguments}→tool_use (names canonicalized bash→Bash,
#     write→Write, …; Write/Edit arguments.path aliased as file_path — the key
#     parse_session_activity reads for the edited-set). usage{input,output,
#     cacheRead,cacheWrite} → usage{input_tokens,output_tokens} per line.
#   message role=toolResult                 → user tool_result line
#     (content texts joined, capped at 2000 chars like codex; isError →
#     is_error). OK4: prefixed with a `[intent] <text>` line (cap 500 chars,
#     applied AFTER the 2000-char result cap so the intent always survives)
#     when a leaf-chain custom/tool_execution_start entry carries a matching
#     data.toolCallId → data.intent.
#   model_change                            → fallback .model for assistant
#     lines whose message carries none
#   compaction (leaf chain)                 → OK4: synthetic assistant
#     message `[compaction] <headline>` + summary body + files read/modified
#     from .details, ~4000-char total cap — the agent's own distillation of
#     discarded history, kept searchable instead of silently dropped.
#   custom/session_exit (leaf chain, final) → OK4: when data.kind != "normal"
#     or data.pendingToolCalls is non-empty, one trailing synthetic assistant
#     message `[session-exit] kind=<kind>; pending tool calls: <N>`. A clean
#     exit (kind == "normal", nothing pending) emits nothing — no noise.
#   custom/kb.recall (leaf chain)    → OK4: reconstructed as the EXACT
#     Claude-shaped hook_additional_context attachment record
#     (type:"attachment", attachment.type:"hook_additional_context",
#     hookEvent:"UserPromptSubmit") the kb-core memory_recalls parser
#     (crates/kb-core/src/sessions/view.rs) consumes, carrying one
#     reconstructed `<!--kb-recall/1 kb=<kb> id=<id>-->` marker line per
#     recorded marker — lights up memory_recalls/recalled-by for omp
#     sessions. Distinct from custom_message below: this is kb's OWN prior
#     recall re-synthesized from durable marker data the extension recorded
#     (kb.recall), never the free-text context kb injected.
#   custom_message / role=custom entries    → dropped (kb's own injected
#     recall/wake context must not echo back into the sessions corpus)
#   branch_summary, title_change, thinking_level_change, ... → dropped
#
# OK4 — subagent sidecars: when the session file <ts>_<sid>.jsonl has a
# sibling directory <ts>_<sid>/ (omp's own on-disk convention for a
# Task-tool delegation — one <AgentName>.jsonl per subagent directly inside
# it, no `subagents/` level and no `agent-` filename prefix, unlike Claude
# Code's own layout), each direct-child *.jsonl is translated with this SAME
# TRANSLATE program into <scratch>/<raw-session-id>/subagents/
# agent-<sanitized-name>.jsonl before `kb sessions capture` runs, so its
# sidecar walk (crates/kb-cli/src/commands/sessions_capture.rs:
# transcript.parent().join(&raw_sid).join("subagents")) finds them. The main
# transcript lands at <scratch>/transcript.jsonl; <raw-session-id> is read
# back off the just-translated main transcript's own adapter-meta sessionId
# — exactly the value `kb sessions capture` itself will resolve as raw_sid
# (it prefers the transcript's own sessionId over any --session-id hint,
# invariant #11), so the two can never disagree even under a hook-mode
# --session-id override. No sibling dir ⇒ this whole step is a no-op and the
# capture is byte-identical to the pre-OK4 script.
set -u
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

TRANSLATE='
  def ms2iso: if (. | type) == "number" then ((. / 1000) | todate) else null end;
  def canon:
    {bash:"Bash", read:"Read", write:"Write", edit:"Edit", grep:"Grep",
     glob:"Glob", task:"Task", web_search:"WebSearch"}[ascii_downcase]
    // ((.[0:1] | ascii_upcase) + .[1:]);
  # OK4 — capture the title-slot line'"'"'s value BEFORE it is filtered out
  # below (it is dropped from the body either way, same as before).
  (map(select(.type == "title")) | first // {}) as $tslot |
  (($tslot.title // "") | gsub("^\\s+|\\s+$"; "")) as $ttitle |
  # --- leaf chain: single backward pass (parents always precede children) ---
  map(select(.type != "title")) as $es |
  ($es | length) as $n |
  if $n == 0 then empty else
  ($es | map(select(.type == "session")) | first // {}) as $hdr |
  (if ($hdr.id // "") != "" then $hdr.id else ($es[0].id // "unknown") end) as $sid |
  ($hdr.cwd // "unknown") as $cwd |
  (($hdr.title // "")) as $stitle |
  (reduce range(($n - 1); -1; -1) as $i (
     {cur: ($es[$n - 1].id // null), keep: []};
     if .cur != null and ($es[$i].id // null) == .cur
     then {cur: ($es[$i].parentId // null), keep: ([$es[$i]] + .keep)}
     else . end)).keep as $chain |
  # /clear boundary: everything at-or-before the LAST reset_boundary is hidden
  ([ $chain | to_entries[] | select(.value.type == "reset_boundary") | .key ]
   | last // -1) as $rb |
  ($chain[$rb + 1:]) as $live |
  ($live | map(select(.type == "model_change") | .model) | last // "omp") as $dmodel |
  # OK4 — per-tool intent index: tool_execution_start.data.toolCallId ->
  # data.intent (leaf-chain-scoped — keyed off $live), so the matching
  # toolResult line below can carry why the call happened.
  ($live | map(select(.type == "custom" and .customType == "tool_execution_start"))
   | map({key: (.data.toolCallId // ""), value: (.data.intent // "")})
   | from_entries) as $intents |
  # --- emit ---
  ({sessionId: $sid, type: "adapter-meta", adapter: "kb-capture-omp/1",
    harness: "omp", cwd: $cwd, title: $stitle, session_file: $file,
    timestamp: ($hdr.timestamp // "unknown")}
   + (if $ttitle != "" then {aiTitle: $ttitle} else {} end)),
  ($live[] |
   if .type == "message" then
     (.message // {}) as $m |
     (.timestamp // ($m.timestamp | ms2iso) // "unknown") as $ts |
     if $m.role == "user" then
       ([$m.content[]? | select(.type == "text") | .text] | join("\n")) as $t |
       select($t != "") |
       {sessionId: $sid, timestamp: $ts, cwd: $cwd, type: "user",
        message: {role: "user", content: [{type: "text", text: $t}]}}
     elif $m.role == "assistant" then
       ([$m.content[]? |
         if .type == "text" and ((.text // "") != "") then
           {type: "text", text: .text}
         elif .type == "thinking" and ((.thinking // "") != "") then
           {type: "thinking", thinking: .thinking}
         elif .type == "toolCall" then
           ((.name // "tool") | canon) as $tn |
           ((.arguments // {}) as $a |
            if (($tn | ascii_downcase) == "write"
                or ($tn | ascii_downcase) == "edit") and ($a | has("path"))
            then ($a + {file_path: $a.path})
            else $a end) as $args |
           {type: "tool_use", id: (.id // ""), name: $tn, input: $args}
         else empty end]) as $blocks |
       ({role: "assistant", model: ($m.model // $dmodel), content: $blocks}
        + (if ($m.usage // null) != null then
            {usage: {input_tokens: (($m.usage.input // 0)
                                    + ($m.usage.cacheRead // 0)
                                    + ($m.usage.cacheWrite // 0)),
                     output_tokens: ($m.usage.output // 0)}}
           else {} end)) as $am |
       {sessionId: $sid, timestamp: $ts, cwd: $cwd, type: "assistant",
        message: $am}
     elif $m.role == "toolResult" then
       (([$m.content[]? | (.text // "")] | join("\n")) | .[0:2000]) as $o |
       ($intents[$m.toolCallId // ""] // "") as $intent |
       (if $intent != "" then "[intent] " + ($intent[0:500]) + "\n" + $o else $o end) as $ofinal |
       {sessionId: $sid, timestamp: $ts, cwd: $cwd, type: "user",
        message: {role: "user", content: [
          {type: "tool_result", tool_use_id: ($m.toolCallId // ""),
           is_error: ($m.isError // false),
           content: [{type: "text", text: $ofinal}]}]}}
     else empty end
   elif .type == "compaction" then
     # OK4 — the agent already distilled this discarded history itself;
     # emit it as a synthetic assistant message so it stays searchable
     # instead of silently vanishing at the compaction boundary.
     (.shortSummary // .summary // "") as $headline |
     (.summary // "") as $body |
     ((.details.readFiles // []) | join(", ")) as $reads |
     ((.details.modifiedFiles // []) | join(", ")) as $mods |
     ("[compaction] " + $headline
      + (if $body != "" then "\n\n" + $body else "" end)
      + (if $reads != "" then "\n\nfiles read: " + $reads else "" end)
      + (if $mods != "" then "\nfiles modified: " + $mods else "" end)
     ) as $ctext |
     {sessionId: $sid, timestamp: (.timestamp // "unknown"), cwd: $cwd, type: "assistant",
      message: {role: "assistant", model: $dmodel,
        content: [{type: "text", text: ($ctext[0:4000])}]}}
   elif .type == "custom" and (.customType | endswith("kb.recall")) then
     # OK4 — reconstruct the EXACT Claude-shaped hook_additional_context
     # attachment record the kb-core memory_recalls parser (view.rs)
     # consumes, so a translated omp session carries the kb-recall/1
     # markers that light up recalled-by/used. NOT a custom_message (those
     # stay dropped below) — this is kb'"'"'s own prior recall re-synthesized
     # from the durable marker data the extension recorded, never an echo
     # of the free-text context kb injected.
     (.data // {}) as $d |
     ([$d.markers[]? | "<!--kb-recall/1 kb=" + (.kb // "") + " id=" + (.id // "") + "-->"]
      | join("\n")) as $markers |
     select($markers != "") |
     {sessionId: $sid, timestamp: (.timestamp // "unknown"), cwd: $cwd, type: "attachment",
      parentUuid: null, isSidechain: false, uuid: (.id // "unknown"),
      attachment: {type: "hook_additional_context",
        content: ["Relevant memories from kb (recall — these persist across sessions):\n" + $markers],
        hookName: "UserPromptSubmit", toolUseID: "UserPromptSubmit",
        hookEvent: "UserPromptSubmit"}}
   else empty end
  ),
  # OK4 — a non-normal exit (or one with pending tool calls) is signal, not
  # noise; a normal, clean exit emits nothing.
  (
    ($live | map(select(.type == "custom" and .customType == "session_exit")) | last) as $exit |
    if $exit != null then
      (($exit.data // {}).kind // "normal") as $kind |
      ((($exit.data // {}).pendingToolCalls // []) | length) as $pending |
      if ($kind != "normal") or ($pending > 0) then
        {sessionId: $sid, timestamp: ($exit.timestamp // "unknown"), cwd: $cwd, type: "assistant",
         message: {role: "assistant", model: $dmodel, content: [
           {type: "text",
            text: ("[session-exit] kind=" + $kind + "; pending tool calls: " + ($pending | tostring))}
         ]}}
      else empty end
    else empty end
  )
  end
'

# capture_one <session.jsonl> [sid] [cwd] — sid/cwd default to the header's.
capture_one() {
  local tpath="$1" sid="${2:-}" cwd="${3:-}"
  [ -f "$tpath" ] || return 0

  local hdr created cts edited
  hdr="$(head -c 262144 "$tpath" \
    | jq -c 'select(.type == "session") | {id, cwd, timestamp}' 2>/dev/null | head -1)"
  # Caller-provided sid (hook mode / TS) wins; CLI mode falls back to the
  # file's own session header.
  if [ -z "$sid" ]; then
    sid="$(jq -r '.id // empty' <<<"$hdr" 2>/dev/null)"
  fi
  [ -n "$sid" ] || return 0
  if [ -z "$cwd" ]; then
    cwd="$(jq -r '.cwd // empty' <<<"$hdr" 2>/dev/null)"
  fi

  created="$(jq -r '.timestamp // empty' <<<"$hdr" 2>/dev/null)"
  cts="$(date -u -d "$created" +%Y%m%dT%H%M%SZ 2>/dev/null)"
  [ -n "${cts:-}" ] || cts="$(date -u +%Y%m%dT%H%M%SZ)"

  edited="$(grep '"toolCall"' "$tpath" 2>/dev/null \
    | jq -c 'select(.type == "message") | .message.content[]?
             | select(.type == "toolCall")
             | select(((.name // "") | ascii_downcase) == "write"
                      or ((.name // "") | ascii_downcase) == "edit")
             | [.arguments.path // .arguments.file_path // empty]' 2>/dev/null \
    | jq -s -c 'add // [] | unique')"
  [ -n "$edited" ] || edited='[]'

  # OK4 — a scratch DIR (not a bare file), so a sibling subagent dir can be
  # staged at <scratch>/<raw-session-id>/subagents/ beside the main
  # translated transcript — exactly the shape sessions_capture.rs's sidecar
  # walk resolves (transcript.parent().join(&raw_sid).join("subagents")).
  local scratch tmpclean tmpjsonl
  scratch="$(mktemp -d)" || return 0
  tmpclean="$(mktemp)" || { rm -rf "$scratch"; return 0; }
  tmpjsonl="$scratch/transcript.jsonl"

  # Lenient pre-clean (same policy as omp's own loader): drop unparsable
  # lines instead of aborting — a torn trailing line from a crash or an
  # active append must never kill the whole capture.
  jq -R -c 'fromjson? // empty' "$tpath" >"$tmpclean" 2>/dev/null
  jq -c -s --arg file "$tpath" "$TRANSLATE" "$tmpclean" >"$tmpjsonl" 2>/dev/null
  rm -f "$tmpclean"
  if [ ! -s "$tmpjsonl" ]; then rm -rf "$scratch"; return 0; fi

  # OK4 — subagent sidecars: <session-file-stem>/ is omp's own on-disk
  # convention for a Task-tool delegation's per-agent JSONL (verified live:
  # ~/.omp/agent/sessions/<cwd>/<ts>_<sid>/<AgentName>.jsonl — no
  # subagents/ level, no agent- prefix, unlike Claude Code). raw_sid is read
  # back off the just-translated main transcript's own adapter-meta
  # sessionId rather than the bash $sid var above, since that is exactly
  # what `kb sessions capture` itself resolves as raw_sid (it prefers the
  # transcript's own sessionId over any --session-id hint — invariant #11),
  # so the two can never disagree even under a hook-mode --session-id
  # override.
  local sdir="${tpath%.jsonl}"
  if [ -d "$sdir" ]; then
    local rsid
    rsid="$(head -1 "$tmpjsonl" | jq -r '.sessionId // empty' 2>/dev/null)"
    if [ -n "$rsid" ]; then
      local subdir_out="$scratch/$rsid/subagents"
      mkdir -p "$subdir_out" 2>/dev/null
      local f base safe subtmp
      for f in "$sdir"/*.jsonl; do
        [ -f "$f" ] || continue
        base="$(basename "$f" .jsonl)"
        safe="$(printf '%s' "$base" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
        [ -n "$safe" ] || continue
        subtmp="$(mktemp)" || continue
        jq -R -c 'fromjson? // empty' "$f" >"$subtmp" 2>/dev/null
        jq -c -s --arg file "$f" "$TRANSLATE" "$subtmp" \
          >"$subdir_out/agent-$safe.jsonl" 2>/dev/null
        rm -f "$subtmp"
        [ -s "$subdir_out/agent-$safe.jsonl" ] || rm -f "$subdir_out/agent-$safe.jsonl"
      done
    fi
  fi

  # Trailing authoritative-edited-set snapshot (parse_session_activity reads it).
  jq -n -c --arg sid "$sid" --argjson edited "$edited" \
    'select(($edited | length) > 0) |
     {sessionId: $sid, type: "file-history-snapshot",
      snapshot: {trackedFileBackups: ($edited | map({key: ., value: {}}) | from_entries)}}' \
    >>"$tmpjsonl" 2>/dev/null

  # Preferred writer: the shared Rust engine (same invocation kimi/grok use).
  if command -v kb >/dev/null 2>&1; then
    if kb sessions capture \
         --transcript "$tmpjsonl" \
         --session-id "$sid" \
         --cwd "${cwd:-unknown}" \
         --out "$KB_SESSIONS_DIR" \
         >/dev/null 2>&1; then
      rm -rf "$scratch"
      return 0
    fi
  fi

  # Bash fallback — hand-rolled envelope, same contract as the other adapters
  # (atomic tmp+mv, one file per session, overwritten on re-capture). Ignores
  # any staged subagent sidecars (Rust-only feature, W0.5/W0.6) — same as
  # pre-OK4.
  mkdir -p "$KB_SESSIONS_DIR" || { rm -rf "$scratch"; return 0; }
  local safe_sid out f esc tmp
  safe_sid="$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
  out=""
  for f in "$KB_SESSIONS_DIR"/session-*-"$safe_sid.html"; do
    [ -f "$f" ] && out="$f"
  done
  [ -n "$out" ] || out="$KB_SESSIONS_DIR/session-$cts-$safe_sid.html"

  local tsize
  tsize="$(stat -c %s "$tmpjsonl" 2>/dev/null || wc -c <"$tmpjsonl" 2>/dev/null)"
  if [ -n "$tsize" ] && [ "$tsize" -gt 50331648 ]; then
    echo "kb-capture-omp.sh: skipping oversized translated transcript ($tsize bytes > 48MiB cap) for session $sid" >&2
    rm -rf "$scratch"
    return 0
  fi

  esc="$(sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' "$tmpjsonl")" \
    || { rm -rf "$scratch"; return 0; }
  rm -rf "$scratch"
  tmp="$out.tmp"
  cat >"$tmp" <<EOF || { rm -f "$tmp"; return 0; }
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>omp session transcript $cts</title>
<meta name="kb-category" content="memory-session">
<meta name="kb-decay" content="fast">
<meta name="kb-session" content="$safe_sid">
<meta name="kb-harness" content="omp">
</head><body>
<h1>omp session transcript $cts</h1>
<pre>$esc</pre>
</body></html>
EOF
  mv -f "$tmp" "$out" 2>/dev/null || rm -f "$tmp"
}

if [ "$#" -gt 0 ]; then
  # CLI mode (backfill): sid + cwd come from each file's own session header.
  for arg in "$@"; do
    capture_one "$arg"
  done
else
  input="$(cat)"
  tpath="$(printf '%s' "$input" | jq -r '.session_file // .transcript_path // empty' 2>/dev/null)"
  sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
  cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"
  [ -n "$tpath" ] && [ -f "$tpath" ] || exit 0
  capture_one "$tpath" "$sid" "$cwd"
fi
exit 0
