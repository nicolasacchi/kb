#!/usr/bin/env bash
# kb-capture-grok — capture adapter: Grok Build (via grokclaude) session
# transcripts → kb session-capture HTML (Claude-Code-shaped JSONL inside a
# <pre>). Sibling of kb-capture-codex.sh / kb-capture-opencode.sh, but the
# PREFERRED write path (W5/R8) is the SAME shared writer kb-capture.sh
# itself prefers: `kb sessions capture --transcript <synthesized.jsonl>
# --session-id <grok-uuid> [--cwd <cwd>] --out $KB_SESSIONS_DIR` — the Rust
# engine, which gets us atomic writes, one-file-per-sid reuse-on-recapture,
# git commit resolution against the job's cwd, and `kb-decay: fast` for
# free (the "shared writer" the milestone brief calls out). A bash
# hand-rolled-HTML fallback (mirroring kb-capture.sh's own dual-path
# design) runs when `kb` is absent from PATH or the capture call fails for
# any reason — a translation bug in the jq program must never leave a real
# grok session uncaptured.
#
# Grok's own message-by-message transcript does NOT live in the grokclaude
# blackboard (.grokclaude/jobs/<ulid>/) — it lives entirely in Grok Build's
# OWN session store, `~/.grok/sessions/<url-encoded-cwd>/<grok-session-uuid>/`
# (undocumented, reverse-engineered; see maps/grok.md). The join key from a
# grokclaude job to that store is `meta.json.grok_session_id`.
#
# Modes:
#   <job-dir>                    post-run mode (the grokclaude trigger's own
#                                 call shape): resolve meta.json →
#                                 grok_session_id + cwd/worktree → locate the
#                                 grok session dir → capture it.
#   --session-dir <dir>          direct mode: capture a grok session dir
#                                 without a grokclaude job wrapper (also the
#                                 path a checked-in test fixture drives).
#   --backfill [--root <dir>]    walk every job under a grokclaude blackboard
#                                 root (default: nearest .grokclaude/ under
#                                 $PWD, else $GROKCLAUDE_ROOT) and capture
#                                 each resolvable non-fake job.
# Flags (compose with any mode):
#   --dry-run                    print what WOULD be captured; write nothing.
#   --with-report                 also render report.md + findings/*.json as
#                                 a linked kb artifact
#                                 (grok-report-<job-ulid>.html, D5).
#
# Env:
#   KB_SESSIONS_DIR    required — same gate as every kb-memory capture hook.
#   GROK_SESSIONS_ROOT override for ~/.grok/sessions (tests, alt installs).
#   GROKCLAUDE_FAKE    when truthy, skip unconditionally — the SAME env var
#                       grokclaude itself uses to mark a test run (a live
#                       post-run trigger inherits this from the grokclaude
#                       process env; defense in depth even though the
#                       Rust-side trigger is ALSO gated on it, item B).
#
# Grok chat_history.jsonl → Claude-shape mapping (live schema, verified
# against real ~/.grok/sessions/*/chat_history.jsonl — see
# ~/.claude/plans/sessions-rethink-workpapers/{designs/cli-grok.md P4,
# maps/grok.md} for the full recon this mirrors):
#
#   Grok line                              → Claude-shaped output
#   -----------------------------------    -----------------------------------
#   {type:"system", content:"<huge>"}      one isMeta user marker noting size;
#                                           body dropped (recoverable at the
#                                           source path in adapter-meta)
#   {type:"user", content:[{text}],        a user message; isMeta set for
#    synthetic_reason?}                    synthetic_reason lines AND grok's
#                                           own <user_info> envelope (not in
#                                           kb-core's Claude-specific wrapper
#                                           list — flagged here at the source
#                                           instead of extending that list)
#   {type:"assistant", content, tool_calls,  one assistant message; content =
#    model_id}                              [thinking?, text?, tool_use…]
#   {type:"tool_result", tool_call_id,       a user message carrying one
#    content}                                tool_result block (content
#                                             capped 16 KiB, head-60/tail-40)
#   {type:"reasoning", summary:[             folded into the FOLLOWING
#    {type:"summary_text",text}]}            assistant record's content as a
#                                             {type:"thinking"} block (grok
#                                             emits it as the immediately
#                                             preceding line — verified live)
#
# Tool-name map (verified live; unknowns pass through their raw grok name —
# the view engine's ToolClass sniffer already buckets those):
#   run_terminal_command → Bash {command}
#   read_file             → Read {file_path, offset?, limit?}
#   write                 → Write {file_path, content}
#   search_replace        → Edit {file_path, old_string, new_string}
#   grep / glob           → Grep / Glob (arguments pass through — same keys)
#   todo_write            → TodoWrite {todos}
#   web_search*           → WebSearch {query}
#
# Timestamps — the one real gap: chat_history.jsonl lines carry NO
# timestamp. events.jsonl DOES carry a `turn_started` event, but live data
# shows only ONE per session (turn_number always 0) even across a
# 14-assistant-turn session — too coarse to be the join key the design
# doc sketched. `loop_started` events (one per agent round, `loop_index`
# 0..N-1) correlate 1:1 with each assistant/tool_result group in file
# order instead, and are used here as the join key (documented deviation
# from the design's literal "turn_started" wording — loop_started is
# simply the finer-grained signal grok's live event stream actually
# provides for this exact purpose). Lines this can't resolve (leading
# user turns before the first loop, an events.jsonl-less session) fall
# back to summary.json's created_at — never fabricated, matching
# parse_session_activity's `.get()`-tolerant reading.
#
# GUARDS (fake-run + substance, both exit 0 with a one-line skip note —
# never a hard failure that could break a caller's fire-and-forget exec):
#   - GROKCLAUDE_FAKE set/truthy → skip.
#   - meta.json.grok_session_id absent, empty, or `fake-*` → skip (the
#     session never touched Grok's real store).
#   - the resolved session dir doesn't exist → skip.
#   - chat_history.jsonl has <1 user or <1 assistant line → skip (a
#     RECORDED exception to "capture stays dumb": this is a post-run
#     adapter, not a live hook — a job that errored before Grok ever
#     produced a real exchange leaves nothing worth indexing, and a
#     retried/resumed job re-captures on its next real round).
#
# Scrub posture: same as every harness (kb-capture.sh, codex, opencode) —
# captured verbatim, no capture-time secret scrub; outbound serve-time scrub
# (#4/#5) and export scrub floors apply identically once indexed. Grok's
# tool_result content can inline file contents the worker read (broad
# research-role read access per grokclaude's write-rails design) — the same
# posture as every other tool-output-bearing harness capture.
set -u
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

