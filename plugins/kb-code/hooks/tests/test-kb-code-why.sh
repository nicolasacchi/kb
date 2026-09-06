#!/usr/bin/env bash
# test-kb-code-why.sh — self-contained test matrix for the kb-code-why.sh
# PreToolUse hook (W5.3), against a MOCK kb-code daemon (mock-daemon.py, a
# tiny stdlib http.server fixture — no live kb-code-server needed).
#
# Runnable standalone: bash plugins/kb-code/hooks/tests/test-kb-code-why.sh
# No network beyond 127.0.0.1, mktemp fixtures only, cleans up on exit,
# exits nonzero if any assertion fails.
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
HOOK="$HOOKS_DIR/kb-code-why.sh"
MOCK="$SCRIPT_DIR/mock-daemon.py"

command -v jq >/dev/null 2>&1 || {
  echo "jq not found — cannot run tests" >&2
  exit 1
}
command -v curl >/dev/null 2>&1 || {
  echo "curl not found — cannot run tests" >&2
  exit 1
}
command -v python3 >/dev/null 2>&1 || {
  echo "python3 not found — cannot run tests (mock daemon needs it)" >&2
  exit 1
}
[ -x "$HOOK" ] || {
  echo "kb-code-why.sh is not executable at $HOOK" >&2
  exit 1
}

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-code-why-test.XXXXXX")"
MOCK_PID=""
cleanup() {
  [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null
  wait "$MOCK_PID" 2>/dev/null || true
  rm -rf "$TMPROOT"
}
trap cleanup EXIT

PASS=0
FAIL=0

ok() {
  PASS=$((PASS + 1))
  printf 'ok      - %s\n' "$1"
}

bad() {
  FAIL=$((FAIL + 1))
  printf 'FAIL    - %s\n' "$1"
}

assert_eq() {
  # assert_eq <desc> <expected> <actual>
  if [ "$2" = "$3" ]; then
    ok "$1"
  else
    bad "$1 (expected [$2], got [$3])"
  fi
}

assert_empty() {
  # assert_empty <desc> <actual>
  if [ -z "$2" ]; then
    ok "$1"
  else
    bad "$1 (expected empty, got [$2])"
  fi
}

assert_contains() {
  # assert_contains <desc> <haystack> <needle>
  case "$2" in
  *"$3"*) ok "$1" ;;
  *) bad "$1 (expected to contain [$3], got [$2])" ;;
  esac
}

assert_status() {
  # assert_status <desc> <expected-exit-status> <actual-exit-status>
  if [ "$2" -eq "$3" ]; then
    ok "$1"
  else
    bad "$1 (expected exit $2, got $3)"
  fi
}

# --- fixture helpers ---------------------------------------------------

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

start_mock() {
  # start_mock <port> <repos.json> <why.json>
  python3 "$MOCK" "$1" "$2" "$3" >"$TMPROOT/mock.log" 2>&1 &
  MOCK_PID=$!
  local i=0
  while [ "$i" -lt 50 ]; do
    curl -fsS "http://127.0.0.1:$1/api/repos" >/dev/null 2>&1 && return 0
    sleep 0.1
    i=$((i + 1))
  done
  echo "mock daemon on port $1 failed to become ready" >&2
  return 1
}

