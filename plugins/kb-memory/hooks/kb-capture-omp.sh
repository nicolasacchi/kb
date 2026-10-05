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
# Deterministic and LLM-free. Landing (v0.45 N4): the translated JSONL (its
# first record is an `adapter-meta` line carrying `harness: "omp"`, the enrich
# ladder's rung 1) goes through `kb sessions capture` (the Rust engine). When
# that fails, or `kb` is missing, the UNSCRUBBED translation is parked in the
# private capture spool (kb-hook-lib.sh hook_adapter_land) and replayed
# through the same scrubbed path by the next successful capture; this adapter
# NEVER writes HTML itself, so nothing raw can reach the corpus. The spool
# keeps the main transcript only: staged subagent sidecars are folded into the
# capture by the live `kb sessions capture` call and are not spooled.
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
#     recorded marker. When the marker has a title, a
#     `- <title>  [<kb>]  (id <id>)` bullet is emitted on the line before
#     it so the free-text fallback grammar still parses; a title-less
#     marker stays bare (view.rs counts one row per marker) and still
#     lights up memory_recalls/recalled-by for omp sessions. Distinct
#     from custom_message below: this is kb's OWN prior recall
#     re-synthesized from durable marker data the extension recorded
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

# Hook mode: become a session leader so everything this capture spawns is
# ours to reap (see "lifecycle" below) and a caller's group-wide signal or
# a terminal never reaches us by accident. `setsid` from a non-leader keeps
# the PID, so a caller that signals the PID it spawned (the pre-OC
# kb-omp.ts: SIGTERM to the shell only) still reaches this script, whose
# trap then reaps its own session. stdin passes through the exec.
if [ "$#" -eq 0 ] && [ -z "${KB_CAPTURE_ISOLATED:-}" ] \
  && command -v setsid >/dev/null 2>&1 && command -v ps >/dev/null 2>&1; then
  _sid="$(ps -o sid= -p "$$" 2>/dev/null | tr -d ' ')"
  _pgid="$(ps -o pgid= -p "$$" 2>/dev/null | tr -d ' ')"
  if [ -n "$_sid" ] && [ "$_sid" != "$$" ] && [ "$_pgid" != "$$" ]; then
    export KB_CAPTURE_ISOLATED=1
    exec setsid "${BASH:-bash}" "$0"
  fi