# Live-trigger fake-run guard (defense in depth — the Rust-side post-run
# trigger, item B, is ALSO gated on this before it ever execs this script).
case "${GROKCLAUDE_FAKE:-}" in
  1 | true | TRUE | yes | YES) echo "kb-capture-grok: skip (GROKCLAUDE_FAKE set)" >&2; exit 0 ;;
esac

GROK_SESSIONS_ROOT="${GROK_SESSIONS_ROOT:-$HOME/.grok/sessions}"
DRY_RUN=0
WITH_REPORT=0
MODE=""
JOB_DIR=""
SESSION_DIR=""
BACKFILL_ROOT=""

usage() {
  cat >&2 <<'EOF'
usage: kb-capture-grok.sh [--dry-run] [--with-report] <job-dir>
       kb-capture-grok.sh [--dry-run] [--with-report] --session-dir <dir> [--cwd <cwd>] [--job-ulid <ulid>] [--job-type <type>] [--round <n>]
       kb-capture-grok.sh [--dry-run] [--with-report] --backfill [--root <blackboard-root>]
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --with-report) WITH_REPORT=1; shift ;;
    --session-dir) MODE="session-dir"; SESSION_DIR="${2:-}"; shift 2 ;;
    --cwd) OPT_CWD="${2:-}"; shift 2 ;;
    --job-ulid) OPT_ULID="${2:-}"; shift 2 ;;
    --job-type) OPT_TYPE="${2:-}"; shift 2 ;;
    --round) OPT_ROUND="${2:-}"; shift 2 ;;
    --backfill) MODE="backfill"; shift ;;
    --root) BACKFILL_ROOT="${2:-}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *)
      if [ -z "$MODE" ]; then MODE="job-dir"; JOB_DIR="$1"; fi
      shift
      ;;
  esac
