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
# replay never overwrites a fresher capture of the same session (the Rust
# writer drops a stale spooled snapshot; see README). Staged subagent sidecars
# are folded into the capture by the live `kb sessions capture` call and are
# parked beside the main transcript only when the landing fails.
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
# v0.46 SEG-PR2 - SEGMENTED capture (off by default, see the README): a session
# whose leaf chain outgrows the segment target (16 MiB of raw bytes) is not
# converted by ONE `jq -s` slurp of the whole file (O(session) CPU and RSS - a
# 500 MB session never finished inside the hard deadline) but as an ORDERED
# CHAIN of ordinary capture sessions planned by `kb sessions segment-plan`:
# part 1 under the bare session id, part k>=2 under `<id>-p<NN>`, each
# translated from only its own byte range of the file with the SAME TRANSLATE
# program (derived from it by three guarded substitutions, never a copy). A
# session whose plan has ONE part takes the unchanged single-capture path.
# Enabled by KB_CAPTURE_SEGMENTS=1 in the environment OR the flag file
# ${XDG_CONFIG_HOME:-~/.config}/kb/capture-segments (checked at every capture,
# so a RUNNING omp needs no restart); KB_CAPTURE_SEGMENTS=0 forces it off.
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
     ((.message // {}) | if type == "object" then . else {} end) as $m |
     (.timestamp // ($m.timestamp | ms2iso) // "unknown") as $ts |
     if $m.role == "user" then
       ([$m.content[]? | objects | select(.type == "text") | .text | if type == "object" or type == "array" then tojson else . end] | join("\n")) as $t |
       select($t != "") |
       {sessionId: $sid, timestamp: $ts, cwd: $cwd, type: "user",
        message: {role: "user", content: [{type: "text", text: $t}]}}
     elif $m.role == "assistant" then
       ([$m.content[]? | objects |
         if .type == "text" and ((.text // "") != "") then
           {type: "text", text: .text}
         elif .type == "thinking" and ((.thinking // "") != "") then
           {type: "thinking", thinking: .thinking}
         elif .type == "toolCall" then
           # A malformed record (non-string name, non-object arguments, a
           # non-string path) must not abort the translation: it degrades to a
           # tool_use that keeps what it can ({arguments: <raw>} for a non-object).
           ((.name // "tool") | if type == "string" then . else "tool" end | canon) as $tn |
           ((.arguments // {}) as $a0 |
            (if ($a0 | type) == "object" then $a0 else {arguments: $a0} end) as $a |
            if (($tn | ascii_downcase) == "write"
                or ($tn | ascii_downcase) == "edit") and (($a.path | type) == "string")
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
       (([$m.content[]? | objects | (.text // "") | if type == "object" or type == "array" then tojson else . end] | join("\n")) | .[0:2000]) as $o |
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
CAP_ORPHAN_WAIT="${KB_CAPTURE_ORPHAN_WAIT_SECS:-8}"
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
#   1. a tiny watchdog, in a session of its own (it survives a kill of ours),
#      polls the owner. When the owner is gone it reaps every process still
#      carrying this run's KB_CAPTURE_RUN marker (plus the owner's session),
#      then removes the run's scratch directory;
#   2. the watchdog INHERITS the per-session lock (fd 9; the conversion
#      children do not - cap_bg closes it for them). So the lock outlives a
#      SIGKILLed owner until the orphans are dead: no new owner can start, and
#      no orphan can publish after a fresher conversion, while it is held.
#      A request arriving in that window sees the recorded owner is dead and
#      waits (bounded) for the lock instead of dropping its request.
# The watchdog is per lock HOLD (capture_one stops it before releasing the
# lock); normal exits and trapped signals stop it themselves.
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
  sleep 0.5 9>&- & sp=$!
  wait "$sp"
done
[ -d "$run" ] || exit 0
# TERM the owner session at once (one ps), THEN find the marker carriers:
# ONE grep over every environ (no per-pid fork), so the scan is a single
# process whose cost scales with the process count but never delays the TERM.
pids="$(ps -s "$owner" -o pid= 2>/dev/null | tr -d " ")"
for p in $pids; do kill -TERM "$p" 2>/dev/null; done
more="$(grep -laz -x -F "KB_CAPTURE_RUN=$marker" /proc/[0-9]*/environ 2>/dev/null \
  | sed -n "s#^/proc/\\([0-9][0-9]*\\)/environ\$#\\1#p")"
for p in $more; do
  [ "$p" = "$$" ] && continue
  case " $pids " in *" $p "*) continue ;; esac
  pids="$pids $p"; kill -TERM "$p" 2>/dev/null
done
live() { # the subset of "$@" that is alive and not a zombie (ONE ps)
  [ "$#" -gt 0 ] || return 0
  ps -o pid=,stat= -p "$(IFS=,; echo "$*")" 2>/dev/null | awk '"'"'substr($2,1,1) != "Z" { print $1 }'"'"'
}
for i in $(seq 1 20); do
  sleep 0.1
  left="$(live $pids)"
  [ -z "$left" ] && break
done
for p in $left; do kill -KILL "$p" 2>/dev/null; done
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
    "$$" "$KB_CAPTURE_RUN" "$CAP_RUN" "$start" </dev/null >/dev/null 2>&1 &
  CAP_WD=$!
  return 0
}

# Stop this lock hold's watchdog (it holds the lock fd, so it must be GONE
# before the lock is released) and drop the run dir.
cap_wd_stop() {
  local i
  if [ -n "$CAP_WD" ]; then
    kill -TERM "$CAP_WD" 2>/dev/null
    for i in $(seq 1 40); do cap_alive "$CAP_WD" || break; sleep 0.05; done
    cap_alive "$CAP_WD" && kill -KILL "$CAP_WD" 2>/dev/null
    wait "$CAP_WD" 2>/dev/null
    CAP_WD=""
  fi
  [ -n "$CAP_RUN" ] && rm -rf "$CAP_RUN" 2>/dev/null
  CAP_RUN=""
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
  cap_wd_stop
  { exec 9>&-; } 2>/dev/null
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
    | capt jq -R -c 'fromjson? | objects | select(.type == "message")
             | (.message | objects | .content | arrays | .[] | objects)
             | select(.type == "toolCall")
             | select(((.name // "") | tostring | ascii_downcase) == "write"
                      or ((.name // "") | tostring | ascii_downcase) == "edit")
             | [(.arguments | objects | (.path // .file_path // empty) | strings)]' 2>/dev/null \
    | capt jq -s -c 'add // [] | unique'
  local st=("${PIPESTATUS[@]}") s
  # Only a deadline kill / TERM fails the pass. A data error in the edited-set
  # extraction loses the snapshot (callers fall back to []), never the capture.
  for s in "${st[@]}"; do cap_fatal_rc "$s" && return 124; done
  [ "${st[2]}" -eq 0 ] || return 1
  return 0
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
# The lock owner's identity (pid|start time), so a request that finds the lock
# held can tell a live owner (coalesce and return) from a SIGKILLed one whose
# watchdog is still reaping (wait for the lock, then serve the request).
cap_owner_write() { # <base>
  printf '%s|%s\n' "$$" "$(ps -o lstart= -p "$$" 2>/dev/null)" >"$1.owner" 2>/dev/null
  return 0
}
cap_owner_dead() { # <base>: rc 0 only when the recorded owner is provably gone
  local rec pid start st lst
  rec="$(cat "$1.owner" 2>/dev/null)"
  pid="${rec%%|*}"; start="${rec#*|}"
  case "$pid" in '' | *[!0-9]*) return 1 ;; esac
  st="$(ps -o stat= -p "$pid" 2>/dev/null | tr -d ' ')"
  [ -n "$st" ] && [ "${st#Z}" = "$st" ] || return 0
  lst="$(ps -o lstart= -p "$pid" 2>/dev/null)"
  [ "$lst" != "$start" ]
}
cap_req_read() { local n; n="$(cat "$1.req" 2>/dev/null)"; printf '%s' "${n:-0}"; }

# Stage ONE subagent sidecar: lenient pre-clean, then the SAME TRANSLATE
# program, into <out>. A sidecar that cannot be translated is dropped (the main
# transcript and the healthy sidecars still land); rc 1 only for a deadline
# kill / TERM, which aborts the pass.
cap_stage_sidecar() { # <src.jsonl> <out-file> <scratch clean file>
  local f="$1" out="$2" subtmp="$3" srcrc
  cap_bg capt jq -R -c 'fromjson? // empty' "$f" >"$subtmp" 2>/dev/null
  srcrc=$?
  if [ "$srcrc" -ne 0 ]; then
    cap_fatal_rc "$srcrc" && return 1
    rm -f "$subtmp" "$out"; return 0
  fi
  cap_bg capt jq -c -s --arg file "$f" "$TRANSLATE" "$subtmp" >"$out" 2>/dev/null
  srcrc=$?
  if [ "$srcrc" -ne 0 ]; then
    cap_fatal_rc "$srcrc" && return 1
    rm -f "$subtmp" "$out"; return 0
  fi
  rm -f "$subtmp"
  [ -s "$out" ] || rm -f "$out"
  return 0
}

# --- segmented capture (v0.46 SEG-PR2) ---------------------------------------
# One pass, under the per-session lock, over the plan `kb sessions segment-plan`
# computes for the session file:
#   * the plan is ONLY a set of cut points; what has been LANDED is this
#     script's own table (<lock-base>.seg: idx, part id, input key), committed
#     tmp+rename after every part that landed with rc 0, so a SIGKILL anywhere
#     loses at most the part in flight and the next pass resumes from the table.
#     (The planner's own divergence/orphan fields are not trusted for this: the
#     planner persists its new plan when it reports them, so a crash right after
#     would lose them. Comparing the plan against the table is crash-proof.)
#   * the LIVE TAIL (the last part) lands first, so the session is searchable at
#     once; then up to KB_CAPTURE_SEG_FREEZE_PER_PASS (4) FROZEN parts, oldest
#     first. A frozen part is converted once and landed once; its key is
#     (first id, last id, entry count, its sidecars), so it is revisited only
#     when the chain behind it changed or one of ITS subagent sidecars did.
#   * a part that converted but did not land (rc 1 = spooled, rc 2 = lost) keeps
#     its converted files under <lock-base>.parts/NN/ and the retry lands them
#     without converting again; landed is recorded only on rc 0.
#   * a rewind or /clear behind a frozen boundary re-lands the changed parts in
#     place (same ids) and drops the parts the chain no longer reaches through
#     `kb sessions drop-part` (the daemon's own delete cascade does the rest;
#     this script never removes a corpus file).
#   * more work than one pass may do sets CAP_MORE and capture_one runs another
#     pass; nothing is recorded as done until every part is landed.
CAP_MORE=""
CAP_SEG_SINGLE=""
CAP_SEG_PROBED=""
CAP_SEG_WARNED=""
CAP_SEG_P1=""
CAP_SEG_PN=""
CAP_SEG_VER="$CAP_VERSION+seg1"

# KB_CAPTURE_TRACE=<file>: one line per conversion / landing (the tests count
# them: a frozen part must be converted exactly once).
cap_trace() {
  [ -n "${KB_CAPTURE_TRACE:-}" ] && printf '%s\n' "$*" >>"$KB_CAPTURE_TRACE" 2>/dev/null
  return 0
}

cap_seg_flag() {
  case "${KB_CAPTURE_SEGMENTS:-}" in
    1 | on | true | yes) return 0 ;;
    0 | off | false | no) return 1 ;;
  esac
  [ -e "${XDG_CONFIG_HOME:-${HOME:-/nonexistent}/.config}/kb/capture-segments" ]
}

cap_seg_warn() {
  [ -n "$CAP_SEG_WARNED" ] && return 0
  CAP_SEG_WARNED=1
  echo "kb-capture-omp.sh: segmented capture is enabled but $* - using the single-capture path" >&2
  return 0
}

# The two per-part programs are DERIVED from TRANSLATE: three exact-text
# substitutions, each verified to apply, so a later edit of TRANSLATE that
# breaks one fails closed (single-capture path + one warning) instead of
# drifting.
#   $dmodel   -> the planner's per-part value (stable under appends)
#   $sid      -> the part id (parts k>=2 only; part 1 keeps the header id)
#   adapter-meta gains $segmeta ({segmentOf, segmentIdx, rawSessionId})
#   $exit     -> only the live tail emits the [session-exit] marker
cap_seg_build_programs() {
  local t="$TRANSLATE" a b c d ra rb rc rd
  shopt -u patsub_replacement 2>/dev/null
  a='(if ($hdr.id // "") != "" then $hdr.id else ($es[0].id // "unknown") end) as $sid |'
  b='($live | map(select(.type == "model_change") | .model) | last // "omp") as $dmodel |'
  c='+ (if $ttitle != "" then {aiTitle: $ttitle} else {} end)),'
  d='($live | map(select(.type == "custom" and .customType == "session_exit")) | last) as $exit |'
  # Legacy emits ONE [session-exit], for the session's LAST exit, at the very
  # end: only the live tail may emit it (falling back to the last exit found
  # in the earlier parts, $xexit), never a frozen part.
  rd='(if $emitexit then ((($live | map(select(.type == "custom" and .customType == "session_exit")) | last) // $xexit)) else null end) as $exit |'
  ra='($psid) as $sid |'
  rb='($pdmodel) as $dmodel |'
  rc='+ (if $ttitle != "" then {aiTitle: $ttitle} else {} end) + $segmeta),'
  case "$t" in *"$a"*) ;; *) return 1 ;; esac
  case "$t" in *"$b"*) ;; *) return 1 ;; esac
  case "$t" in *"$c"*) ;; *) return 1 ;; esac
  case "$t" in *"$d"*) ;; *) return 1 ;; esac
  t="${t/"$d"/$rd}"
  CAP_SEG_P1="${t/"$b"/$rb}"
  t="$CAP_SEG_P1"
  t="${t/"$a"/$ra}"
  t="${t/"$c"/$rc}"
  CAP_SEG_PN="$t"
  return 0
}

# Is segmented capture on AND usable? Probed once per run.
cap_seg_ready() {
  cap_seg_flag || return 1
  if [ -n "$CAP_SEG_PROBED" ]; then [ "$CAP_SEG_PROBED" = 1 ]; return; fi
  CAP_SEG_PROBED=0
  if ! declare -F hook_spool_count_group >/dev/null 2>&1 || ! declare -F hook_kb_in_path >/dev/null 2>&1; then
    cap_seg_warn "kb-hook-lib.sh lacks the segmented-capture helpers"; return 1
  fi
  hook_kb_in_path || { cap_seg_warn "kb is not on PATH"; return 1; }
  cap_bg capt kb sessions segment-plan --help >/dev/null 2>&1 \
    || { cap_seg_warn "this kb has no 'sessions segment-plan'"; return 1; }
  cap_seg_build_programs || { cap_seg_warn "the translator no longer matches the segment programs"; return 1; }
  CAP_SEG_PROBED=1
  return 0
}

# The landed table: idx <TAB> part id <TAB> key.
cap_tab_get() { awk -F'\t' -v i="$2" '$1 == i { print $3; exit }' "$1" 2>/dev/null; }
cap_tab_pid() { awk -F'\t' -v i="$2" '$1 == i { print $2; exit }' "$1" 2>/dev/null; }
cap_tab_idxs() { awk -F'\t' '{ print $1 }' "$1" 2>/dev/null; }
cap_tab_set() { # <file> <idx> <part id> <key>
  local tmp="$1.tmp.$$"
  { awk -F'\t' -v i="$2" '$1 != i' "$1" 2>/dev/null; printf '%s\t%s\t%s\n' "$2" "$3" "$4"; } \
    | LC_ALL=C sort -t "$(printf '\t')" -n -k1,1 >"$tmp" 2>/dev/null \
    && mv -f "$tmp" "$1" 2>/dev/null
}
cap_tab_del() { # <file> <idx>
  local tmp="$1.tmp.$$"
  awk -F'\t' -v i="$2" '$1 != i' "$1" >"$tmp" 2>/dev/null && mv -f "$tmp" "$1" 2>/dev/null
  [ -s "$1" ] || rm -f "$1"
  return 0
}

# Raw-byte target per part: halved (and persisted) when a translated part came
# out over the size cap; else KB_CAPTURE_SEGMENT_BYTES; else 16 MiB.
cap_seg_target() {
  local t
  t="$(cat "$1.target" 2>/dev/null)"
  case "$t" in '' | *[!0-9]*) t="${KB_CAPTURE_SEGMENT_BYTES:-}" ;; esac
  case "$t" in '' | *[!0-9]* | 0) t=16777216 ;; esac
  printf '%s' "$t"
}

# ISO timestamp of the source line starting at byte offset $2 of $1 (empty when
# it carries none). Reads one line, never the file.
cap_iso_at() {
  capt tail -c +"$(($2 + 1))" -- "$1" 2>/dev/null | capt head -n 1 \
    | capt jq -r '(.timestamp // (.message.timestamp | if type == "number" then (. / 1000 | todate) else empty end)) // empty' 2>/dev/null
}
# Same, run as a tracked background job - NOT inside $(...), which would defer
# a TERM behind a cold read; the answer is left in CAP_ISO.
CAP_ISO=""
cap_iso_bg() {
  CAP_ISO=""
  if [ -z "${CAP_RUN:-}" ]; then CAP_ISO="$(cap_iso_at "$@")"; return 0; fi
  cap_bg cap_iso_at "$@" >"$CAP_RUN/iso.out"
  read -r CAP_ISO <"$CAP_RUN/iso.out" 2>/dev/null || true
  rm -f "$CAP_RUN/iso.out"
  return 0
}
cap_epoch_of() { [ -n "$1" ] && date -u -d "$1" +%s 2>/dev/null; }

# A part id for index $2 of raw id $1: part 1 is the bare id.
cap_part_id() { if [ "$2" -le 1 ]; then printf '%s' "$1"; else printf '%s-p%02d' "$1" "$2"; fi; }
# The raw id a part id belongs to (strips one trailing -p<digits>).
cap_part_raw() { if [[ "$1" =~ ^(.+)-p[0-9]+$ ]]; then printf '%s' "${BASH_REMATCH[1]}"; else printf '%s' "$1"; fi; }

# The session_exit candidates in bytes [$2, $3) of $1, one compact JSON object
# per line. Every external stage is capped by the pass deadline (capt) and the
# pipeline runs under cap_bg, so a TERM is not deferred behind a cold read.
cap_exit_scan() { # <src> <from> <to>
  capt tail -c +"$(($2 + 1))" -- "$1" 2>/dev/null | capt head -c "$(($3 - $2))" \
    | capt grep -a -E '"customType": ?"session_exit"' \
    | capt jq -c 'select(.type == "custom" and .customType == "session_exit" and (.id | type) == "string")' 2>/dev/null
}

# The session_exit of the RESOLVED chain that legacy would carry, among the
# bytes BEFORE offset $2 of $1 (the live tail's start), as one JSON line (empty
# when none). Legacy reads only $live - the leaf chain after the last
# reset_boundary - so a raw `last exit in the file` is wrong for an exit on an
# abandoned branch or at/before a /clear. Candidates (every exit seen, newest
# 64) are cached incrementally in `<base>.exits` (scanned offset, then one
# candidate per line; a shrunk source rescans from 0); chain membership and
# order come from the planner (`--print-chain`, only when a candidate exists).
# Needs $plan/$target/$tpath from the caller. rc 0 ok, 1 failed (the part must
# not be converted with a guessed marker).
cap_seg_last_exit() { # <tpath> <upto> <base>
  local src="$1" upto="$2" cf="$3.exits" off=0 cached
  local cand="$CAP_RUN/exit.cand" new="$CAP_RUN/exit.new" chain="$CAP_RUN/exit.chain" want last src_rc
  : >"$cand"
  cached="$(head -n 1 "$cf" 2>/dev/null)"
  case "$cached" in '' | *[!0-9]*) ;; *) off="$cached"; tail -n +2 "$cf" >"$cand" 2>/dev/null ;; esac
  if [ "$off" -gt "$upto" ]; then off=0; : >"$cand"; fi
  if [ "$off" -lt "$upto" ]; then
    cap_bg cap_exit_scan "$src" "$off" "$upto" >"$new"
    local sts=$?
    cap_fatal_rc "$sts" && return 1
    cat "$new" >>"$cand" 2>/dev/null
    rm -f "$new"
    tail -n 64 "$cand" >"$cand.t" 2>/dev/null && mv -f "$cand.t" "$cand"
    { printf '%s\n' "$upto"; cat "$cand"; } >"$cf.$$" 2>/dev/null && mv -f "$cf.$$" "$cf" 2>/dev/null
  fi
  [ -s "$cand" ] || return 0
  cap_bg capt kb sessions segment-plan --source "$src" --state "$plan" --target-bytes "$target" \
    --adapter-ver "$CAP_SEG_VER" --print-chain --no-write >"$chain" 2>/dev/null
  src_rc=$?
  if [ "$src_rc" -ne 0 ]; then rm -f "$chain" "$cand"; return 1; fi
  want="$CAP_RUN/exit.want"
  jq -r '.id' "$cand" >"$want" 2>/dev/null
  last="$(jq -r '.chain_ids[]?' "$chain" 2>/dev/null \
    | awk 'NR == FNR { w[$0] = 1; next } ($0 in w) { l = $0 } END { print l }' "$want" -)"
  rm -f "$chain" "$want"
  [ -n "$last" ] || { rm -f "$cand"; return 0; }
  jq -c --arg id "$last" 'select(.id == $id)' "$cand" 2>/dev/null | tail -n 1
  rm -f "$cand"
  return 0
}

# Room in the private spool for one more parked part of this session?
cap_seg_spool_room() { # <raw id> <part id>
  local dir cap="${KB_CAPTURE_SEG_SPOOL_MAX:-8}"
  dir="$(hook_spool_dir)" || return 0
  [ -f "$dir/$(hook_spool_key "$2").jsonl" ] && return 0 # already parked: overwrites in place
  [ "$(hook_spool_count_group "$1")" -lt "$cap" ]
}

# Convert part $1 into <lock-base>.parts/NN/ (transcript.jsonl, the part's own
# sidecars under <part id>/subagents/, and `key` written LAST - a directory
# without it is a torn conversion and is redone). Arrays/vars are the caller's
# (bash dynamic scope). rc 0 ok, 1 failed/fatal, 5 translated part over the
# size cap (the target was halved, the pass must re-plan).
cap_seg_convert() {
  local k="$1" pid="${PIDS[$1]}" d raw tmp trc erc edited snapsid dm segj
  d="${pdir:?}/$(printf '%02d' "$k")"
  raw="$CAP_RUN/part.raw"
  tmp="$d/transcript.jsonl.tmp"
  rm -rf -- "${d:?}"
  mkdir -p "$d" 2>/dev/null || return 1
  cap_bg capt kb sessions segment-plan --source "$tpath" --state "$plan" --target-bytes "$target" \
    --adapter-ver "$CAP_SEG_VER" --emit "$k" --no-write >"$raw" 2>/dev/null
  trc=$?
  if [ "$trc" -ne 0 ] || [ ! -s "$raw" ]; then rm -rf -- "${d:?}"; rm -f "$raw"; return 1; fi
  cap_bg cap_edited_set "$raw" >"$CAP_RUN/edited.json"
  erc=$?
  if cap_fatal_rc "$erc"; then rm -rf -- "${d:?}"; rm -f "$raw"; return 1; fi
  edited=""
  [ "$erc" -eq 0 ] && edited="$(cat "$CAP_RUN/edited.json" 2>/dev/null)"
  [ -n "$edited" ] || edited='[]'
  dm="${DMS[$k]}"
  local emitexit=false xexit=null
  if [ "$k" -eq "$n" ]; then
    emitexit=true
    # A tracked background job with its answer in a file - NEVER $(...): bash
    # defers a trapped TERM until a command substitution returns, and the exit
    # scan + planner chain walk can be slow on a cold multi-GB source.
    cap_bg cap_seg_last_exit "$tpath" "${STARTS[$n]}" "$base" >"$CAP_RUN/xexit.out"
    erc=$?
    if [ "$erc" -ne 0 ]; then rm -rf -- "${d:?}"; rm -f "$raw" "$CAP_RUN/xexit.out"; return 1; fi
    xexit="$(cat "$CAP_RUN/xexit.out" 2>/dev/null)"
    rm -f "$CAP_RUN/xexit.out"
    [ -n "$xexit" ] || xexit=null
  fi
  cap_trace "convert part=$k state=${STATES[$k]}"
  if [ "$k" -le 1 ]; then
    snapsid="$sid"
    cap_bg capt jq -c -s --arg file "$tpath" --argjson pdmodel "$dm" \
      --argjson emitexit "$emitexit" --argjson xexit "$xexit" "$CAP_SEG_P1" "$raw" >"$tmp" 2>/dev/null
  else
    snapsid="$pid"
    segj="$(jq -n -c --arg of "$rawsid" --argjson idx "$k" '{segmentOf: $of, segmentIdx: $idx, rawSessionId: $of}')"
    cap_bg capt jq -c -s --arg file "$tpath" --arg psid "$pid" --argjson pdmodel "$dm" \
      --argjson segmeta "$segj" \
      --argjson emitexit "$emitexit" --argjson xexit "$xexit" "$CAP_SEG_PN" "$raw" >"$tmp" 2>/dev/null
  fi
  trc=$?
  rm -f "$raw"
  if cap_fatal_rc "$trc" || [ ! -s "$tmp" ]; then rm -rf -- "${d:?}"; return 1; fi
  # Subagent sidecars assigned to THIS part.
  if [ -n "${SCF[$k]:-}" ]; then
    local f b safe so="$d/$pid/subagents"
    mkdir -p "$so" 2>/dev/null
    while IFS= read -r f; do
      [ -f "$f" ] || continue
      b="$(basename "$f" .jsonl)"
      safe="$(hook_agent_safe_name "$b" "$sdir")" || continue
      cap_stage_sidecar "$f" "$so/agent-$safe.jsonl" "$CAP_RUN/sub.clean.jsonl" \
        || { rm -rf -- "${d:?}"; return 1; }
    done <<<"${SCF[$k]}"
  fi
  cap_bg capt jq -n -c --arg sid "$snapsid" --argjson edited "$edited" \
    'select(($edited | length) > 0) |
     {sessionId: $sid, type: "file-history-snapshot",
      snapshot: {trackedFileBackups: ($edited | map({key: ., value: {}}) | from_entries)}}' \
    >>"$tmp" 2>/dev/null
  if cap_fatal_rc "$?"; then rm -rf -- "${d:?}"; return 1; fi
  mv -f "$tmp" "$d/transcript.jsonl"
  # Size safety: a translated part over the cap is never truncated and never
  # landed oversized - the raw target halves (per session: the planner has one
  # target) and the next pass re-plans.
  local sz maxp="${KB_CAPTURE_SEG_MAX_PART_BYTES:-41943040}" nt
  sz="$(stat -c %s -- "$d/transcript.jsonl" 2>/dev/null || echo 0)"
  if [ "$sz" -gt "$maxp" ]; then
    rm -rf -- "${d:?}"
    nt=$((target / 2))
    if [ "$nt" -lt "${KB_CAPTURE_SEG_MIN_TARGET:-65536}" ]; then
      echo "kb-capture-omp.sh: part $k of $rawsid translates to $sz bytes and the target cannot shrink further - not captured" >&2
      return 1
    fi
    printf '%s\n' "$nt" >"$base.target" 2>/dev/null
    cap_trace "halve target=$nt"
    CAP_MORE=1
    return 5
  fi
  printf '%s\n' "${KEYS[$k]}" >"$d/key"
  return 0
}

# Convert (unless a cached conversion for this exact key exists) and land part
# $1. rc 0 landed (recorded), 1 failed - the converted files stay for the
# retry, 3 no spool room, 4 out of time, 5 target halved.
cap_seg_do_part() {
  local k="$1" pid="${PIDS[$1]}" d stamp iso rc=0 crc
  d="${pdir:?}/$(printf '%02d' "$k")"
  if [ $((CAP_HARD - SECONDS)) -le 15 ]; then
    echo "kb-capture-omp.sh: under 15 s of the pass deadline left - part $k of ${rawsid:-the session} waits for the next pass" >&2
    return 4
  fi
  if [ "$(cat "$d/key" 2>/dev/null)" != "${KEYS[$k]}" ] || [ ! -s "$d/transcript.jsonl" ]; then
    cap_seg_convert "$k"
    crc=$?
    [ "$crc" -eq 0 ] || return "$crc"
  fi
  if ! cap_seg_spool_room "$rawsid" "$pid"; then
    echo "kb-capture-omp.sh: ${KB_CAPTURE_SEG_SPOOL_MAX:-8} parts of $rawsid are already parked in the spool - not landing part $k yet" >&2
    return 3
  fi
  if [ "$k" -le 1 ]; then
    stamp="$cts"
  else
    cap_iso_bg "$tpath" "${STARTS[$k]}"
    iso="$CAP_ISO"
    stamp="$(date -u -d "$iso" +%Y%m%dT%H%M%SZ 2>/dev/null)"
    [ -n "$stamp" ] || stamp="$cts"
  fi
  # Write-ahead: from here on the part may reach the corpus WITHOUT a landed
  # row (parked in the spool and published by ANOTHER session's replay, or
  # kill -9 between the landing and the row). `<base>.seg.pend` remembers it so
  # a later shrink/fork still finds it; the landed row replaces it on success.
  [ "$k" -le 1 ] || cap_tab_set "$pend" "$k" "$pid" "${KEYS[$k]}"
  hook_deadline_init
  cap_bg hook_adapter_land "$pid" "$d/transcript.jsonl" "${cwd:-unknown}" "$stamp" omp || rc=$?
  cap_trace "land part=$k rc=$rc"
  if [ "$rc" -eq 1 ] && [ -d "$d/$pid/subagents" ]; then
    hook_spool_put_sidecars "$pid" "$d/$pid/subagents" || true
  fi
  # A landing that reports success but leaves no capture file is a failure
  # (kb missing, an engine that wrote elsewhere): never recorded, and it must
  # not keep the coalescing loop re-landing the same parts.
  if [ "$rc" -eq 0 ] && ! cap_capture_exists "$pid"; then
    echo "kb-capture-omp.sh: landing part $k of $rawsid reported success but no capture file exists - not recorded" >&2
    rc=1
  fi
  if [ "$rc" -eq 0 ]; then
    cap_tab_set "$tab" "$k" "$pid" "${KEYS[$k]}"
    cap_tab_del "$pend" "$k"
    rm -rf -- "${d:?}"
    return 0
  fi
  return 1
}

# Forget every LOCAL trace of continuation parts beyond index $1: spool items
# (a part parked after a failed landing has NO table row, and the next landing
# of any session replays the spool - a stale part would be published as a
# ghost) and the converted-part caches. Needs $tab/$pdir from the caller.
cap_seg_purge_local() { # <keep idx count> [raw id]
  local keep="$1" raw="${2:-${rawsid:-${sid:-}}}" id k d
  [ -n "$raw" ] || return 0
  if declare -F hook_spool_group_ids >/dev/null 2>&1; then
    while IFS= read -r id; do
      [ -n "$id" ] && [ "$id" != "$raw" ] || continue
      [[ "$id" =~ ^.+-p([0-9]+)$ ]] || continue
      k=$((10#${BASH_REMATCH[1]}))
      if [ "$k" -gt "$keep" ]; then hook_spool_drop "$id"; cap_trace "purge spool part=$k"; fi
    done < <(hook_spool_group_ids "$raw")
  fi
  for d in "${pdir:?}"/[0-9][0-9]*; do
    [ -d "$d" ] || continue
    k=$((10#$(basename "$d")))
    [ "$k" -gt "$keep" ] && rm -rf -- "${d:?}"
  done
  return 0
}

# Drop the continuation parts the table holds beyond index $1 (the new part
# count; 1 = every continuation part) through `kb sessions drop-part`. A row is
# removed only when kb confirmed. rc 0 = nothing left; rc 1 = a drop is
# outstanding, so the caller must NOT record the input as done (the next
# unchanged trigger has to come back and retry it).
cap_seg_drop_orphans() { # <keep idx count>
  local keep="$1" i pid raw left=0 t
  cap_seg_purge_local "$keep"
  # The landed table AND the write-ahead one (parts that may have reached the
  # corpus without a landed row).
  for t in "$tab" "$pend"; do
    for i in $(cap_tab_idxs "$t"); do
      [ "$i" -gt "$keep" ] || continue
      pid="$(cap_tab_pid "$t" "$i")"
      raw="$(cap_part_raw "$pid")"
      if [ "$i" -le 1 ] || [ "$raw" = "$pid" ]; then cap_tab_del "$t" "$i"; continue; fi
      if cap_bg capt kb sessions drop-part --help >/dev/null 2>&1; then
        if cap_bg capt kb sessions drop-part --session-id "$pid" --segment-of "$raw" --out "$KB_SESSIONS_DIR" >/dev/null 2>&1; then
          cap_trace "drop part=$i"
          hook_spool_drop "$pid"
          rm -rf -- "${pdir:?}/$(printf '%02d' "$i")"
          cap_tab_del "$t" "$i"
        else
          left=1
        fi
      else
        cap_seg_warn "this kb has no 'sessions drop-part', so superseded parts stay in the corpus"
        left=1
      fi
    done
  done
  [ "$left" -eq 0 ]
}

# The session now fits in ONE part again (a /clear): after the single-capture
# path re-landed the bare id in place, drop every stale continuation part.
cap_seg_reset() { # <lock-base>
  local tab="$1.seg" pend="$1.seg.pend" pdir="$1.parts"
  local drc=0
  cap_seg_drop_orphans 1 || drc=1
  cap_tab_del "$tab" 1
  return "$drc"
}

# ONE segmented pass. rc 0 = done or progressed (CAP_MORE says another pass is
# due), 1 = nothing recordable, 9 = not segmented (<= 1 part: the caller runs
# the single-capture path).
cap_seg_pass() { # <tpath> <sid> <cwd> <base> <cts> <fp>
  local tpath="$1" sid="$2" cwd="$3" base="$4" cts="$5" fp="$6"
  [ -n "$base" ] || return 9
  cap_run_init
  [ -n "$CAP_RUN" ] || return 9
  local plan="$base.plan" tab="$base.seg" pend="$base.seg.pend" pdir="$base.parts" planj="$CAP_RUN/plan.json"
  local target prc n rawsid title i tkey
  target="$(cap_seg_target "$base")"
  cap_bg capt kb sessions segment-plan --source "$tpath" --state "$plan" --target-bytes "$target" \
    --adapter-ver "$CAP_SEG_VER" >"$planj" 2>"$CAP_RUN/plan.err"
  prc=$?
  cap_fatal_rc "$prc" && return 1
  if [ "$prc" -ne 0 ] || [ "$(jq -r '.schema // empty' "$planj" 2>/dev/null)" != "segment-plan/1" ]; then
    echo "kb-capture-omp.sh: segment-plan failed (rc $prc): $(head -c 300 "$CAP_RUN/plan.err" 2>/dev/null)" >&2
    local size
    size="$(stat -c %s -- "$tpath" 2>/dev/null)"
    # Small enough for the single-capture path anyway: fall back to it.
    if [ -n "$size" ] && [ "$size" -le "$target" ]; then return 9; fi
    return 1
  fi
  n="$(jq -r '.parts | length' "$planj" 2>/dev/null)"
  case "$n" in '' | *[!0-9]*) return 1 ;; esac
  if [ "$n" -le 1 ]; then CAP_SEG_SINGLE=1; return 9; fi
  rawsid="$(jq -r '.session_id // empty' "$planj")"
  [ -n "$rawsid" ] || rawsid="$sid"
  title="$(jq -r '.title // ""' "$planj")"
  tkey="$(printf '%s' "$title" | cksum | cut -d' ' -f1)"

  local -a PIDS=() KEYS=() STATES=() STARTS=() DMS=() SCF=() F=()
  mapfile -t F < <(jq -r '.parts[] | ((.idx | tostring), .state, (.first_id | tojson), (.last_id | tojson),
      (.n_entries | tostring), (.start_offset | tostring), (.dmodel | tojson))' "$planj")
  [ "${#F[@]}" -eq $((n * 7)) ] || return 1
  local o
  for ((i = 1; i <= n; i++)); do
    o=$(((i - 1) * 7))
    PIDS[$i]="$(cap_part_id "$rawsid" "$i")"
    STATES[$i]="${F[$((o + 1))]}"
    STARTS[$i]="${F[$((o + 5))]}"
    DMS[$i]="${F[$((o + 6))]}"
    KEYS[$i]="${F[$((o + 1))]}|${F[$((o + 2))]}|${F[$((o + 3))]}|${F[$((o + 4))]}"
    [ "${STATES[$i]}" = live ] && KEYS[$i]="${KEYS[$i]}|t:$tkey"
  done

  # Subagent sidecars go to the part whose time range holds their FIRST
  # timestamp: the last part that starts strictly before it (a tie goes to the
  # earlier part); no readable timestamp -> the live tail.
  local sdir="${tpath%.jsonl}" f s j
  local -a EP=()
  if [ -d "$sdir" ] && compgen -G "$sdir/*.jsonl" >/dev/null 2>&1; then
    for ((j = 2; j <= n; j++)); do
      cap_iso_bg "$tpath" "${STARTS[$j]}"
      EP[$j]="$(cap_epoch_of "$CAP_ISO")"
    done
    for f in "$sdir"/*.jsonl; do
      [ -f "$f" ] || continue
      s="$(cap_epoch_of "$(head -n 1 -- "$f" 2>/dev/null | jq -r '.timestamp // empty' 2>/dev/null)")"
      i=1
      if [ -n "$s" ]; then
        for ((j = 2; j <= n; j++)); do
          if [ -n "${EP[$j]:-}" ] && [ "${EP[$j]}" -lt "$s" ]; then i=$j; fi
        done
      else
        i=$n
      fi
      SCF[$i]+="$f"$'\n'
      KEYS[$i]="${KEYS[$i]}|s:$(stat -c '%s %.9Y %i' -- "$f" 2>/dev/null | cksum | cut -d' ' -f1)-${#f}"
    done
  fi

  mkdir -p "$pdir" 2>/dev/null && chmod 700 "$pdir" 2>/dev/null
  # A chain that shrank (rewind, /clear): a parked part beyond the new count
  # must never be replayed, whatever else happens in this pass.
  cap_seg_purge_local "$n" "$rawsid"
  rm -f "$base".seg.tmp.* "$base".seg.pend.tmp.* "$base".done.[0-9]* "$base".exits.[0-9]* "$base".exit.[0-9]* "$plan".tmp* 2>/dev/null
  local timedout="" landed=0 failed=0 stop="" per="${KB_CAPTURE_SEG_FREEZE_PER_PASS:-4}" frozen_done=0 rc
  local -a NEED=()
  for ((i = n; i >= 1; i--)); do
    if [ "$(cap_tab_get "$tab" "$i")" != "${KEYS[$i]}" ] || ! cap_capture_exists "${PIDS[$i]}"; then
      NEED[$i]=1
    fi
  done
  # 1. the live tail first, 2. frozen parts oldest first.
  if [ -n "${NEED[$n]:-}" ]; then
    cap_seg_do_part "$n"
    rc=$?
    case "$rc" in
      0) landed=$((landed + 1)) ;;
      5) stop=1 ;;
      *) failed=1 ;;
    esac
  fi
  for ((i = 1; i < n; i++)); do
    [ "$failed" -eq 0 ] && [ -z "$stop" ] || break
    [ -n "${NEED[$i]:-}" ] || continue
    if [ "$frozen_done" -ge "$per" ]; then CAP_MORE=1; break; fi
    cap_seg_do_part "$i"
    rc=$?
    case "$rc" in
      0) landed=$((landed + 1)); frozen_done=$((frozen_done + 1)) ;;
      4) timedout=1; stop=1 ;; # out of time: progress is on disk, go again (only if something landed)
      5) stop=1 ;;
      *) failed=1 ;;
    esac
  done
  [ -n "$timedout" ] && [ "$landed" -gt 0 ] && CAP_MORE=1
  # Another pass only after progress (a landing, or a re-plan after halving):
  # a failing kb must never spin the coalescing loop.
  if [ "$failed" -ne 0 ] || { [ "$landed" -eq 0 ] && [ -z "$stop" ]; }; then CAP_MORE=""; fi
  [ "$failed" -eq 0 ] || return 1
  [ -z "$CAP_MORE" ] || return 0
  [ -z "$stop" ] || return 1
  # Everything on the plan is landed: drop what the plan no longer reaches.
  local drops=0
  cap_seg_drop_orphans "$n" || drops=1
  rmdir "$pdir" 2>/dev/null
  if [ -n "$fp" ] && [ "$drops" -eq 0 ]; then
    printf '%s\n' "$fp" >"$base.done.$$" 2>/dev/null && mv -f "$base.done.$$" "$base.done" 2>/dev/null
  fi
  return 0
}

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

  # v0.46 SEG-PR2: segmented capture (flag + usable kb only). rc 9 = this
  # session is one part: fall through to the unchanged single-capture path.
  CAP_SEG_SINGLE=""
  if [ -n "$base" ] && cap_seg_ready; then
    local sprc
    cap_seg_pass "$tpath" "$sid" "$cwd" "$base" "$cts" "$fp"
    sprc=$?
    [ "$sprc" -eq 9 ] || return "$sprc"
  fi

  # OK4 - a scratch DIR (not a bare file), so a sibling subagent dir can be
  # staged at <scratch>/<raw-session-id>/subagents/ beside the main
  # translated transcript - exactly the shape sessions_capture.rs's sidecar
  # walk resolves (transcript.parent().join(&raw_sid).join("subagents")).
  local scratch tmpclean tmpjsonl edited erc trc srcrc
  cap_run_init
  scratch="$(mktemp -d "${CAP_RUN:-${TMPDIR:-/tmp}}/p.XXXXXX")" || return 1
  cap_track "$scratch"
  tmpclean="$scratch/clean.jsonl"
  tmpjsonl="$scratch/transcript.jsonl"

  cap_bg cap_edited_set "$tpath" >"$scratch/edited.json"
  erc=$?
  cap_fatal_rc "$erc" && { cap_untrack "$scratch"; return 1; }
  edited=""
  [ "$erc" -eq 0 ] && edited="$(cat "$scratch/edited.json" 2>/dev/null)"
  [ -n "$edited" ] || edited='[]'

  # Lenient pre-clean (same policy as omp's own loader): drop unparsable
  # lines instead of aborting - a torn trailing line from a crash or an
  # active append must never kill the whole capture.
  if ! cap_bg capt jq -R -c 'fromjson? // empty' "$tpath" >"$tmpclean" 2>/dev/null; then
    cap_untrack "$scratch"; return 1
  fi
  # jq -s streams its output, so a data error part-way through leaves every
  # record translated before it in $tmpjsonl: that partial transcript is landed
  # (as the former script did) rather than the whole session being lost and
  # retried forever. Only a deadline kill / TERM aborts the pass.
  cap_bg capt jq -c -s --arg file "$tpath" "$TRANSLATE" "$tmpclean" >"$tmpjsonl" 2>/dev/null
  trc=$?
  if cap_fatal_rc "$trc"; then cap_untrack "$scratch"; return 1; fi
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
      local f b safe subtmp
      for f in "$sdir"/*.jsonl; do
        [ -f "$f" ] || continue
        b="$(basename "$f" .jsonl)"
        safe="$(hook_agent_safe_name "$b" "$sdir")" || continue
        subtmp="$scratch/sub.clean.jsonl"
        # A sidecar that cannot be translated is dropped (the main transcript
        # and the healthy sidecars still land); only a deadline kill or a
        # TERM aborts the pass.
        cap_stage_sidecar "$f" "$subdir_out/agent-$safe.jsonl" "$subtmp" \
          || { cap_untrack "$scratch"; return 1; }
      done
    fi
  fi

  # Trailing authoritative-edited-set snapshot (parse_session_activity reads it).
  cap_bg capt jq -n -c --arg sid "$sid" --argjson edited "$edited" \
    'select(($edited | length) > 0) |
     {sessionId: $sid, type: "file-history-snapshot",
      snapshot: {trackedFileBackups: ($edited | map({key: ., value: {}}) | from_entries)}}' \
    >>"$tmpjsonl" 2>/dev/null
  srcrc=$?
  # The snapshot is optional: a failure other than a deadline kill / TERM only
  # drops it (jq emits the line whole, so nothing partial is appended).
  if cap_fatal_rc "$srcrc"; then cap_untrack "$scratch"; return 1; fi

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
  # (the record itself is written below, once no drop-part is outstanding)
  local drops=0
  # The session fits in one part again (a /clear after segmenting): the bare id
  # was just re-landed in place, so the continuation parts are stale.
  if [ "$rc" -eq 0 ] && [ -n "$CAP_SEG_SINGLE" ] && [ -n "$base" ]; then
    cap_seg_reset "$base" || drops=1
  elif [ "$rc" -eq 0 ] && [ -n "$base" ] && [ -s "$base.seg" ]; then
    # The legacy path (segmentation off, or unusable) just landed the WHOLE
    # session over part 1: part 1 is no longer what the table says. Forget its
    # row so re-enabling re-lands it small (the -pNN parts stay as landed).
    cap_tab_del "$base.seg" 1
  fi
  # Not recorded while a drop is outstanding: the fingerprint shortcut would
  # otherwise skip every later unchanged trigger and the orphan part would stay
  # in the corpus for good.
  if [ "$rc" -eq 0 ] && [ -n "$base" ] && [ -n "$fp" ] && [ "$drops" -eq 0 ]; then
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
      # A live owner will see the bumped request counter: return. A dead one
      # (SIGKILLed; its watchdog still holds the lock while it reaps the
      # orphans) will not: wait for the lock and serve the request ourselves.
      if cap_owner_dead "$base" && cap_bg_lock flock -w "$CAP_ORPHAN_WAIT" 9; then
        :
      else
        exec 9>&-
        return 0
      fi
    fi
    cap_owner_write "$base"
    first=0
    while :; do
      g="$(cap_req_read "$base")"
      SECONDS=0
      CAP_MORE=""
      capture_pass "$tpath" "$sid" "$cwd" "$force" "$base" || true
      # A segmented catch-up that has more parts to land runs another pass
      # (progress is on disk, so a TERM/deadline kill resumes, never restarts).
      [ -n "$CAP_MORE" ] && continue
      cur="$(cap_req_read "$base")"
      [ "$cur" = "$g" ] && break
    done
    cap_wd_stop # the watchdog holds the lock fd too: it must be gone first
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
