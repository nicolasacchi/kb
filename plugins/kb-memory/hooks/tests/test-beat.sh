#!/usr/bin/env bash
# test-beat.sh — LSC-3: self-contained test matrix for kb-beat.sh (+ a
# small bonus section for kb-beat-throttle.sh, its PostToolUse gate).
# Runs the REAL scripts with KB_BEAT_DRYRUN=1 so the JSON body construction
# is exercised without any network call — no fake `kb` binary needed, `jq`
# and `curl` are real (kb-beat.sh only needs `curl`/`jq` to be ON PATH; the
# dryrun path never actually invokes curl).
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-beat.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BEAT="$HOOKS_DIR/kb-beat.sh"
BEAT_THROTTLE="$HOOKS_DIR/kb-beat-throttle.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-beat-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"
export XDG_CACHE_HOME="$TMPROOT/cache"

echo "== kb-beat.sh (LSC-3 push collection) test matrix =="
echo "tmp root: $TMPROOT"
echo

# ---------------------------------------------------------------------
# helper: run kb-beat.sh in dry-run mode, capturing stdout + exit code.
# beat_dryrun <harness> <event> <stdin-json> [extra env assignments...]
# ---------------------------------------------------------------------
beat_dryrun() {
  local harness="$1" event="$2" stdin="$3"
  shift 3
  local out rc
  out="$(printf '%s' "$stdin" | env KB_BEAT_DRYRUN=1 "$@" "$BEAT" "$harness" "$event" 2>/dev/null)"
  rc=$?
  printf '%s\n' "$out"
  return "$rc"
}

jget() { printf '%s' "$1" | jq -r "$2" 2>/dev/null; }

# --- 1. Claude SessionStart -> event "start", core fields present --------
p1='{"session_id":"s-start-1","cwd":"/tmp/proj-a","source":"startup"}'
out1="$(beat_dryrun claude start "$p1")"; rc1=$?
if [ "$rc1" -eq 0 ] && [ "$(jget "$out1" '.v')" = "1" ] \
   && [ "$(jget "$out1" '.session_id')" = "s-start-1" ] \
   && [ "$(jget "$out1" '.harness')" = "claude" ] \
   && [ "$(jget "$out1" '.event')" = "start" ] \
   && [ "$(jget "$out1" '.cwd')" = "/tmp/proj-a" ] \
   && [ "$(jget "$out1" '.at')" != "null" ] \
   && [ "$(jget "$out1" '.lease_secs')" = "900" ]; then
  ok "SessionStart -> event=start, v=1, session_id/cwd/at/lease_secs populated"
else
  bad "SessionStart -> event=start (got: $out1, rc=$rc1)"
fi
case "$out1" in
  *'"host"'*) ok "start beat carries a host field" ;;
  *) bad "start beat should carry a host field (got: $out1)" ;;
esac

# --- 2. Claude UserPromptSubmit -> event "prompt" --------------------------
p2='{"session_id":"s-prompt-1","cwd":"/tmp/proj-a","prompt_id":"p1","permission_mode":"default","prompt":"hello"}'
out2="$(beat_dryrun claude prompt "$p2")"
if [ "$(jget "$out2" '.event')" = "prompt" ] && [ "$(jget "$out2" '.session_id')" = "s-prompt-1" ]; then
  ok "UserPromptSubmit -> event=prompt"
else
  bad "UserPromptSubmit -> event=prompt (got: $out2)"
fi

# --- 3. Claude Stop -> event "turn_end", last_line = last_assistant_message
p3='{"session_id":"s-stop-1","cwd":"/tmp/proj-a","hook_event_name":"Stop","last_assistant_message":"All done, tests pass."}'
out3="$(beat_dryrun claude turn_end "$p3")"
if [ "$(jget "$out3" '.event')" = "turn_end" ] \
   && [ "$(jget "$out3" '.last_line')" = "All done, tests pass." ]; then
  ok "Stop -> event=turn_end, last_line = last_assistant_message verbatim"
else
  bad "Stop -> event=turn_end + last_line (got: $out3)"
fi

# --- 4. last_line is TRUNCATED to 240 chars --------------------------------
longmsg="$(printf 'x%.0s' $(seq 1 400))"
p4="$(jq -nc --arg sid "s-stop-2" --arg cwd "/tmp/proj-a" --arg m "$longmsg" \
  '{session_id:$sid, cwd:$cwd, hook_event_name:"Stop", last_assistant_message:$m}')"
out4="$(beat_dryrun claude turn_end "$p4")"
ll4="$(jget "$out4" '.last_line')"
len4="${#ll4}"
if [ "$len4" -eq 240 ]; then
  ok "last_line truncated to exactly 240 chars (got length $len4)"
else
  bad "last_line truncated to exactly 240 chars (got length $len4)"
fi

# --- 5. last_line OMITTED when KB_BEAT_CONTENT=0 ---------------------------
out5="$(beat_dryrun claude turn_end "$p3" KB_BEAT_CONTENT=0)"
if printf '%s' "$out5" | jq -e 'has("last_line")' >/dev/null 2>&1; then
  bad "KB_BEAT_CONTENT=0 must omit last_line (got: $out5)"
else
  ok "KB_BEAT_CONTENT=0 omits last_line entirely"
fi

# --- 6. Claude Notification -> event "blocked", detail.reason from .message
p6='{"session_id":"s-notif-1","cwd":"/tmp/proj-a","message":"Claude needs your permission to use Bash"}'
out6="$(beat_dryrun claude blocked "$p6")"
if [ "$(jget "$out6" '.event')" = "blocked" ] \
   && [ "$(jget "$out6" '.detail.reason')" = "Claude needs your permission to use Bash" ]; then
  ok "Notification -> event=blocked, detail.reason from .message"
else
  bad "Notification -> event=blocked + detail.reason (got: $out6)"
