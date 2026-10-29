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
if [ "$elapsed_ms" -lt 1500 ]; then
  ok "daemon unreachable -> script returns fast ($elapsed_ms ms, backgrounded not blocking)"
else
  bad "daemon unreachable -> script returns fast (took ${elapsed_ms}ms, expected < 1500ms)"
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

# --- 21. hooks.json: a matcher group runs kb-beat.sh claude tool directly --
if jq -e '
     .hooks.PostToolUse as $g
     | ($g | length) == 2
     and ($g[0].matcher == null)
     and ($g[0].hooks | map(.command) == [
          "\"${CLAUDE_PLUGIN_ROOT}/hooks/kb-capture-throttle.sh\"",
          "\"${CLAUDE_PLUGIN_ROOT}/hooks/kb-beat-throttle.sh\" claude"])
     and ($g[1].matcher == "Edit|Write|MultiEdit|NotebookEdit")
     and ($g[1].hooks | length == 1)
     and ($g[1].hooks[0].command == "\"${CLAUDE_PLUGIN_ROOT}/hooks/kb-beat.sh\" claude tool")
     and ($g[1].hooks[0].timeout == 5)
   ' "$HOOKS_JSON" >/dev/null 2>&1; then
  ok "hooks.json: matcher group calls kb-beat.sh (not the throttle); original PostToolUse group unchanged"
else
  bad "hooks.json PostToolUse matcher group shape"
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
o="$(beat_dryrun claude blocked '{"session_id":"s-wt","cwd":"/tmp/proj-a","message":"needs input","tool_name":"Edit","tool_input":{"file_path":"/tmp/proj-a/x"}}')"
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
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
