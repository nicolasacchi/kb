#!/usr/bin/env bash
# test-kb-code-why-codex.sh — test matrix for kb-code-why-codex.sh (the
# Codex CLI PreToolUse adapter), against the SAME mock-daemon.py fixture
# test-kb-code-why.sh uses. Only exercises the adapter's own job (apply_patch
# envelope parsing + per-path delegation + multi-block merge) — the
# provenance lookup/cache/cap/confidence-gate logic is kb-code-why.sh's, and
# is already covered by test-kb-code-why.sh.
#
# Runnable standalone: bash plugins/kb-code/hooks/tests/test-kb-code-why-codex.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
HOOK="$HOOKS_DIR/kb-code-why-codex.sh"
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
  echo "kb-code-why-codex.sh is not executable at $HOOK" >&2
  exit 1
}

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-code-why-codex-test.XXXXXX")"
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
  if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected [$2], got [$3])"; fi
}

assert_empty() {
  if [ -z "$2" ]; then ok "$1"; else bad "$1 (expected empty, got [$2])"; fi
}

assert_contains() {
  case "$2" in
  *"$3"*) ok "$1" ;;
  *) bad "$1 (expected to contain [$3], got [$2])" ;;
  esac
}

assert_status() {
  if [ "$2" -eq "$3" ]; then ok "$1"; else bad "$1 (expected exit $2, got $3)"; fi
}

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

start_mock() {
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

make_repo() {
  local dir="$1"
  mkdir -p "$dir/src"
  cat >"$dir/src/greet.rs" <<'EOF'
fn greet() {
    println!("hello");
}
EOF
  cat >"$dir/src/other.rs" <<'EOF'
fn other() {}
EOF
}

repos_json_for() {
  jq -n --arg name "$2" --arg path "$3" \
    '{repos: [{name: $name, path: $path, file_count: 2, symbol_count: 2, head: null, watcher: "watching"}]}' \
    >"$1"
}

# run_codex_hook <tool_name> <command-text> <session_id> <daemon_url>
#                 <kb_daemon_url> <xdg_cache_home> [kill_switch_value]
run_codex_hook() {
  (
    unset KB_CODE_WHY_HOOK
    export KB_CODE_DAEMON_URL="$4"
    export KB_DAEMON_URL="$5"
    export XDG_CACHE_HOME="$6"
    if [ -n "${7:-}" ]; then
      export KB_CODE_WHY_HOOK="$7"
    fi
    payload="$(jq -n --arg tn "$1" --arg cmd "$2" --arg sid "$3" \
      '{tool_name: $tn, session_id: $sid, tool_input: {command: $cmd}}')"
    printf '%s' "$payload" | "$HOOK"
  )
}

echo "== kb-code-why-codex.sh test matrix =="
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
  "repo": "fixture",
  "path": "src/greet.rs",
  "sessions": [
    {"session_id": "sess-001", "display_name": "fixed the gizmo race", "confidence": "trailer", "via": "commit-trailer", "lines": 3, "regions": 1}
  ],
  "uncommitted_lines": 0
}
EOF

FUZZY_WHY_JSON="$TMPROOT/fuzzy-why.json"
cat >"$FUZZY_WHY_JSON" <<'EOF'
{
  "repo": "fixture",
  "path": "src/greet.rs",
  "sessions": [
    {"session_id": "sess-002", "display_name": "maybe this", "confidence": "fuzzy", "via": "time-window", "lines": 1, "regions": 1}
  ],
  "uncommitted_lines": 0
}
EOF

DEAD_DAEMON="http://127.0.0.1:1"
UNUSED_KB_DAEMON="http://127.0.0.1:9"

SINGLE_FILE_PATCH="*** Begin Patch
*** Update File: $REPO/src/greet.rs
@@
-println!(\"hello\");
+println!(\"hi\");
*** End Patch"

# =======================================================================
# 1. single-file apply_patch, trailer confidence -> injection emitted
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"
out="$(run_codex_hook "apply_patch" "$SINGLE_FILE_PATCH" "sess-A" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-1")"
rc=$?
assert_status "single-file: exits 0" 0 "$rc"
assert_contains "single-file: session display_name injected" "$out" "fixed the gizmo race"
event_name="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName' 2>/dev/null)"
assert_eq "single-file: hookEventName is PreToolUse" "PreToolUse" "$event_name"
stop_mock

# =======================================================================
# 2. multi-file apply_patch -> both files delegated, blocks merged into one
#    additionalContext (same fixture answers for both paths — this test
#    only asserts BOTH file paths appear in the merged output, not that the
#    mock daemon can distinguish per-path answers).
# =======================================================================
MULTI_FILE_PATCH="*** Begin Patch
*** Update File: $REPO/src/greet.rs
@@
-println!(\"hello\");
+println!(\"hi\");
*** Add File: $REPO/src/new.rs
+fn new_fn() {}
*** Delete File: $REPO/src/other.rs
*** End Patch"
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"
out="$(run_codex_hook "apply_patch" "$MULTI_FILE_PATCH" "sess-B" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-2")"
# new.rs doesn't exist on disk (a pure Add) so kb-code-why.sh's own
# "no pre-existing file" gate skips it; other.rs (Delete) DOES exist, so
# both greet.rs and other.rs should surface an injected block.
n_blocks="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext' 2>/dev/null | grep -c "fixed the gizmo race")"
assert_eq "multi-file: two merged blocks (greet.rs + other.rs)" "2" "${n_blocks:-0}"
stop_mock

# =======================================================================
# 3. fuzzy confidence -> no blocks anywhere -> silent overall
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$FUZZY_WHY_JSON"
out="$(run_codex_hook "apply_patch" "$SINGLE_FILE_PATCH" "sess-C" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-3")"
rc=$?
assert_status "fuzzy: exits 0" 0 "$rc"
assert_empty "fuzzy: no injection" "$out"
stop_mock

# =======================================================================
# 4. daemon down -> fail-open, silent
# =======================================================================
out="$(run_codex_hook "apply_patch" "$SINGLE_FILE_PATCH" "sess-D" \
  "$DEAD_DAEMON" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-4")"
rc=$?
assert_status "daemon down: exits 0" 0 "$rc"
assert_empty "daemon down: no injection" "$out"

# =======================================================================
# 5. kill switch passthrough (kb-code-why.sh's own env var) -> silent
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"
out="$(run_codex_hook "apply_patch" "$SINGLE_FILE_PATCH" "sess-E" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-5" "off")"
rc=$?
assert_status "kill switch: exits 0" 0 "$rc"
assert_empty "kill switch: no injection" "$out"
stop_mock

# =======================================================================
# 6. non apply_patch tool_name -> silent (Codex never sends anything else
#    to this event per the matcher, but defense in depth mirrors the base
#    script's own non-Edit/Write guard)
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"
out="$(run_codex_hook "shell" "$SINGLE_FILE_PATCH" "sess-F" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-6")"
assert_empty "non apply_patch tool_name: silent" "$out"
stop_mock

# =======================================================================
# 7. no jq-parseable "*** ... File:" lines -> silent, no crash
# =======================================================================
port="$(free_port)"
start_mock "$port" "$REPOS_JSON" "$TRAILER_WHY_JSON"
out="$(run_codex_hook "apply_patch" "not a patch envelope at all" "sess-G" \
  "http://127.0.0.1:$port" "$UNUSED_KB_DAEMON" "$TMPROOT/xdg-7")"
rc=$?
assert_status "unparseable command: exits 0" 0 "$rc"
assert_empty "unparseable command: no injection" "$out"
stop_mock

echo
echo "== $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