fi
# a non-blocked event must never carry a detail key at all.
if printf '%s' "$out3" | jq -e 'has("detail")' >/dev/null 2>&1; then
  bad "a turn_end beat must not carry a detail key (got: $out3)"
else
  ok "non-blocked beats never carry a detail key"
fi

# --- 7. Claude SessionEnd -> event "end" ------------------------------------
p7='{"session_id":"s-end-1","cwd":"/tmp/proj-a","hook_event_name":"SessionEnd","reason":"other"}'
out7="$(beat_dryrun claude end "$p7")"
if [ "$(jget "$out7" '.event')" = "end" ] && [ "$(jget "$out7" '.session_id')" = "s-end-1" ]; then
  ok "SessionEnd -> event=end"
else
  bad "SessionEnd -> event=end (got: $out7)"
fi

# --- 8. codex: session_id resolved from the rollout's OWN --------------
#        session_meta.payload.id (ground truth), not re-derived, mirroring
#        kb-capture-codex.sh's own extraction exactly. The hook payload
#        itself carries no .session_id at all, matching codex's real shape.
rollout="$TMPROOT/codex-rollout.jsonl"
cat >"$rollout" <<'JSONL'
{"timestamp":"2026-08-01T00:00:00.000Z","type":"session_meta","payload":{"id":"codex-sess-99","timestamp":"2026-08-01T00:00:00.000Z","cwd":"/tmp/proj-b","originator":"codex_cli","cli_version":"0.99.0"}}
{"timestamp":"2026-08-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}
JSONL
p8="$(jq -nc --arg cwd "/tmp/proj-b" --arg tp "$rollout" '{cwd:$cwd, transcript_path:$tp}')"
out8="$(beat_dryrun codex turn_end "$p8")"
if [ "$(jget "$out8" '.session_id')" = "codex-sess-99" ] && [ "$(jget "$out8" '.harness')" = "codex" ]; then
  ok "codex: session_id resolved from rollout session_meta.payload.id"
else
  bad "codex: session_id resolved from rollout session_meta.payload.id (got: $out8)"
fi

# --- 9. codex: falls back to a bare .session_id when no resolvable rollout -
p9='{"cwd":"/tmp/proj-b","transcript_path":"/nonexistent/rollout.jsonl","session_id":"codex-fallback-1"}'
out9="$(beat_dryrun codex prompt "$p9")"
if [ "$(jget "$out9" '.session_id')" = "codex-fallback-1" ]; then
  ok "codex: falls back to bare .session_id when the rollout can't be read"
else
  bad "codex: falls back to bare .session_id when the rollout can't be read (got: $out9)"
fi

# --- 10. malformed stdin -> exits 0, sends nothing --------------------------
out10="$(printf 'this is not json at all' | env KB_BEAT_DRYRUN=1 "$BEAT" claude start 2>/dev/null)"
rc10=$?
if [ "$rc10" -eq 0 ] && [ -z "$out10" ]; then
  ok "malformed stdin -> exit 0, nothing printed/sent"
else
  bad "malformed stdin -> exit 0, nothing printed/sent (rc=$rc10, got: $out10)"
fi

# --- 11. empty stdin -> exits 0, sends nothing ------------------------------
out11="$(printf '' | env KB_BEAT_DRYRUN=1 "$BEAT" claude start 2>/dev/null)"
rc11=$?
if [ "$rc11" -eq 0 ] && [ -z "$out11" ]; then
  ok "empty stdin -> exit 0, nothing printed/sent"
else
  bad "empty stdin -> exit 0, nothing printed/sent (rc=$rc11, got: $out11)"
fi

# --- 12. KB_BEAT=0 -> exits 0, sends nothing, even with a valid payload ----
out12="$(printf '%s' "$p1" | env KB_BEAT=0 KB_BEAT_DRYRUN=1 "$BEAT" claude start 2>/dev/null)"
rc12=$?
if [ "$rc12" -eq 0 ] && [ -z "$out12" ]; then
  ok "KB_BEAT=0 -> exit 0, nothing printed/sent"
else
  bad "KB_BEAT=0 -> exit 0, nothing printed/sent (rc=$rc12, got: $out12)"
fi

# --- 13. KB_SESSIONS_DIR unset (kb not configured) -> exit 0, nothing ------
out13="$(printf '%s' "$p1" | env -u KB_SESSIONS_DIR KB_BEAT_DRYRUN=1 "$BEAT" claude start 2>/dev/null)"
rc13=$?
if [ "$rc13" -eq 0 ] && [ -z "$out13" ]; then
  ok "KB_SESSIONS_DIR unset -> exit 0, nothing printed/sent (kb not configured)"
else
  bad "KB_SESSIONS_DIR unset -> exit 0, nothing printed/sent (rc=$rc13, got: $out13)"
fi

# --- 14. missing harness/event argv -> exit 0, nothing --------------------
out14="$(printf '%s' "$p1" | env KB_BEAT_DRYRUN=1 "$BEAT" 2>/dev/null)"
rc14=$?
if [ "$rc14" -eq 0 ] && [ -z "$out14" ]; then
  ok "no harness/event argv -> exit 0, nothing printed/sent"
else
  bad "no harness/event argv -> exit 0, nothing printed/sent (rc=$rc14, got: $out14)"
fi

# --- 15. no session_id anywhere resolvable -> exit 0, nothing --------------
p15='{"cwd":"/tmp/proj-a"}'
out15="$(beat_dryrun claude start "$p15")"
rc15=$?
if [ "$rc15" -eq 0 ] && [ -z "$out15" ]; then
  ok "no resolvable session_id -> exit 0, nothing printed/sent"
else
  bad "no resolvable session_id -> exit 0, nothing printed/sent (rc=$rc15, got: $out15)"
fi

# --- 16. daemon unreachable (real curl, closed port) -> exits 0, and -------
#         returns fast (proves the POST is actually backgrounded rather than
#         this process blocking on curl's own --max-time 2 budget).
t0="$(date +%s%N)"
printf '%s' "$p1" | env KB_DAEMON_URL="http://127.0.0.1:39217" "$BEAT" claude start
rc16=$?
t1="$(date +%s%N)"
elapsed_ms=$(( (t1 - t0) / 1000000 ))
if [ "$rc16" -eq 0 ]; then
  ok "daemon unreachable (closed port) -> script still exits 0"
else
  bad "daemon unreachable (closed port) -> script still exits 0 (rc=$rc16)"
fi
if [ "$elapsed_ms" -lt 1900 ]; then
  ok "daemon unreachable -> script returns fast ($elapsed_ms ms, backgrounded not blocking)"
else
  bad "daemon unreachable -> script returns fast (took ${elapsed_ms}ms, expected < 1900ms)"
fi

echo
echo "== kb-beat-throttle.sh (PostToolUse heartbeat gate) bonus checks =="

# --- 17. first PostToolUse for a session fires immediately -----------------
p17='{"session_id":"s-tool-1","cwd":"/tmp/proj-a"}'
out17="$(printf '%s' "$p17" | env KB_BEAT_DRYRUN=1 "$BEAT_THROTTLE" claude 2>/dev/null)"
if [ "$(jget "$out17" '.event')" = "tool" ] && [ "$(jget "$out17" '.session_id')" = "s-tool-1" ]; then
  ok "first PostToolUse for a session fires a tool beat"
else
  bad "first PostToolUse for a session fires a tool beat (got: $out17)"
fi

# --- 18. a second call within the interval is throttled (silent) ----------
out18="$(printf '%s' "$p17" | env KB_BEAT_DRYRUN=1 KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS=180 "$BEAT_THROTTLE" claude 2>/dev/null)"
if [ -z "$out18" ]; then
  ok "a second PostToolUse within the interval is throttled (no beat)"
else
  bad "a second PostToolUse within the interval is throttled (got: $out18)"
fi

# --- 19. KB_BEAT_HEARTBEAT=0 disables the heartbeat outright ---------------
p19='{"session_id":"s-tool-2","cwd":"/tmp/proj-a"}'
out19="$(printf '%s' "$p19" | env KB_BEAT_HEARTBEAT=0 KB_BEAT_DRYRUN=1 "$BEAT_THROTTLE" claude 2>/dev/null)"
if [ -z "$out19" ]; then
  ok "KB_BEAT_HEARTBEAT=0 disables the heartbeat outright"
else
  bad "KB_BEAT_HEARTBEAT=0 disables the heartbeat outright (got: $out19)"
fi

# --- 20. an interval of 0 lets the NEXT call through immediately -----------
out20="$(printf '%s' "$p17" | env KB_BEAT_DRYRUN=1 KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS=0 "$BEAT_THROTTLE" claude 2>/dev/null)"
if [ "$(jget "$out20" '.event')" = "tool" ]; then
  ok "min-interval=0 lets a repeat call through immediately"
else
  bad "min-interval=0 lets a repeat call through immediately (got: $out20)"
fi

echo
echo "== OL PRES1: write-tool beats carry paths =="

HOOKS_JSON="$HOOKS_DIR/hooks.json"

# --- 21. hooks.json: ONE PostToolUse group; the write-tool beat rides the throttle
if jq -e '
     .hooks.PostToolUse as $g
     | ($g | length) == 1
     and ($g[0].matcher == null)
     and ($g[0].hooks | map(.command) == [
          "\"${CLAUDE_PLUGIN_ROOT}/hooks/kb-capture-throttle.sh\"",
          "\"${CLAUDE_PLUGIN_ROOT}/hooks/kb-beat-throttle.sh\" claude"])
   ' "$HOOKS_JSON" >/dev/null 2>&1; then
  ok "hooks.json: a single PostToolUse group (no second hook process tree per edit)"
else
  bad "hooks.json PostToolUse must be one group, capture-throttle + beat-throttle"
fi

wt() { # wt <tool_name> <cwd> <tool_input-json> [env...]
  local tool="$1" cwd="$2" ti="$3"
  shift 3
  beat_dryrun claude tool "{\"session_id\":\"s-wt\",\"cwd\":\"$cwd\",\"tool_name\":\"$tool\",\"tool_input\":$ti}" "$@"
}

# --- 22. the four write tools carry paths; Read/Bash carry none ------------
for pair in 'Edit:file_path' 'Write:file_path' 'MultiEdit:file_path' 'NotebookEdit:notebook_path'; do
  t="${pair%%:*}"; k="${pair##*:}"
  o="$(wt "$t" /tmp/proj-a "{\"$k\":\"/tmp/proj-a/src/x.rs\"}")"
  if [ "$(jget "$o" '.detail.tool')" = "$t" ] \
     && [ "$(jget "$o" '.detail.paths | join(",")')" = "/tmp/proj-a/src/x.rs" ] \
     && [ "$(jget "$o" '.detail.root')" = "/tmp/proj-a" ] \
     && [ "$(jget "$o" '.detail.project')" = "proj-a" ]; then
    ok "$t beat carries detail.tool + detail.paths + root/project (non-git cwd falls back to cwd)"
  else
    bad "$t beat carries paths (got: $o)"
  fi
done
for t in Read Bash; do
  o="$(wt "$t" /tmp/proj-a '{"file_path":"/tmp/proj-a/x","command":"ls"}')"
  if [ "$(jget "$o" '.detail')" = "null" ]; then
    ok "$t beat carries no detail at all"
  else
    bad "$t beat carries no detail (got: $o)"
  fi
done

# --- 23. relative path joined onto cwd; lexically normalised ---------------
o="$(wt Edit /tmp/proj-a '{"file_path":"src/../lib/a.rs"}')"
if [ "$(jget "$o" '.detail.paths | join(",")')" = "/tmp/proj-a/lib/a.rs" ]; then
  ok "relative path is joined onto cwd and lexically normalised"
else
  bad "relative path join (got: $o)"
fi

# --- 24. `..` escape, control chars, over-long paths are dropped -----------
o="$(wt Edit /tmp/proj-a '{"file_path":"../../../../etc/passwd"}')"
if [ "$(jget "$o" '.detail.paths')" = "null" ]; then
  ok "a relative path whose .. climbs out of its base is dropped"
else
  bad ".. escape dropped (got: $o)"
fi
o="$(wt Edit /tmp/proj-a '{"file_path":"/tmp/proj-a/../../../x"}')"
if [ "$(jget "$o" '.detail.paths')" = "null" ]; then
  ok "an absolute path whose .. climbs past / is dropped"
else
  bad "absolute .. escape dropped (got: $o)"
fi
o="$(wt Edit /tmp/proj-a '{"file_path":"/tmp/proj-a/a\tb"}')"
if [ "$(jget "$o" '.detail.paths')" = "null" ]; then
  ok "a path carrying a control character is dropped"
else
  bad "control char dropped (got: $o)"
fi
long="$(printf 'a%.0s' $(seq 1 1100))"
o="$(wt Edit /tmp/proj-a "{\"file_path\":\"/tmp/proj-a/$long\"}")"
if [ "$(jget "$o" '.detail.paths')" = "null" ]; then
  ok "an over-long (>1024 byte) path is dropped"
else
  bad "over-long path dropped (got: $o)"
fi

# --- 25. at most 8 paths ---------------------------------------------------
edits="$(jq -nc '[range(0;9) | {file_path: "/tmp/proj-a/f\(.).rs"}]')"
o="$(wt MultiEdit /tmp/proj-a "{\"edits\":$edits}")"
if [ "$(jget "$o" '.detail.paths | length')" = "8" ] \
   && [ "$(jget "$o" '.detail.paths[7]')" = "/tmp/proj-a/f7.rs" ]; then
  ok "the 9th path is truncated (max 8)"
else
  bad "9th path truncated (got: $o)"
fi

# --- 26. KB_BEAT_PATHS=0 removes every file-presence field -----------------
o="$(wt Edit /tmp/proj-a '{"file_path":"/tmp/proj-a/x"}' KB_BEAT_PATHS=0)"
if [ "$(jget "$o" '.detail')" = "null" ]; then
  ok "KB_BEAT_PATHS=0 removes detail.tool/paths/root/project"
else
  bad "KB_BEAT_PATHS=0 (got: $o)"
fi

# --- 27. KB_BEAT=0 prints nothing -----------------------------------------
o="$(wt Edit /tmp/proj-a '{"file_path":"/tmp/proj-a/x"}' KB_BEAT=0)"
if [ -z "$o" ]; then ok "KB_BEAT=0 prints nothing"; else bad "KB_BEAT=0 (got: $o)"; fi

# --- 28. non-tool events never carry paths; blocked keeps reason only ------
o="$(beat_dryrun claude prompt '{"session_id":"s-wt","cwd":"/tmp/proj-a","tool_name":"Edit","tool_input":{"file_path":"/tmp/proj-a/x"}}')"
if [ "$(jget "$o" '.detail')" = "null" ]; then
  ok "a non-tool event never carries paths"
else
  bad "non-tool event paths (got: $o)"
fi
o="$(beat_dryrun claude blocked '{"session_id":"s-wt","cwd":"/tmp/proj-a","notification_type":"elicitation_dialog","message":"Claude Code needs your input","tool_name":"Edit","tool_input":{"file_path":"/tmp/proj-a/x"}}')"
if [ "$(jget "$o" '.detail | keys | join(",")')" = "reason" ]; then
  ok "blocked keeps detail.reason only"
else
  bad "blocked detail (got: $o)"
fi

# --- 29. malformed JSON still exits 0 with nothing --------------------------
o="$(printf '%s' '{not json' | env KB_BEAT_DRYRUN=1 "$BEAT" claude tool 2>/dev/null)"; rc=$?
if [ "$rc" -eq 0 ] && [ -z "$o" ]; then ok "malformed JSON still exits 0"; else bad "malformed JSON (rc=$rc, got: $o)"; fi
o="$(printf '%s' '{"session_id":"s-wt","cwd":"/tmp/proj-a","tool_name":"Edit","tool_input":"oops"}' | env KB_BEAT_DRYRUN=1 "$BEAT" claude tool 2>/dev/null)"; rc=$?
if [ "$rc" -eq 0 ] && [ "$(jget "$o" '.event')" = "tool" ] && [ "$(jget "$o" '.detail.paths')" = "null" ]; then
  ok "a non-object tool_input degrades to a path-less beat"
else
  bad "non-object tool_input (rc=$rc, got: $o)"
fi

# --- 30. repo identity: worktree root + MAIN checkout project, cached ------
if command -v git >/dev/null 2>&1; then
  REPO="$TMPROOT/mainrepo"
  mkdir -p "$REPO/sub"
  git -C "$REPO" init -q 2>/dev/null
  git -C "$REPO" -c user.name=t -c user.email=t@example.invalid commit -q --allow-empty -m init 2>/dev/null
  git -C "$REPO" worktree add -q "$TMPROOT/wt-feature" -b feature 2>/dev/null
  REPO_REAL="$(cd "$REPO" && pwd -P)"
  WT_REAL="$(cd "$TMPROOT/wt-feature" && pwd -P)"
  o="$(wt Edit "$REPO_REAL/sub" "{\"file_path\":\"$REPO_REAL/sub/a.rs\"}")"
  if [ "$(jget "$o" '.detail.root')" = "$REPO_REAL" ] && [ "$(jget "$o" '.detail.project')" = "mainrepo" ]; then
    ok "git checkout: root = worktree top-level, project = main checkout basename"
  else
    bad "git root/project (got: $o)"
  fi
  o="$(beat_dryrun claude tool "{\"session_id\":\"s-wt2\",\"cwd\":\"$WT_REAL\",\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"$WT_REAL/b.rs\"}}")"
  if [ "$(jget "$o" '.detail.root')" = "$WT_REAL" ] && [ "$(jget "$o" '.detail.project')" = "mainrepo" ]; then
    ok "linked worktree: root = the worktree, project = the MAIN checkout's basename"
  else
    bad "worktree root/project (got: $o)"
  fi
  # cached: wreck the repo; the second beat for the same (session, cwd) must
  # still answer from the cache file rather than re-running git.
  rm -rf "$REPO_REAL/.git"
  o="$(wt Edit "$REPO_REAL/sub" "{\"file_path\":\"$REPO_REAL/sub/a.rs\"}")"
  if [ "$(jget "$o" '.detail.project')" = "mainrepo" ] && [ "$(jget "$o" '.detail.root')" = "$REPO_REAL" ]; then
    ok "repo identity is cached per (session, cwd), not recomputed per beat"
  else
    bad "repo identity cache (got: $o)"
  fi
else
  ok "git not installed: repo-identity checks skipped"
fi

echo
echo "== OL IDEA1: blocked on you =="

# marker_path <session_id> — mirrors kb-beat.sh's beat-blocked-<hook_sid_key>.
# shellcheck disable=SC1091
. "$HOOKS_DIR/kb-hook-lib.sh"
marker_path() { printf '%s/kb/beat-blocked-%s' "$XDG_CACHE_HOME" "$(hook_sid_key "$1")"; }

# --- 30. the Notification fixture: which texts block, reason, tool, marker --
FIXTURE="$SCRIPT_DIR/fixtures/claude-notifications.json"
ncases="$(jq '.cases | length' "$FIXTURE")"
if [ "${ncases:-0}" -ge 20 ]; then
  ok "notification fixture has $ncases cases"
else
  bad "notification fixture too small (${ncases:-0})"
fi
i=0
while [ "$i" -lt "${ncases:-0}" ]; do
  cname="$(jq -r ".cases[$i].case" "$FIXTURE")"
  charness="$(jq -r ".cases[$i].harness" "$FIXTURE")"
  cpayload="$(jq -c ".cases[$i].payload" "$FIXTURE")"
  csid="$(jq -r ".cases[$i].payload.session_id" "$FIXTURE")"
  cout="$(beat_dryrun "$charness" blocked "$cpayload")"
  fail=""
  if [ "$(jq -r ".cases[$i].expect.beat" "$FIXTURE")" = "true" ]; then
    [ "$(jget "$cout" '.event')" = "blocked" ] || fail="no blocked beat"
    want_reason="$(jq -r ".cases[$i].expect.reason // empty" "$FIXTURE")"
    if [ -n "$want_reason" ] && [ "$(jget "$cout" '.detail.reason')" != "$want_reason" ]; then fail="reason"; fi
    want_len="$(jq -r ".cases[$i].expect.reason_len // empty" "$FIXTURE")"
    if [ -n "$want_len" ] && [ "$(printf '%s' "$cout" | jq -r '.detail.reason | length')" != "$want_len" ]; then fail="reason length"; fi
    want_tool="$(jq -r ".cases[$i].expect.tool // empty" "$FIXTURE")"
    if [ -n "$want_tool" ]; then
      [ "$(jget "$cout" '.detail.tool')" = "$want_tool" ] || fail="tool"
    else
      printf '%s' "$cout" | jq -e '.detail | has("tool")' >/dev/null 2>&1 && fail="unexpected tool"
    fi
    mp="$(marker_path "$csid")"
    if [ "$charness" = "claude" ]; then
      want_marker="$(jq -r ".cases[$i].expect.marker // empty" "$FIXTURE")"
      if [ ! -f "$mp" ] || [ "$(cat "$mp")" != "$want_marker" ]; then fail="marker"; fi
    else
      [ ! -e "$mp" ] || fail="unexpected marker"
    fi
  else
    [ -z "$cout" ] || fail="a beat was sent: $cout"
    [ ! -e "$(marker_path "$csid")" ] || fail="unexpected marker"
  fi
  if [ -z "$fail" ]; then ok "fixture: $cname"; else bad "fixture: $cname ($fail)"; fi
  i=$((i + 1))
done

# --- 31. the blocked beat carries detail.tool (existing case 6 shape) -------
if [ "$(jget "$out6" '.detail.tool')" = "Bash" ]; then
  ok "blocked beat carries detail.tool=Bash"
else
  bad "blocked beat carries detail.tool (got: $out6)"
fi

# --- 32. throttle: the first PostToolUse of the blocked agent emits unblocked
tb() { # tb <session_id> [agent_id] [env...]
  local sid="$1" agent="${2:-}"
  shift 2 2>/dev/null || shift $#
  local p="{\"session_id\":\"$sid\",\"cwd\":\"/tmp/p\""
  [ -z "$agent" ] || p="$p,\"agent_id\":\"$agent\""
  printf '%s}' "$p" | env KB_BEAT_DRYRUN=1 "$@" "$BEAT_THROTTLE" claude 2>/dev/null
}
bl() { # bl <session_id> [agent_id]
  local p="{\"session_id\":\"$1\",\"cwd\":\"/tmp/p\",\"notification_type\":\"permission_prompt\",\"message\":\"Claude needs your permission to use Bash\""
  [ -z "${2:-}" ] || p="$p,\"agent_id\":\"$2\""
  beat_dryrun claude blocked "$p}" >/dev/null
}
bl s-ub1
o="$(tb s-ub1)"
if [ "$(jget "$o" '.event')" = "unblocked" ] && [ "$(printf '%s\n' "$o" | grep -c '"event"')" = "1" ] \
   && [ ! -e "$(marker_path s-ub1)" ]; then
  ok "blocked(main) then PostToolUse emits exactly one unblocked and clears the marker"
else
  bad "unblocked on the first PostToolUse (got: $o)"
fi
o="$(tb s-ub1)"
if [ -z "$o" ]; then ok "a second PostToolUse inside the interval prints nothing"; else bad "second PostToolUse (got: $o)"; fi

# subagent: another agent's tool does not resolve the block.
bl s-ub2 a1
mkdir -p "$XDG_CACHE_HOME/kb" && touch "$XDG_CACHE_HOME/kb/beat-heartbeat-$(hook_sid_key s-ub2)"
o="$(tb s-ub2 a2)"
if [ -z "$o" ] && [ -f "$(marker_path s-ub2)" ]; then
  ok "blocked(a1) then PostToolUse(a2) emits no unblocked and keeps the marker"
else
  bad "other agent must not unblock (got: $o)"
fi
o="$(tb s-ub2 a1)"
if [ "$(jget "$o" '.event')" = "unblocked" ] && [ ! -e "$(marker_path s-ub2)" ]; then
  ok "blocked(a1) then PostToolUse(a1) emits unblocked"
else
  bad "same agent unblocks (got: $o)"
fi

# lifecycle beat: KB_BEAT_HEARTBEAT=0 still emits it.
bl s-ub3
o="$(tb s-ub3 "" KB_BEAT_HEARTBEAT=0)"
if [ "$(jget "$o" '.event')" = "unblocked" ]; then
  ok "KB_BEAT_HEARTBEAT=0 still emits the unblocked lifecycle beat"
else
  bad "heartbeat kill switch must not swallow unblocked (got: $o)"
fi

# a prompt clears the marker, so a later PostToolUse emits no unblocked.
bl s-ub4
beat_dryrun claude prompt '{"session_id":"s-ub4","cwd":"/tmp/p"}' >/dev/null
touch "$XDG_CACHE_HOME/kb/beat-heartbeat-$(hook_sid_key s-ub4)"
o="$(tb s-ub4)"
if [ ! -e "$(marker_path s-ub4)" ] && [ -z "$o" ]; then
  ok "blocked then prompt removes the marker; no later unblocked"
else
  bad "prompt must clear the marker (got: $o)"
fi

# omp never emits unblocked from the throttle.
beat_dryrun omp blocked '{"session_id":"s-ub5","cwd":"/tmp/p","message":"bash: approval required"}' >/dev/null
o="$(printf '%s' '{"session_id":"s-ub5","cwd":"/tmp/p"}' | env KB_BEAT_DRYRUN=1 "$BEAT_THROTTLE" omp 2>/dev/null)"
if [ "$(jget "$o" '.event')" != "unblocked" ]; then
  ok "omp's throttle input never emits unblocked"
else
  bad "omp must not unblock via the throttle (got: $o)"
fi

# --- 33. hooks.json: Notification stays matcher-less ------------------------
if jq -e '.hooks.Notification | length == 1 and (.[0] | has("matcher") | not)
          and (.[0].hooks[0].command | endswith("kb-beat.sh\" claude blocked"))' \
     "$HOOKS_JSON" >/dev/null 2>&1; then
  ok "hooks.json: Notification group has no matcher and calls kb-beat.sh claude blocked"
else
  bad "hooks.json Notification group shape"
fi

echo "== OL PRES2: worktree basename + write-tool beats through the throttle =="

# --- 34. a linked worktree carries detail.worktree (basename); main does not -
if command -v git >/dev/null 2>&1; then
  REPO2="$TMPROOT/wtmain"
  mkdir -p "$REPO2"
  git -C "$REPO2" init -q 2>/dev/null
  git -C "$REPO2" -c user.name=t -c user.email=t@example.invalid commit -q --allow-empty -m init 2>/dev/null
  git -C "$REPO2" worktree add -q "$TMPROOT/wt-x" -b wtx 2>/dev/null
  R2="$(cd "$REPO2" && pwd -P)"
  W2="$(cd "$TMPROOT/wt-x" && pwd -P)"
  o="$(beat_dryrun claude tool "{\"session_id\":\"s-w1\",\"cwd\":\"$W2\",\"tool_name\":\"Edit\",\"tool_input\":{\"file_path\":\"$W2/a.rs\"}}")"
  if [ "$(jget "$o" '.detail.worktree')" = "wt-x" ] && [ "$(jget "$o" '.detail.project')" = "wtmain" ]; then
    ok "linked worktree beat carries detail.worktree = basename (never a path)"
  else
    bad "detail.worktree for a linked worktree (got: $o)"
  fi
  o="$(beat_dryrun claude tool "{\"session_id\":\"s-w2\",\"cwd\":\"$R2\",\"tool_name\":\"Edit\",\"tool_input\":{\"file_path\":\"$R2/a.rs\"}}")"
  if [ "$(jget "$o" '.detail | has("worktree")')" = "false" ]; then
    ok "main checkout beat carries no detail.worktree"
  else
    bad "main checkout must not carry worktree (got: $o)"
  fi
  # same-named worktree dir (basename equals project) is still a worktree
  mkdir -p "$TMPROOT/same"
  git -C "$REPO2" worktree add -q "$TMPROOT/same/wtmain" -b same 2>/dev/null
  S2="$(cd "$TMPROOT/same/wtmain" && pwd -P)"
  o="$(beat_dryrun claude tool "{\"session_id\":\"s-w3\",\"cwd\":\"$S2\",\"tool_name\":\"Edit\",\"tool_input\":{\"file_path\":\"$S2/a.rs\"}}")"
  if [ "$(jget "$o" '.detail.worktree')" = "wtmain" ]; then
    ok "a worktree whose directory is named like the project is still reported"
  else
    bad "same-named worktree (got: $o)"
  fi
else
  ok "git not installed: worktree checks skipped"
fi

# --- 35. write tools bypass the throttle gate; other tools stay throttled ---
tw() { # tw <session> <tool_name> [env...]
  local sid="$1" tool="$2"
  shift 2
  printf '{"session_id":"%s","cwd":"/tmp/proj-a","tool_name":"%s","tool_input":{"file_path":"/tmp/proj-a/x.rs"}}' "$sid" "$tool" \
    | env KB_BEAT_DRYRUN=1 KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS=180 "$@" "$BEAT_THROTTLE" claude 2>/dev/null
}
tw s-gate Read >/dev/null   # first call opens the gate window
o="$(tw s-gate Read)"
if [ -z "$o" ]; then ok "a Read inside the interval stays throttled"; else bad "Read must stay throttled (got: $o)"; fi
o="$(tw s-gate Bash)"
if [ -z "$o" ]; then ok "a Bash call inside the interval stays throttled"; else bad "Bash must stay throttled (got: $o)"; fi
for t in Edit Write MultiEdit NotebookEdit; do
  o="$(tw s-gate "$t")"
  if [ "$(jget "$o" '.event')" = "tool" ] && [ "$(jget "$o" '.detail.tool')" = "$t" ] \
     && [ "$(jget "$o" '.detail.paths | length')" = "1" ]; then
    ok "$t passes the throttle gate and carries its path"
  else
    bad "$t must bypass the throttle (got: $o)"
  fi
done
o="$(tw s-gate2 Edit KB_BEAT_HEARTBEAT=0)"
if [ "$(jget "$o" '.detail.tool')" = "Edit" ]; then
  ok "KB_BEAT_HEARTBEAT=0 kills the heartbeat only, not the write-tool presence beat"
else
  bad "KB_BEAT_HEARTBEAT=0 must keep write-tool beats (got: $o)"
fi
o="$(tw s-gate3 Read KB_BEAT_HEARTBEAT=0)"
if [ -z "$o" ]; then ok "KB_BEAT_HEARTBEAT=0 still silences a non-write tool"; else bad "heartbeat off (got: $o)"; fi
o="$(tw s-gate4 Edit KB_BEAT=0)"
if [ -z "$o" ]; then ok "KB_BEAT=0 silences write-tool beats too"; else bad "KB_BEAT=0 (got: $o)"; fi
tw s-gate5 Read >/dev/null
o="$(tw s-gate5 Edit KB_BEAT_PATHS=0)"
if [ -z "$o" ]; then
  ok "KB_BEAT_PATHS=0 leaves a write tool throttled like any other tool"
else
  bad "KB_BEAT_PATHS=0 must not bypass the gate (got: $o)"
fi

echo
echo "== OL IDEA4: kb-beat-pretool.sh (deduped tool-START beat) =="

PRETOOL="$HOOKS_DIR/kb-beat-pretool.sh"
. "$HOOKS_DIR/kb-hook-lib.sh"

# pt <payload-json> [env assignments...] -> stdout of one dry-run run
pt() {
  local payload="$1"
  shift
  printf '%s' "$payload" | env KB_BEAT_DRYRUN=1 "$@" "$PRETOOL" claude 2>/dev/null
}
ptp() { # ptp <sid> <tool> [extra-json-members]
  jq -nc --arg sid "$1" --arg t "$2" '{session_id:$sid, cwd:"/tmp/p", tool_name:$t,
    tool_input:{command:"SECRET-COMMAND-MARKER"}}'
}