stop_mock() {
  [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null
  wait "$MOCK_PID" 2>/dev/null || true
  MOCK_PID=""
}

# repo_dir <name> — a plain (non-git — the hook never shells to git) dir
# with a small file, longest-prefix-matchable against fixture repos.json.
make_repo() {
  local dir="$1"
  mkdir -p "$dir/src"
  cat >"$dir/src/greet.rs" <<'EOF'
fn greet() {
    println!("hello");
}
EOF
}

repos_json_for() {
  # repos_json_for <out-file> <name> <path>
  jq -n --arg name "$2" --arg path "$3" \
    '{repos: [{name: $name, path: $path, file_count: 1, symbol_count: 1, head: null, watcher: "watching"}]}' \
    >"$1"
}

# run_hook <tool_name> <file_path> <old_string-or-empty> <session_id>
run_hook() {
  local payload
  if [ -n "$3" ]; then
    payload="$(jq -n --arg tn "$1" --arg fp "$2" --arg os "$3" --arg sid "$4" \
      '{tool_name: $tn, session_id: $sid, tool_input: {file_path: $fp, old_string: $os, new_string: "x"}}')"
  else
    payload="$(jq -n --arg tn "$1" --arg fp "$2" --arg sid "$4" \
      '{tool_name: $tn, session_id: $sid, tool_input: {file_path: $fp, content: "x"}}')"
  fi
  printf '%s' "$payload" | "$HOOK"
}

# run_hook_env <tool_name> <file_path> <old_string> <session_id> <daemon_url>
#              <kb_daemon_url> <xdg_cache_home> [kill_switch_value]
# `env -u` can't invoke a shell function, so isolation is a subshell instead
# (command substitution already runs in one — the exports never leak out).
run_hook_env() {
  (
    unset KB_CODE_WHY_HOOK
    export KB_CODE_DAEMON_URL="$5"
    export KB_DAEMON_URL="$6"
    export XDG_CACHE_HOME="$7"
    if [ -n "${8:-}" ]; then
      export KB_CODE_WHY_HOOK="$8"
    fi
    run_hook "$1" "$2" "$3" "$4"
  )
}

echo "== kb-code-why.sh test matrix =="
echo "hook: $HOOK"
echo "tmp:  $TMPROOT"
echo

REPO="$TMPROOT/repo"
make_repo "$REPO"
REPOS_JSON="$TMPROOT/repos.json"
repos_json_for "$REPOS_JSON" "fixture" "$REPO"

TRAILER_WHY_JSON="$TMPROOT/trailer-why.json"
cat >"$TRAILER_WHY_JSON" <<'EOF'
{
  "line": 2,
  "region": {"sha": "abc123def456", "subject": "add greet", "author": "Test", "author_time": 1700000000},
  "attribution": {"confidence": "trailer", "via": "commit-trailer", "session_id": "sess-001", "display_name": "fixed the gizmo race"},
  "kb_context": {"decisions": [{"kind": "decision", "prompt": "use a commit trailer for provenance"}], "prompt_excerpt": "excerpt"},
  "timeline_available": true
}
EOF

FUZZY_WHY_JSON="$TMPROOT/fuzzy-why.json"
cat >"$FUZZY_WHY_JSON" <<'EOF'
{
  "line": 2,
  "region": {"sha": "abc123def456", "subject": "add greet", "author": "Test", "author_time": 1700000000},
  "attribution": {"confidence": "fuzzy", "via": "time-window", "session_id": "sess-002"},
  "timeline_available": true
}
EOF

TRAILER_WHY_WITH_KB_JSON="$TMPROOT/trailer-why-with-kb.json"
cat >"$TRAILER_WHY_WITH_KB_JSON" <<'EOF'
{
  "line": 2,
  "region": {"sha": "abc123def456", "subject": "add greet", "author": "Test", "author_time": 1700000000},
  "attribution": {"confidence": "trailer", "via": "commit-trailer", "session_id": "sess-001", "display_name": "fixed the gizmo race", "kb": "memory"},
  "timeline_available": true
}
EOF

FILE_WHY_JSON="$TMPROOT/file-why.json"
cat >"$FILE_WHY_JSON" <<'EOF'
{
  "repo": "fixture",
  "path": "src/greet.rs",
  "sessions": [
    {"session_id": "sess-003", "display_name": "wrote the whole file", "confidence": "exact", "via": "by-commit", "lines": 3, "regions": 1}
  ],
  "uncommitted_lines": 0
}
EOF

DEAD_DAEMON="http://127.0.0.1:1"
UNUSED_KB_DAEMON="http://127.0.0.1:9"

# =======================================================================
# 1. trailer confidence -> injection emitted, correct shape
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"

out="$(run_hook_env "Edit" "$REPO/src/greet.rs" 'println!("hello");' "sess-A" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-1")"
rc=$?
assert_status "trailer: hook exits 0" 0 "$rc"
assert_contains "trailer: session display_name injected" "$out" "fixed the gizmo race"
assert_contains "trailer: decision injected" "$out" "use a commit trailer for provenance"
assert_contains "trailer: kb session link injected" "$out" "$UNUSED_KB_DAEMON/sessions?focus=sess-001"
event_name="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName' 2>/dev/null)"
assert_eq "trailer: hookEventName is PreToolUse" "PreToolUse" "$event_name"