fi

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
  # Linear walk (v0.45 OC): only the scalar cursor lives in the loop state and
  # the chain is COLLECTED by foreach. The earlier reduce carried the growing
  # `keep` array in its state and prepended to it, copying the whole array on
  # every kept record (O(chain^2): ~38 s at 20k records, ~3 min at 40k).
  # Semantics are unchanged: a null cursor stops the walk, and the first
  # backward match wins on a duplicate id.
  ([foreach range(($n - 1); -1; -1) as $i (
     {cur: ($es[$n - 1].id // null), hit: false};
     if .cur != null and ($es[$i].id // null) == .cur
     then {cur: ($es[$i].parentId // null), hit: true}
     else {cur: .cur, hit: false} end;
     if .hit then $es[$i] else empty end)] | reverse) as $chain |
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
     # Title bullet (when recorded) keeps the free-text fallback alive.
     # A title-less marker stays bare — one row per marker in view.rs.
     ([$d.markers[]? |
       (.kb // "") as $kb | (.id // "") as $id |
       ((.title // "") | gsub("^\\s+|\\s+$"; "")) as $title |
       (if $title != "" then
          "- " + $title + "  [" + $kb + "]  (id " + $id + ")\n"
        else "" end)
       + "<!--kb-recall/1 kb=" + $kb + " id=" + $id + "-->"]
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

# v0.44 X6 (INT4) - every `kb` call is bounded by the shared hook deadline
# (kb-hook-lib.sh run_to), so a hung daemon/CLI can never hang the session
# end; the harness timeout is the last resort, not the design. A standalone
# copy without the lib runs its calls unbounded, as before.
. "$(dirname "$0")/kb-hook-lib.sh" 2>/dev/null || {
  # Standalone copy without the lib: capture only, no spool, never any HTML.
  run_to() { shift; "$@"; }
  hook_deadline_init() { :; }
  hook_agent_safe_name() { return 1; }
  hook_spool_put_sidecars() { return 1; }
  hook_capture_lock_dir() { return 1; }
  hook_capture_key() { return 1; }
  hook_adapter_land() {
    command -v kb >/dev/null 2>&1 || { echo "kb-capture-omp.sh: kb not found - session $1 not captured" >&2; return 0; }
    kb sessions capture --transcript "$2" --session-id "$1" ${4:+--stamp "$4"} \
      --out "$KB_SESSIONS_DIR" >/dev/null 2>&1 \
      || echo "kb-capture-omp.sh: kb sessions capture failed - session $1 not captured" >&2
    return 0
  }
}
KB_HOOK_BUDGET_SECS="${KB_CAPTURE_BUDGET_SECS:-25}"


# --- lifecycle (v0.45 OC) ----------------------------------------------------
# A growing session is captured from three triggers (every turn end, a
# compaction, shutdown), from several omp processes at once, and a large one
# can take minutes. This script therefore owns its lifecycle:
#   * EXCLUSION   one active conversion per canonical session file, across
#                 processes (flock, key = hash of the realpath). A request that
#                 finds the lock held only BUMPS a request counter and returns;
#                 the owner re-reads the source afresh and runs another pass
#                 until no request arrived during the last one (coalescing,
#                 never a silent drop). Independent sessions never contend.
#   * FRESHNESS   only the lock holder publishes, strictly in order, and it
#                 re-converts the live file on every pass, so a stale
#                 conversion cannot overwrite a fresher one. A pass is skipped
#                 only when the input fingerprint (parent inode:size:mtime-ns,
#                 a hash of the first 512 bytes = the in-place title slot, the
#                 sidecar listing, this script's version) equals the one
#                 recorded after the last SUCCESSFUL landing AND the capture
#                 file still exists. The fingerprint is taken BEFORE reading, so
#                 an append during conversion forces another pass; it is
#                 recorded only when hook_adapter_land returned 0 (rc 1 = only
#                 parked in the spool, rc 2 = lost), so failed work is never
#                 recorded as captured. CLI backfill never skips.
#   * OWNERSHIP   in hook mode the script makes itself a session leader
#                 (setsid; same PID, so a caller that signals the PID it spawned
#                 still reaches us). Every long child runs in the background
#                 under `timeout` (the remaining hard deadline) and is awaited
#                 with `wait`, which a trapped signal interrupts - a foreground
#                 child would defer the trap until it exited. TERM/INT/HUP and
#                 EXIT reap every process of OUR session (not our process
#                 group: timeout(1) moves its child into a group of its own),
#                 remove the scratch files and release the lock. Nothing
#                 outside our session is ever signalled.
#   * DEADLINE    KB_CAPTURE_HARD_SECS (default 120, the caller's own timeout)
#                 per pass: each child is capped at what is left of it.
CAP_HARD="${KB_CAPTURE_HARD_SECS:-120}"
CAP_LOCK_WAIT="${KB_CAPTURE_LOCK_WAIT_SECS:-60}"
CAP_VERSION="kb-capture-omp/1+oc1"
CAP_TMP=()
CAP_CLEANED=""
CAP_HAVE_TIMEOUT=""
command -v timeout >/dev/null 2>&1 && CAP_HAVE_TIMEOUT=1

# Pids of every process this script owns (never $$ itself).
cap_owned_pids() {
  command -v ps >/dev/null 2>&1 || return 0
  local mysid
  mysid="$(ps -o sid= -p "$$" 2>/dev/null | tr -d ' ')"
  if [ "$mysid" = "$$" ]; then
    ps -s "$$" -o pid= 2>/dev/null | tr -d ' ' | grep -vx "$$"
  else
    # Not a session leader (setsid unavailable / CLI mode): our descendants.
    ps -e -o pid=,ppid= 2>/dev/null | awk -v root="$$" '
      { pp[$1] = $2 }
      END {
        mark[root] = 1; changed = 1
        while (changed) {
          changed = 0
          for (p in pp) if (!(p in mark) && (pp[p] in mark)) { mark[p] = 1; changed = 1; print p }
        }
      }'
  fi
  return 0
}

cap_alive() { # alive and not a zombie
  local st
  st="$(ps -o stat= -p "$1" 2>/dev/null | tr -d ' ')"
  [ -n "$st" ] && [ "${st#Z}" = "$st" ]
}

cap_reap() {
  local pids p i alive
  pids="$(cap_owned_pids)"
  [ -n "$pids" ] || return 0
  for p in $pids; do kill -TERM "$p" 2>/dev/null; done
  for i in $(seq 1 25); do
    alive=""
    for p in $pids; do
      if cap_alive "$p"; then alive=1; break; fi
    done
    [ -z "$alive" ] && return 0
    sleep 0.1
  done
  for p in $pids; do cap_alive "$p" && kill -KILL "$p" 2>/dev/null; done
  return 0
}

# --- survivor watchdog -------------------------------------------------------
# A SIGKILL of this script (an external kill, the OOM killer, the caller's
# group-kill backstop) runs no trap, and `timeout(1)` moves its child into a
# process group of its own, so even a group-wide kill misses jq/kb. Two things
# make that case safe anyway:
#   1. children never inherit the lock fd (cap_bg closes fd 9 for them), so the
#      per-session lock dies WITH the owner and the next request takes it;
#   2. a tiny watchdog, in a session of its own (it survives a kill of ours),
#      polls the owner. When the owner is gone it reaps every process still
#      carrying this run's KB_CAPTURE_RUN marker (plus the owner's session),
#      then removes the run's scratch directory.
# Normal exits and trapped signals stop the watchdog themselves.
CAP_RUN=""
CAP_WD=""
CAP_WD_SRC='
owner="$1"; marker="$2"; run="$3"; start="$4"
alive() {
  local st lst
  st="$(ps -o stat= -p "$owner" 2>/dev/null | tr -d " ")"
  [ -n "$st" ] && [ "${st#Z}" = "$st" ] || return 1
  lst="$(ps -o lstart= -p "$owner" 2>/dev/null)"
  [ "$lst" = "$start" ]
}
sp=""
trap '"'"'[ -n "$sp" ] && kill "$sp" 2>/dev/null; exit 0'"'"' TERM
while alive && [ -d "$run" ]; do
  sleep 0.5 & sp=$!
  wait "$sp"
done
[ -d "$run" ] || exit 0
members() {
  ps -s "$owner" -o pid= 2>/dev/null | tr -d " "
  local d p
  for d in /proc/[0-9]*; do
    p="${d#/proc/}"
    [ "$p" = "$$" ] && continue
    tr "\0" "\n" <"$d/environ" 2>/dev/null | grep -qx "KB_CAPTURE_RUN=$marker" && echo "$p"
  done
}
pids="$(members | sort -u)"
for p in $pids; do kill -TERM "$p" 2>/dev/null; done
sleep 2
for p in $pids; do
  st="$(ps -o stat= -p "$p" 2>/dev/null | tr -d " ")"
  [ -n "$st" ] && [ "${st#Z}" = "$st" ] && kill -KILL "$p" 2>/dev/null
done
rm -rf "$run"
'

cap_run_init() { # once per process: the run dir + the watchdog
  [ -z "$CAP_RUN" ] || return 0
  CAP_RUN="$(mktemp -d)" || { CAP_RUN=""; return 0; }
  export KB_CAPTURE_RUN="$$.$RANDOM$RANDOM"
  if [ -n "${KB_CAPTURE_NO_WATCHDOG:-}" ] || ! command -v setsid >/dev/null 2>&1 \
    || ! command -v ps >/dev/null 2>&1; then
    return 0
  fi
  local start
  start="$(ps -o lstart= -p "$$" 2>/dev/null)"
  [ -n "$start" ] || return 0
  env -u KB_CAPTURE_RUN setsid "${BASH:-bash}" -c "$CAP_WD_SRC" cap-watchdog \
    "$$" "$KB_CAPTURE_RUN" "$CAP_RUN" "$start" 9>&- </dev/null >/dev/null 2>&1 &
  CAP_WD=$!
  return 0
}

cap_cleanup() {
  [ -z "$CAP_CLEANED" ] || return 0
  CAP_CLEANED=1
  trap '' TERM INT HUP
  cap_reap
  local i=0
  while [ "$i" -lt "${#CAP_TMP[@]}" ]; do
    [ -n "${CAP_TMP[$i]}" ] && rm -rf "${CAP_TMP[$i]}" 2>/dev/null
    i=$((i + 1))
  done
  { exec 9>&-; } 2>/dev/null
  if [ -n "$CAP_WD" ]; then
    kill -TERM "$CAP_WD" 2>/dev/null
    for i in $(seq 1 40); do kill -0 "$CAP_WD" 2>/dev/null || break; sleep 0.05; done
    kill -KILL "$CAP_WD" 2>/dev/null
  fi
  [ -n "$CAP_RUN" ] && rm -rf "$CAP_RUN" 2>/dev/null
  return 0
}
cap_on_signal() { cap_cleanup; exit 143; }

cap_track() { CAP_TMP+=("$1"); }
cap_untrack() { # rm now and forget
  local i=0
  while [ "$i" -lt "${#CAP_TMP[@]}" ]; do
    [ "${CAP_TMP[$i]}" = "$1" ] && CAP_TMP[$i]=""
    i=$((i + 1))
  done
  rm -rf "$1" 2>/dev/null
}

# Run "$@" in the background and wait for it. `wait` is interruptible by a
# trapped signal; a foreground child is not.
# The child does NOT inherit the lock fd (9): the lock must die with the owner.
cap_bg() {
  "$@" 9>&- &
  local p=$!
  wait "$p"
  local rc=$?
  return "$rc"
}
# Same, but keeps fd 9: only for `flock ... 9` itself.
cap_bg_lock() {
  "$@" &
  local p=$!
  wait "$p"
  return $?
}
# A deadline kill / TERM is fatal for the pass; any other failure is not.
cap_fatal_rc() { [ "$1" = 124 ] || [ "$1" = 137 ] || [ "$1" = 143 ]; }

# Cap an EXTERNAL command at what is left of this pass's hard deadline.
capt() {
  local left=$((CAP_HARD - SECONDS))
  [ "$left" -gt 0 ] || return 124
  if [ -n "$CAP_HAVE_TIMEOUT" ]; then
    timeout -k 2 "$left" "$@"
  else
    "$@"
  fi
}

# The authoritative edited-set (lenient: a torn trailing line is skipped).
cap_edited_set() {
  capt grep '"toolCall"' "$1" 2>/dev/null \
    | capt jq -R -c 'fromjson? | select(.type == "message") | .message.content[]?
             | select(.type == "toolCall")
             | select(((.name // "") | ascii_downcase) == "write"
                      or ((.name // "") | ascii_downcase) == "edit")
             | [.arguments.path // .arguments.file_path // empty]' 2>/dev/null \
    | capt jq -s -c 'add // [] | unique'
  local st=("${PIPESTATUS[@]}")
  [ "${st[0]}" -le 1 ] && [ "${st[1]}" -eq 0 ] && [ "${st[2]}" -eq 0 ]
}

# Input fingerprint (see the header). rc 1 = cannot be computed (never skip).
cap_fingerprint() {
  local f="$1" sdir="${1%.jsonl}" a b c=""
  a="$(stat -c '%i:%s:%.9Y' -- "$f" 2>/dev/null)" || return 1
  [ -n "$a" ] || return 1
  b="$(head -c 512 -- "$f" 2>/dev/null | cksum)"
  if [ -d "$sdir" ]; then
    c="$(find "$sdir" -maxdepth 1 -name '*.jsonl' -printf '%f %s %T@ %i\n' 2>/dev/null | LC_ALL=C sort)"
  fi
  printf '%s\n%s\n%s\n%s\n' "$CAP_VERSION" "$a" "$b" "$c" | { sha256sum 2>/dev/null || cksum; } | cut -d' ' -f1-2
}

cap_capture_exists() { compgen -G "$KB_SESSIONS_DIR/session-*-$(hook_sid_key "$1").html" >/dev/null 2>&1; }

cap_req_bump() { # <base>
  (
    flock 8 || exit 0
    n="$(cat "$1.req" 2>/dev/null)"
    case "$n" in '' | *[!0-9]*) n=0 ;; esac
    printf '%s\n' "$((n + 1))" >"$1.req"
  ) 8>"$1.reqlock" 9>&- 2>/dev/null
  return 0
}
cap_req_read() { local n; n="$(cat "$1.req" 2>/dev/null)"; printf '%s' "${n:-0}"; }

# capture_pass <session.jsonl> [sid] [cwd] [force] [lock-base] - ONE conversion
# + landing. rc 0 = captured or legitimately skipped, 1 = not captured.
capture_pass() {
  hook_deadline_init # per-session budget (a backfill runs many)
  local tpath="$1" sid="${2:-}" cwd="${3:-}" force="${4:-0}" base="${5:-}"
  [ -f "$tpath" ] || return 0

  local hdr created cts fp=""
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

  if [ -n "$base" ]; then
    fp="$(cap_fingerprint "$tpath")" || fp=""
    if [ "$force" != 1 ] && [ -n "$fp" ] \
      && [ "$(cat "$base.done" 2>/dev/null)" = "$fp" ] && cap_capture_exists "$sid"; then
      return 0 # nothing changed since the last successful landing
    fi
  fi

  # OK4 - a scratch DIR (not a bare file), so a sibling subagent dir can be
  # staged at <scratch>/<raw-session-id>/subagents/ beside the main
  # translated transcript - exactly the shape sessions_capture.rs's sidecar
  # walk resolves (transcript.parent().join(&raw_sid).join("subagents")).
  local scratch tmpclean tmpjsonl edited
  cap_run_init
  scratch="$(mktemp -d "${CAP_RUN:-${TMPDIR:-/tmp}}/p.XXXXXX")" || return 1
  cap_track "$scratch"
  tmpclean="$scratch/clean.jsonl"
  tmpjsonl="$scratch/transcript.jsonl"

  cap_bg cap_edited_set "$tpath" >"$scratch/edited.json" || { cap_untrack "$scratch"; return 1; }
  edited="$(cat "$scratch/edited.json" 2>/dev/null)"
  [ -n "$edited" ] || edited='[]'

  # Lenient pre-clean (same policy as omp's own loader): drop unparsable
  # lines instead of aborting - a torn trailing line from a crash or an
  # active append must never kill the whole capture.
  if ! cap_bg capt jq -R -c 'fromjson? // empty' "$tpath" >"$tmpclean" 2>/dev/null; then
    cap_untrack "$scratch"; return 1
  fi
  if ! cap_bg capt jq -c -s --arg file "$tpath" "$TRANSLATE" "$tmpclean" >"$tmpjsonl" 2>/dev/null; then
    cap_untrack "$scratch"; return 1
  fi
  rm -f "$tmpclean"
  if [ ! -s "$tmpjsonl" ]; then cap_untrack "$scratch"; return 0; fi

  # OK4 - subagent sidecars: <session-file-stem>/ is omp's own on-disk
  # convention for a Task-tool delegation's per-agent JSONL (verified live:
  # ~/.omp/agent/sessions/<cwd>/<ts>_<sid>/<AgentName>.jsonl - no
  # subagents/ level, no agent- prefix, unlike Claude Code). raw_sid is read
  # back off the just-translated main transcript's own adapter-meta
  # sessionId rather than the bash $sid var above, since that is exactly
  # what `kb sessions capture` itself resolves as raw_sid (it prefers the
  # transcript's own sessionId over any --session-id hint - invariant #11),
  # so the two can never disagree even under a hook-mode --session-id
  # override.
  local sdir="${tpath%.jsonl}" rsid=""
  if [ -d "$sdir" ]; then
    rsid="$(head -1 "$tmpjsonl" | jq -r '.sessionId // empty' 2>/dev/null)"
    if [ -n "$rsid" ]; then
      local subdir_out="$scratch/$rsid/subagents"
      mkdir -p "$subdir_out" 2>/dev/null
      local f b safe subtmp srcrc
      for f in "$sdir"/*.jsonl; do
        [ -f "$f" ] || continue
        b="$(basename "$f" .jsonl)"
        safe="$(hook_agent_safe_name "$b" "$sdir")" || continue
        subtmp="$scratch/sub.clean.jsonl"
        # A sidecar that cannot be translated is dropped (the main transcript
        # and the healthy sidecars still land); only a deadline kill or a
        # TERM aborts the pass.
        cap_bg capt jq -R -c 'fromjson? // empty' "$f" >"$subtmp" 2>/dev/null
        srcrc=$?
        if [ "$srcrc" -ne 0 ]; then
          cap_fatal_rc "$srcrc" && { cap_untrack "$scratch"; return 1; }
          rm -f "$subtmp" "$subdir_out/agent-$safe.jsonl"; continue
        fi
        cap_bg capt jq -c -s --arg file "$f" "$TRANSLATE" "$subtmp" \
          >"$subdir_out/agent-$safe.jsonl" 2>/dev/null
        srcrc=$?
        if [ "$srcrc" -ne 0 ]; then
          cap_fatal_rc "$srcrc" && { cap_untrack "$scratch"; return 1; }
          rm -f "$subtmp" "$subdir_out/agent-$safe.jsonl"; continue
        fi
        rm -f "$subtmp"
        [ -s "$subdir_out/agent-$safe.jsonl" ] || rm -f "$subdir_out/agent-$safe.jsonl"
      done
    fi
  fi

  # Trailing authoritative-edited-set snapshot (parse_session_activity reads it).
  cap_bg capt jq -n -c --arg sid "$sid" --argjson edited "$edited" \
    'select(($edited | length) > 0) |
     {sessionId: $sid, type: "file-history-snapshot",
      snapshot: {trackedFileBackups: ($edited | map({key: ., value: {}}) | from_entries)}}' \
    >>"$tmpjsonl" 2>/dev/null || { cap_untrack "$scratch"; return 1; }

  # The kb calls get their own fresh budget: conversion has its own hard
  # deadline and must not eat the landing's.
  hook_deadline_init
  local rc=0
  cap_bg hook_adapter_land "$sid" "$tmpjsonl" "${cwd:-unknown}" "$cts" omp || rc=$?
  # rc 1 = parked in the spool: park the translated subagent sidecars with it
  # so the replay folds them in (v0.45 N10).
  if [ "$rc" -eq 1 ] && [ -n "${rsid:-}" ] && [ -d "$scratch/$rsid/subagents" ]; then
    hook_spool_put_sidecars "$rsid" "$scratch/$rsid/subagents" || true
  fi
  cap_untrack "$scratch"
  # Recorded ONLY for a real landing, and only for the state read BEFORE the
  # conversion (an append during it makes the next fingerprint differ).
  if [ "$rc" -eq 0 ] && [ -n "$base" ] && [ -n "$fp" ]; then
    printf '%s\n' "$fp" >"$base.done.$$" 2>/dev/null && mv -f "$base.done.$$" "$base.done" 2>/dev/null
  fi
  [ "$rc" -eq 0 ]
}

# capture_one <session.jsonl> [sid] [cwd] [force] - the lock + coalescing
# driver around capture_pass. force=1 (CLI backfill) waits for the lock and
# never skips on an unchanged fingerprint.
capture_one() {
  local tpath="$1" sid="${2:-}" cwd="${3:-}" force="${4:-0}"
  [ -f "$tpath" ] || return 0
  local ldir key base g cur first=1
  if command -v flock >/dev/null 2>&1 && ldir="$(hook_capture_lock_dir 2>/dev/null)" \
    && key="$(hook_capture_key "$tpath" 2>/dev/null)" \
    && mkdir -p "$ldir" 2>/dev/null && chmod 700 "$ldir" 2>/dev/null; then
    base="$ldir/$key"
  else
    SECONDS=0
    capture_pass "$tpath" "$sid" "$cwd" "$force" "" || true # no lock available: as before
    return 0
  fi
  cap_req_bump "$base"
  while :; do
    if ! { exec 9>"$base.lock"; } 2>/dev/null; then
      SECONDS=0
      capture_pass "$tpath" "$sid" "$cwd" "$force" "" || true
      return 0
    fi
    if [ "$first" = 1 ] && [ "$force" = 1 ]; then
      if ! cap_bg_lock flock -w "$CAP_LOCK_WAIT" 9; then
        exec 9>&-
        echo "kb-capture-omp.sh: capture of $tpath is busy - not captured" >&2
        return 0
      fi
    elif ! flock -n 9; then
      exec 9>&- # the owner will see the bumped request counter
      return 0
    fi
    first=0
    while :; do
      g="$(cap_req_read "$base")"
      SECONDS=0
      capture_pass "$tpath" "$sid" "$cwd" "$force" "$base" || true
      cur="$(cap_req_read "$base")"
      [ "$cur" = "$g" ] && break
    done
    exec 9>&-
    # A request that landed after the last check but before the release found
    # the lock held and returned: re-check AFTER releasing, then go again.
    cur="$(cap_req_read "$base")"
    [ "$cur" = "$g" ] && return 0
  done
}

trap cap_on_signal TERM INT HUP
trap cap_cleanup EXIT
[ "$#" -gt 0 ] || trap '' PIPE # a shutdown capture outlives omp's pipes

if [ "$#" -gt 0 ]; then
  # CLI mode (backfill): sid + cwd come from each file's own session header.
  for arg in "$@"; do
    capture_one "$arg" "" "" 1
  done
else
  input="$(cat)"
  tpath="$(printf '%s' "$input" | jq -r '.session_file // .transcript_path // empty' 2>/dev/null)"
  sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
  cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"
  [ -n "$tpath" ] && [ -f "$tpath" ] || exit 0
  capture_one "$tpath" "$sid" "$cwd" 0
fi
exit 0