# --- 34. executable; first call sends event=tool + detail.tool/phase --------
if [ -x "$PRETOOL" ]; then ok "kb-beat-pretool.sh is executable"; else bad "kb-beat-pretool.sh must be executable (-x)"; fi
o="$(pt "$(ptp s-pt1 Bash)")"
if [ "$(jget "$o" '.event')" = "tool" ] && [ "$(jget "$o" '.detail.tool')" = "Bash" ] \
   && [ "$(jget "$o" '.detail.phase')" = "start" ] && [ "$(jget "$o" '.session_id')" = "s-pt1" ]; then
  ok "first Bash call sends event=tool, detail.tool=Bash, detail.phase=start"
else
  bad "first Bash call (got: $o)"
fi
case "$o" in
  *SECRET-COMMAND-MARKER*) bad "the beat body must never carry tool_input (got: $o)" ;;
  *) ok "the body carries the tool name only, never tool_input" ;;
esac
if printf '%s' "$o" | jq -e '.detail | has("paths") or has("root") or has("project")' >/dev/null 2>&1; then
  bad "a tool-start beat must not carry paths/root/project (got: $o)"
else
  ok "a tool-start beat carries no paths/root/project even for a write tool name"
fi
o="$(pt "$(ptp s-pt1w Edit)")"
if [ "$(jget "$o" '.detail.tool')" = "Edit" ] && [ "$(jget "$o" '.detail.paths')" = "null" ]; then
  ok "an Edit tool-start beat names the tool and extracts no paths"