# --- dedupe: same (repo, path) in the same session -> silent the 2nd time
out2="$(run_hook_env "Edit" "$REPO/src/greet.rs" 'println!("hello");' "sess-A" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-1")"
assert_empty "dedupe: repeat (repo,path) in same session is silent" "$out2"
seen_file="$TMPROOT/xdg-1/kb-code/why-hook-seen-sess-A"
seen_count="$(wc -l <"$seen_file" 2>/dev/null | tr -d '[:space:]')"
assert_eq "dedupe: seen-file has exactly 1 line, not 2" "1" "${seen_count:-0}"

stop_mock

# =======================================================================
# 2. fuzzy confidence -> always silent (never fuzzy, per the design)
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$FUZZY_WHY_JSON"
out="$(run_hook_env "Edit" "$REPO/src/greet.rs" 'println!("hello");' "sess-B" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-2")"
rc=$?
assert_status "fuzzy: hook still exits 0" 0 "$rc"
assert_empty "fuzzy: no injection" "$out"
stop_mock

# =======================================================================
# 3. daemon down -> silence + exit 0 (fail-open)
# =======================================================================
out="$(run_hook_env "Edit" "$REPO/src/greet.rs" 'println!("hello");' "sess-C" \
  "$DEAD_DAEMON" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-3")"
rc=$?
assert_status "daemon down: hook exits 0" 0 "$rc"
assert_empty "daemon down: no injection" "$out"

# =======================================================================
# 4. kill switch -> silent even with a live trailer-confidence daemon
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"
out="$(run_hook_env "Edit" "$REPO/src/greet.rs" 'println!("hello");' "sess-D" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-4" "off")"
rc=$?
assert_status "kill switch: hook exits 0" 0 "$rc"
assert_empty "kill switch: no injection despite a hit-shaped daemon" "$out"
[ ! -e "$TMPROOT/xdg-4/kb-code/why-hook-seen-sess-D" ] &&
  ok "kill switch: no state file written" ||
  bad "kill switch: no state file written"
stop_mock

# =======================================================================
# 5. global cap: 3 injections per session, 4th is silent
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"
for f in a b c d; do
  cp "$REPO/src/greet.rs" "$REPO/src/$f.rs"
done
n_hits=0
for f in a b c d; do
  out="$(run_hook_env "Edit" "$REPO/src/$f.rs" 'println!("hello");' "sess-E" \
    "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-5")"
  [ -n "$out" ] && n_hits=$((n_hits + 1))
done
assert_eq "cap: exactly 3 of 4 edits inject (global cap = 3/session)" "3" "$n_hits"
cap_seen_count="$(wc -l <"$TMPROOT/xdg-5/kb-code/why-hook-seen-sess-E" 2>/dev/null | tr -d '[:space:]')"
assert_eq "cap: seen-file stops growing at the cap" "3" "${cap_seen_count:-0}"
stop_mock

# =======================================================================
# 6. Write tool, file-grade why (no line) -> still injects on exact/trailer
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$FILE_WHY_JSON"
out="$(run_hook_env "Write" "$REPO/src/greet.rs" "" "sess-F" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-6")"
assert_contains "Write/file-grade: session display_name injected" "$out" "wrote the whole file"
stop_mock

# =======================================================================
# 7. a non Edit/Write tool -> silent (matcher-shaped defense in depth)
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"
out="$(run_hook_env "Bash" "$REPO/src/greet.rs" "" "sess-G" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-7")"
assert_empty "non Edit/Write tool_name: silent" "$out"
stop_mock

# =======================================================================
# 8. attribution.kb resolved -> kb session link is scoped with ?kb=
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_WITH_KB_JSON"
out="$(run_hook_env "Edit" "$REPO/src/greet.rs" 'println!("hello");' "sess-H" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-8")"
assert_contains "attribution.kb: kb session link scoped with ?kb=" "$out" \
  "$UNUSED_KB_DAEMON/sessions?focus=sess-001&kb=memory"
stop_mock

echo
echo "== $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