done

log() { printf 'kb-capture-grok: %s\n' "$1" >&2; }

# --- resolution ---------------------------------------------------------

# jq's @uri percent-encoding matches Rust's `percent-encoding`/`urlencoding`
# crates' default (unreserved: A-Za-z0-9-_.~) closely enough to reproduce
# the real on-disk directory names verified live (`/home/user/project/x` →
# `%2Fhome%2Fuser%2Fproject%2Fx`).
url_encode() { jq -rn --arg s "$1" '$s | @uri'; }

# Resolve a grok session uuid to its session directory. Priority: the job's
# recorded cwd, then its worktree (build jobs run in a `.grokclaude-worktrees/`
# checkout — grok itself was invoked with THAT as --cwd), then a bounded
# glob across every project dir under GROK_SESSIONS_ROOT (covers a job with
# neither field recorded, e.g. some `research` jobs — verified live: 1/17
# real jobs needed this fallback). Never opens/parses anything — directory
# existence only.
resolve_session_dir() {
  local gid="$1" cwd="${2:-}" worktree="${3:-}"
  local cand enc d
  for cand in "$cwd" "$worktree"; do
    [ -n "$cand" ] || continue
    enc="$(url_encode "$cand")"
    d="$GROK_SESSIONS_ROOT/$enc/$gid"
    if [ -d "$d" ]; then printf '%s\n' "$d"; return 0; fi
  done
  # Bounded fallback: at most 4096 project dirs (mirrors LF-1's presence
  # probe's own bound), stat-only.
  local n=0 pd
  [ -d "$GROK_SESSIONS_ROOT" ] || return 1
  for pd in "$GROK_SESSIONS_ROOT"/*/; do
    n=$((n + 1))
    [ "$n" -le 4096 ] || break
    d="${pd%/}/$gid"
    if [ -d "$d" ]; then printf '%s\n' "$d"; return 0; fi
  done
  return 1
}

# --- substance gate ------------------------------------------------------

substance_counts() {
  local dir="$1"
  local f="$dir/chat_history.jsonl"
  [ -f "$f" ] || { echo "0 0"; return; }
  local u a
  u="$(jq -r 'select(.type=="user") | 1' "$f" 2>/dev/null | wc -l | tr -d ' ')"
  a="$(jq -r 'select(.type=="assistant") | 1' "$f" 2>/dev/null | wc -l | tr -d ' ')"
  echo "$u $a"
}

# --- the transform (chat_history.jsonl → Claude-shaped JSONL) -----------

# Emits ONE Claude-shaped JSONL file (adapter-meta line first) to stdout.
# Arguments: session_dir gid job_ulid job_type round cwd
translate() {
  local dir="$1" gid="$2" job_ulid="$3" job_type="$4" round="$5" cwd="$6"
  local chat="$dir/chat_history.jsonl"
  local events="$dir/events.jsonl"
  local summary="$dir/summary.json"

  local created updated generated_title agent_name sandbox_profile reasoning_effort
  created="$(jq -r '.created_at // empty' "$summary" 2>/dev/null)"
  updated="$(jq -r '.updated_at // empty' "$summary" 2>/dev/null)"
  generated_title="$(jq -r '.generated_title // empty' "$summary" 2>/dev/null)"
  agent_name="$(jq -r '.agent_name // empty' "$summary" 2>/dev/null)"
  sandbox_profile="$(jq -r '.sandbox_profile // empty' "$summary" 2>/dev/null)"
  reasoning_effort="$(jq -r '.reasoning_effort // empty' "$summary" 2>/dev/null)"
  [ -n "$created" ] || created="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  [ -n "$updated" ] || updated="$created"

  local grok_version
  grok_version="$(grok --version 2>/dev/null | head -1)"
  [ -n "$grok_version" ] || grok_version="unknown"

  # loop_started timestamps, ordered by loop_index — the join key (see the
  # header note on the turn_started-vs-loop_started deviation).
  local loopts_json="[]"
  if [ -f "$events" ]; then
    loopts_json="$(jq -c '[inputs | select(.type=="loop_started")] | sort_by(.loop_index) | map(.ts)' "$events" 2>/dev/null)"
    [ -n "$loopts_json" ] || loopts_json="[]"
  fi

  # adapter-meta first.
  jq -n -c \
    --arg sid "$gid" --arg harness "grok" --arg driver "grokclaude" \
    --arg job_ulid "$job_ulid" --arg job_type "$job_type" --arg round "$round" \
    --arg session_dir "$dir" --arg cwd "$cwd" --arg grok_version "$grok_version" \
    --arg agent_name "$agent_name" --arg sandbox_profile "$sandbox_profile" \
    --arg reasoning_effort "$reasoning_effort" --arg generated_title "$generated_title" \
    --arg created "$created" \
    '{sessionId: $sid, type: "adapter-meta", adapter: "kb-capture-grok/1",
      harness: $harness, driver: $driver,
      job_ulid: (if $job_ulid == "" then null else $job_ulid end),
      job_type: (if $job_type == "" then null else $job_type end),
      round: (if $round == "" then null else $round end),
      session_dir: $session_dir,
      cwd: (if $cwd == "" then null else $cwd end),
      grok_version: $grok_version,
      agent_name: (if $agent_name == "" then null else $agent_name end),
      sandbox_profile: (if $sandbox_profile == "" then null else $sandbox_profile end),
      reasoning_effort: (if $reasoning_effort == "" then null else $reasoning_effort end),
      generated_title: (if $generated_title == "" then null else $generated_title end),
      timestamp: $created}
     | with_entries(select(.value != null))'

  [ -f "$chat" ] || return 0

  jq -n -c \
    --slurpfile loopts <(printf '%s' "$loopts_json") \
    --arg sid "$gid" --arg cwd "$cwd" --arg created "$created" --argjson cap 16384 \
    '
    ($loopts[0] // []) as $lts |
    def lts_at($i): if $i >= 0 and $i < ($lts|length) then ($lts[$i] // null) else null end;
    def cap_str($n):
      if (type=="string") and (length > $n) then
        (.[0:(($n*0.6)|floor)] + "\n…[truncated]…\n" + .[(length-(($n*0.4)|floor)):])
      else . end;
    def tool_map:
      . as $tc |
      (try ($tc.arguments | fromjson) catch {}) as $a |
      ($tc.name // "tool") as $nm |
      (
        if $nm == "run_terminal_command" then
          {name: "Bash", input: {command: ($a.command // "")}}
        elif $nm == "read_file" then
          {name: "Read", input: ({file_path: ($a.target_file // $a.path // "")}
            + (if $a.offset then {offset: $a.offset} else {} end)
            + (if $a.limit then {limit: $a.limit} else {} end))}
        elif $nm == "write" then
          {name: "Write", input: {file_path: ($a.file_path // ""), content: ($a.content // "")}}
        elif $nm == "search_replace" then
          {name: "Edit", input: {file_path: ($a.file_path // ""),
                                  old_string: ($a.old_string // ""),
                                  new_string: ($a.new_string // "")}}
        elif $nm == "grep" then {name: "Grep", input: $a}
        elif $nm == "glob" then {name: "Glob", input: $a}
        elif $nm == "todo_write" then {name: "TodoWrite", input: $a}
        elif ($nm | test("^web_search")) then
          {name: "WebSearch", input: {query: ($a.query // "")}}
        else {name: $nm, input: $a}
        end
      ) as $mapped |
      {type: "tool_use", id: ($tc.id // ""), name: $mapped.name, input: $mapped.input};
    foreach (inputs) as $l (
      {li: -1, pend: null, thinking: null};
      if $l.type == "reasoning" then
        {li: (.li + 1),
         pend: ([$l.summary[]? | select(.type=="summary_text") | (.text // "")] | join("\n")),
         thinking: null}
      elif $l.type == "assistant" then
        {li: (if .pend != null then .li else .li + 1 end), pend: null, thinking: .pend}
      else
        {li: .li, pend: .pend, thinking: null}
      end;
      ($l.type // "") as $t |
      if $t == "system" then
        (($l.content // "") | length) as $n |
        {sessionId: $sid, type: "user", cwd: $cwd, isMeta: true,
         timestamp: $created,
         message: {role: "user", content: [
           {type: "text", text: ("[grok system prompt, " + ($n|tostring) + " chars — see adapter-meta source_dir]")}]}}
      elif $t == "user" then
        ([$l.content[]? | select(.type=="text") | (.text // "")] | join("\n")) as $txt |
        if $txt == "" then empty else
          {sessionId: $sid, type: "user", cwd: $cwd,
           timestamp: (lts_at(.li) // $created),
           isMeta: (($l.synthetic_reason? // null) != null
                    or ($txt | startswith("<user_info>"))
                    or ($txt | startswith("<system-reminder>"))),
           message: {role: "user", content: [{type: "text", text: $txt}]}}
        end
      elif $t == "assistant" then
        ([ (if (.thinking // "") != "" then [{type: "thinking", thinking: .thinking}] else [] end),
           (if ($l.content // "") != "" then [{type: "text", text: $l.content}] else [] end),
           [$l.tool_calls[]? | tool_map]
         ] | add) as $content |
        if ($content | length) == 0 then empty else
          {sessionId: $sid, type: "assistant",
           timestamp: (lts_at(.li) // $created),
           message: {role: "assistant", model: ($l.model_id // "grok"), content: $content}}
        end
      elif $t == "tool_result" then
        (($l.content // "") | if type == "string" then . else tojson end) as $raw |
        {sessionId: $sid, type: "user",
         timestamp: (lts_at(.li) // $created),
         message: {role: "user", content: [
           {type: "tool_result", tool_use_id: ($l.tool_call_id // ""),
            is_error: false,
            content: [{type: "text", text: ($raw | cap_str($cap))}]}]}}
      else empty end
    )
    ' "$chat"
}

# --- report lane (D5) -----------------------------------------------------

# Deterministic render of report.md + findings/*.json into a linked kb
# artifact — no LLM, just formatting of files that already exist.
# `kb-category: reference` (a durable authored-artifact convention already
# used elsewhere in this ecosystem, e.g. grokclaude's own
# docs/examples/claude-code-smoke.html) rather than `memory-session`
# (reserved for actual transcript captures — the session-view renderer, R0
# search exclusion, and the #11 multi-capture/newest-capture machinery all
# key off that category, none of which apply to a static report doc) or
# `research` (kb's `research` corpus convention is for standalone authored
# deliverables, not per-job derivative artifacts). `kb-decay: fast` matches
# every other session-adjacent artifact (#11). The link back to the
# transcript capture is `<meta name="kb-session" content="<grok-uuid>">` —
# NOT a tail block on the transcript itself (never inject content into a
# capture's envelope) — which rides the EXISTING generic S1 `kb_session`
# lance column (`GET /api/sessions/{sid}/memories` filters purely on that
# column, category-agnostic — verified: `list_docs_with_kb_session` in
# kb-server/src/routes/sessions.rs carries no category constraint), so the
# report surfaces automatically as one of the session's "memories produced"
# without any new join code. `kb-tags: origin:grokclaude job:<ulid>` adds
# the driver/child job cross-link visibility on top.
write_report() {
  local job_dir="$1" gid="$2" job_ulid="$3" dry_run="$4"
  local report_md="$job_dir/report.md"
  [ -f "$report_md" ] || return 0

  local ts out tmp title
  ts="$(date -u +%Y%m%dT%H%M%SZ)"
  title="$(grep -m1 '^# ' "$report_md" 2>/dev/null | sed 's/^# *//')"
  [ -n "$title" ] || title="Grok job report $job_ulid"

  local safe_ulid out_glob f
  safe_ulid="$(printf '%s' "$job_ulid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
  out=""
  for f in "$KB_SESSIONS_DIR"/grok-report-"$safe_ulid".html; do
    [ -f "$f" ] && out="$f"
  done
  [ -n "$out" ] || out="$KB_SESSIONS_DIR/grok-report-$safe_ulid.html"

  if [ "$dry_run" = "1" ]; then
    log "[dry-run] would write report artifact -> $out"
    return 0
  fi

  local body findings_html esc_body
  body="$(cat "$report_md")"
  findings_html=""
  if [ -d "$job_dir/findings" ]; then
    local n=0
    for f in "$job_dir/findings"/*.json; do
      [ -f "$f" ] || continue
      n=$((n + 1))
      local claim confidence ev
      claim="$(jq -r '.claim // ""' "$f" 2>/dev/null)"
      confidence="$(jq -r '.confidence // ""' "$f" 2>/dev/null)"
      ev="$(jq -r '[.evidence[]?.path // empty] | join(", ")' "$f" 2>/dev/null)"
      findings_html="$findings_html$(printf '<li><strong>%s</strong> (%s) — %s</li>\n' \
        "$(html_escape "$claim")" "$(html_escape "$confidence")" "$(html_escape "$ev")")"
    done
    [ "$n" -gt 0 ] && findings_html="<h2 id=\"findings\">Findings (evidence index)</h2><ul>$findings_html</ul>"
  fi

  esc_body="$(html_escape "$body")"
  tmp="$out.tmp"
  cat >"$tmp" <<EOF || { rm -f "$tmp"; return 0; }
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>$(html_escape "$title")</title>
<meta name="kb-category" content="reference">
<meta name="kb-decay" content="fast">
<meta name="kb-session" content="$gid">
<meta name="kb-tags" content="origin:grokclaude, job:$job_ulid">
</head><body>
<h1 id="report">$(html_escape "$title")</h1>
<p>Grok Build job <code>$job_ulid</code> — deterministic render of <code>report.md</code>
(+ findings evidence index), captured $ts. Linked session transcript:
<code>$gid</code>.</p>
<pre>$esc_body</pre>
$findings_html
</body></html>
EOF
  mv -f "$tmp" "$out" 2>/dev/null || { rm -f "$tmp"; return 0; }
  log "wrote report artifact -> $out"
}

html_escape() {
  printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}

# --- distill-pending relay (MI-W0.3) ---------------------------------------
#
# Grok runs headless — nothing reads a Stop-time systemMessage the way
# kb-distill-nudge.sh's own trick relies on — so a "commit without a
# successful kb remember" signal can't nudge in place here. Instead it's
# QUEUED: append one line to a small ledger that kb-wake.sh (SessionStart,
# the next INTERACTIVE session, any harness) surfaces and consumes. Reuses
# the exact same two regexes as kb-distill-nudge.sh against the
# SYNTHESIZED Claude-shaped JSONL this script just captured (`translate()`'s
# output, still sitting in $tmpjsonl at the call site below) — the
# `"command":"…git commit` trigger and the `remembered <12-hex-id>` success
# marker — because the translated JSONL is byte-shaped like a real Claude
# transcript, so the same patterns apply unmodified; no grok-specific regex
# needed. Best-effort and silent: any failure here must never fail a
# capture that already succeeded. Dedup by session id — a re-capture of the
# same session (retry, resumed job) must not queue a second ledger line.
queue_distill_pending() {
  local tmpjsonl="$1" gid="$2"
  grep -qE '"command":"([^"\\]|\\.)*git commit' "$tmpjsonl" 2>/dev/null || return 0
  grep -qE 'remembered [0-9a-f]{12}' "$tmpjsonl" 2>/dev/null && return 0
  local dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
  local ledger="$dir/distill-pending"
  mkdir -p "$dir" 2>/dev/null || return 0
  [ -f "$ledger" ] && grep -qF "grok $gid " "$ledger" 2>/dev/null && return 0
  printf 'grok %s %s\n' "$gid" "$(date +%s)" >>"$ledger" 2>/dev/null
  return 0
}

# --- one capture -----------------------------------------------------------

# capture_one session_dir gid job_dir job_ulid job_type round cwd dry_run with_report
capture_one() {
  local dir="$1" gid="$2" job_dir="$3" job_ulid="$4" job_type="$5" round="$6" cwd="$7" dry_run="$8" with_report="$9"

  read -r ucount acount <<<"$(substance_counts "$dir")"

  if [ "$dry_run" = "1" ]; then
    log "job=${job_ulid:-direct} grok_session=$gid session_dir=$dir user_lines=$ucount assistant_lines=$acount"
  fi

  if [ "$ucount" -lt 1 ] || [ "$acount" -lt 1 ]; then
    log "job=${job_ulid:-direct} grok_session=$gid action=skip reason=no-substance (user=$ucount assistant=$acount)"
    return 1
  fi

  if [ "$dry_run" = "1" ]; then
    log "job=${job_ulid:-direct} grok_session=$gid action=would-capture"
    [ "$with_report" = "1" ] && [ -n "$job_dir" ] && write_report "$job_dir" "$gid" "$job_ulid" 1
    return 0
  fi

  local tmpjsonl
  tmpjsonl="$(mktemp "${TMPDIR:-/tmp}/kb-grok-capture.XXXXXX.jsonl")" || return 1
  translate "$dir" "$gid" "$job_ulid" "$job_type" "$round" "$cwd" >"$tmpjsonl"
  if [ ! -s "$tmpjsonl" ]; then
    rm -f "$tmpjsonl"
    log "job=${job_ulid:-direct} grok_session=$gid action=skip reason=empty-translation"
    return 1
  fi

  local wrote=0
  if command -v kb >/dev/null 2>&1; then
    if kb sessions capture \
         --transcript "$tmpjsonl" \
         --session-id "$gid" \
         ${cwd:+--cwd "$cwd"} \
         --out "$KB_SESSIONS_DIR" \
         >/dev/null 2>&1; then
      wrote=1
    fi
  fi

  if [ "$wrote" -eq 0 ]; then
    # 2026-08-21 ci-host incident hardening — refuse an oversized translated
    # transcript in this bash fallback path (the Rust `kb sessions capture`
    # call above already refuses one on its own — this guards the exact
    # case where that refusal is why wrote=0). A capture skip must never
    # abort the wider grokclaude backfill.
    local tsize
    tsize="$(stat -c %s "$tmpjsonl" 2>/dev/null || wc -c <"$tmpjsonl" 2>/dev/null)"
    if [ -n "$tsize" ] && [ "$tsize" -gt 50331648 ]; then
      log "job=${job_ulid:-direct} grok_session=$gid action=skip reason=oversized-transcript ($tsize bytes > 48MiB cap)"
      rm -f "$tmpjsonl"
      return 1
    fi

    # Bash fallback — hand-rolled envelope, same contract as
    # kb-capture.sh's own fallback path (no commit resolution, no sidecar
    # walk; still atomic, still one-file-per-sid).
    mkdir -p "$KB_SESSIONS_DIR" || { rm -f "$tmpjsonl"; return 1; }
    local ts safe_sid out f esc tmp
    ts="$(date -u +%Y%m%dT%H%M%SZ)"
    safe_sid="$(printf '%s' "$gid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
    out=""
    for f in "$KB_SESSIONS_DIR"/session-*-"$safe_sid.html"; do
      [ -f "$f" ] && out="$f"
    done
    [ -n "$out" ] || out="$KB_SESSIONS_DIR/session-$ts-$safe_sid.html"
    esc="$(sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' "$tmpjsonl")" || { rm -f "$tmpjsonl"; return 1; }
    tmp="$out.tmp"
    cat >"$tmp" <<EOF || { rm -f "$tmp" "$tmpjsonl"; return 1; }
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Grok session transcript $ts</title>
<meta name="kb-category" content="memory-session">
<meta name="kb-decay" content="fast">
<meta name="kb-session" content="$safe_sid">
<meta name="kb-harness" content="grok">
</head><body>
<h1>Grok session transcript $ts</h1>
<pre>$esc</pre>
</body></html>
EOF
    mv -f "$tmp" "$out" 2>/dev/null && wrote=1 || rm -f "$tmp"
  fi

  if [ "$wrote" -eq 1 ]; then
    queue_distill_pending "$tmpjsonl" "$gid"
  fi
  rm -f "$tmpjsonl"
  if [ "$wrote" -eq 1 ]; then
    log "job=${job_ulid:-direct} grok_session=$gid action=captured"
    [ "$with_report" = "1" ] && [ -n "$job_dir" ] && write_report "$job_dir" "$gid" "$job_ulid" 0
    return 0
  fi
  log "job=${job_ulid:-direct} grok_session=$gid action=skip reason=write-failed"
  return 1
}

# capture_job job_dir dry_run with_report
capture_job() {
  local job_dir="$1" dry_run="$2" with_report="$3"
  local meta="$job_dir/meta.json"
  [ -f "$meta" ] || { log "job-dir=$job_dir action=skip reason=no-meta-json"; return 1; }

  local ulid gid cwd worktree jtype round
  ulid="$(jq -r '.id // empty' "$meta" 2>/dev/null)"
  gid="$(jq -r '.grok_session_id // empty' "$meta" 2>/dev/null)"
  cwd="$(jq -r '.cwd // empty' "$meta" 2>/dev/null)"
  worktree="$(jq -r '.worktree // empty' "$meta" 2>/dev/null)"
  jtype="$(jq -r '.type // empty' "$meta" 2>/dev/null)"
  round="$(jq -r '.round // empty' "$meta" 2>/dev/null)"

  if [ -z "$gid" ] || [[ "$gid" == fake-* ]]; then
    if [ "$dry_run" = "1" ]; then
      log "job=$ulid grok_session=MISSING fake=$([ -z "$gid" ] && echo no || echo yes) action=skip reason=fake-or-no-session"
    fi
    return 1
  fi

  local dir
  if ! dir="$(resolve_session_dir "$gid" "$cwd" "$worktree")"; then
    if [ "$dry_run" = "1" ]; then
      log "job=$ulid grok_session=$gid session_dir=MISSING action=skip reason=session-dir-not-found"
    fi
    return 1
  fi

  capture_one "$dir" "$gid" "$job_dir" "$ulid" "$jtype" "$round" "$cwd" "$dry_run" "$with_report"
}

# --- backfill --------------------------------------------------------------

default_blackboard_root() {
  if [ -n "${GROKCLAUDE_ROOT:-}" ]; then
    printf '%s\n' "$GROKCLAUDE_ROOT"
    return
  fi
  local d="$PWD"
  while [ "$d" != "/" ]; do
    if [ -d "$d/.grokclaude" ]; then
      printf '%s\n' "$d/.grokclaude"
      return
    fi
    d="$(dirname "$d")"
  done
  printf '%s\n' "$HOME/.local/share/grokclaude"
}

run_backfill() {
  local root="$1" dry_run="$2" with_report="$3"
  [ -n "$root" ] || root="$(default_blackboard_root)"
  local jobs_dir="$root/jobs"
  if [ ! -d "$jobs_dir" ]; then
    log "backfill: no jobs dir under $root"
    return 1
  fi
  local total=0 captured=0 skipped=0
  local jd
  for jd in "$jobs_dir"/*/; do
    [ -d "$jd" ] || continue
    total=$((total + 1))
    if capture_job "${jd%/}" "$dry_run" "$with_report"; then
      captured=$((captured + 1))
    else
      skipped=$((skipped + 1))
    fi
  done
  log "backfill: scanned=$total captured=$captured skipped=$skipped root=$root"
}

# --- dispatch ----------------------------------------------------------

case "$MODE" in
  job-dir)
    [ -n "$JOB_DIR" ] || { usage; exit 1; }
    capture_job "$JOB_DIR" "$DRY_RUN" "$WITH_REPORT"
    exit 0
    ;;
  session-dir)
    [ -n "$SESSION_DIR" ] || { usage; exit 1; }
    gid="$(basename "$SESSION_DIR")"
    capture_one "$SESSION_DIR" "$gid" "" "${OPT_ULID:-}" "${OPT_TYPE:-}" "${OPT_ROUND:-}" "${OPT_CWD:-}" "$DRY_RUN" "$WITH_REPORT"
    exit 0
    ;;
  backfill)
    run_backfill "$BACKFILL_ROOT" "$DRY_RUN" "$WITH_REPORT"
    exit 0
    ;;
  *)
    usage
    exit 1
    ;;
esac