else
  bad "Edit tool-start beat (got: $o)"
fi

# --- 35. dedupe: repeat within the interval silent; a name change sends -----
o="$(pt "$(ptp s-pt1 Bash)")"
if [ -z "$o" ]; then ok "a repeat Bash call within the interval is silent"; else bad "repeat Bash call (got: $o)"; fi
o="$(pt "$(ptp s-pt1 Read)")"
if [ "$(jget "$o" '.detail.tool')" = "Read" ]; then ok "Read (a name change) sends"; else bad "Read name change (got: $o)"; fi
o="$(pt "$(ptp s-pt1 Read)")"
if [ -z "$o" ]; then ok "an immediate repeat of Read is silent"; else bad "repeat Read (got: $o)"; fi
o="$(pt "$(ptp s-pt1 Read)" KB_BEAT_TOOLS_MIN_INTERVAL_SECS=0)"
if [ "$(jget "$o" '.detail.tool')" = "Read" ]; then
  ok "a repeat Read with KB_BEAT_TOOLS_MIN_INTERVAL_SECS=0 sends"
else
  bad "repeat Read with interval 0 (got: $o)"
fi
# the interval is a real clock: a marker older than the interval re-sends.
touch -d '10 minutes ago' "$XDG_CACHE_HOME/kb/beat-tool-$(hook_sid_key s-pt1)" 2>/dev/null \
  || touch -t "$(date -d '10 minutes ago' +%Y%m%d%H%M.%S 2>/dev/null)" "$XDG_CACHE_HOME/kb/beat-tool-$(hook_sid_key s-pt1)" 2>/dev/null
o="$(pt "$(ptp s-pt1 Read)")"
if [ "$(jget "$o" '.detail.tool')" = "Read" ]; then
  ok "a repeat Read past the default 180s interval sends"
else
  bad "repeat Read past the interval (got: $o)"
fi

# --- 36. subagent tool calls never name the parent's last tool --------------
o="$(pt "$(jq -nc '{session_id:"s-pt2", cwd:"/tmp/p", tool_name:"Bash", agent_id:"agent-7"}')")"
if [ -z "$o" ]; then ok "a payload with a non-empty agent_id is silent"; else bad "agent_id payload (got: $o)"; fi
# an empty tool_name must not shift fields (agent_id must not become the tool).
o="$(pt "$(jq -nc '{session_id:"s-pt2", cwd:"/tmp/p", agent_id:"agent-7"}')")"
if [ -z "$o" ]; then ok "an agent_id payload with no tool_name is silent (no field shift)"; else bad "agent_id without tool_name (got: $o)"; fi

# --- 37. each kill switch is silent -----------------------------------------
for sw in KB_BEAT=0 KB_BEAT_HEARTBEAT=0 KB_BEAT_TOOLS=0; do
  o="$(pt "$(ptp "s-pt-$sw" Bash)" "$sw")"
  if [ -z "$o" ]; then ok "$sw is silent"; else bad "$sw must be silent (got: $o)"; fi
done

# --- 38. never blocks, never prints for degenerate input ---------------------
for bad_in in '' 'not json' '{}' '{"session_id":"s-pt3"}' '{"tool_name":"Bash"}' '[1,2]'; do
  o="$(printf '%s' "$bad_in" | env KB_BEAT_DRYRUN=1 "$PRETOOL" claude 2>/dev/null)"; rc=$?
  if [ "$rc" -eq 0 ] && [ -z "$o" ]; then ok "degenerate stdin ($bad_in) -> exit 0, empty stdout"; else bad "degenerate stdin $bad_in (rc=$rc, got: $o)"; fi
done
o="$(printf '%s' "$(ptp s-pt4 Bash)" | env -u KB_SESSIONS_DIR KB_BEAT_DRYRUN=1 "$PRETOOL" claude 2>/dev/null)"
if [ -z "$o" ]; then ok "no KB_SESSIONS_DIR -> silent no-op"; else bad "no KB_SESSIONS_DIR (got: $o)"; fi

# --- 39. kb-beat.sh: third argument is optional and strictly `start` --------
o="$(printf '%s' "$(ptp s-pt5 Bash)" | env KB_BEAT_DRYRUN=1 "$BEAT" claude tool 2>/dev/null)"
if [ "$(jget "$o" 'has("detail")')" = "false" ]; then
  ok "kb-beat.sh tool with no third argument still has no detail"
else
  bad "kb-beat.sh tool without phase must have no detail (got: $o)"
fi
o="$(printf '%s' "$(ptp s-pt5 Bash)" | env KB_BEAT_DRYRUN=1 "$BEAT" claude tool bogus 2>/dev/null)"
if [ "$(jget "$o" 'has("detail")')" = "false" ]; then
  ok "an unknown third argument is ignored"
else
  bad "unknown third argument must be ignored (got: $o)"
fi
o="$(printf '%s' "$(ptp s-pt5 Bash)" | env KB_BEAT_DRYRUN=1 "$BEAT" claude prompt start 2>/dev/null)"
if [ "$(jget "$o" 'has("detail")')" = "false" ]; then
  ok "a start third argument is honoured only on the tool event"
else
  bad "phase=start on a non-tool event must be ignored (got: $o)"
fi
long="$(printf 'T%.0s' $(seq 1 200))"
o="$(printf '%s' "$(ptp s-pt5 "$long")" | env KB_BEAT_DRYRUN=1 "$BEAT" claude tool start 2>/dev/null)"
if [ "$(jget "$o" '.detail.tool | length')" = "64" ]; then
  ok "detail.tool is capped at 64 bytes client-side"
else
  bad "detail.tool cap (got: $o)"
fi

# --- 40. hooks.json wiring ---------------------------------------------------
if jq -e '
     (.hooks.PreToolUse | length) == 1
     and (.hooks.PreToolUse[0] | has("matcher") | not)
     and (.hooks.PreToolUse[0].hooks | length) == 1
     and (.hooks.PreToolUse[0].hooks[0].command == "\"${CLAUDE_PLUGIN_ROOT}/hooks/kb-beat-pretool.sh\" claude")
     and (.hooks.PreToolUse[0].hooks[0].timeout == 5)
   ' "$HOOKS_JSON" >/dev/null 2>&1; then
  ok "hooks.json: a matcher-less PreToolUse group runs kb-beat-pretool.sh claude (timeout 5)"
else
  bad "hooks.json PreToolUse group shape"
fi
if jq -e '
     .hooks.PostToolUse[0] as $g
     | ($g | has("matcher") | not)
     and ($g.hooks | map(.command) == [
          "\"${CLAUDE_PLUGIN_ROOT}/hooks/kb-capture-throttle.sh\"",
          "\"${CLAUDE_PLUGIN_ROOT}/hooks/kb-beat-throttle.sh\" claude"])
   ' "$HOOKS_JSON" >/dev/null 2>&1; then
  ok "hooks.json: the matcher-less PostToolUse group still runs capture-throttle + beat-throttle"
else
  bad "hooks.json PostToolUse matcher-less group changed"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
