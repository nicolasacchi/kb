#!/usr/bin/env bash
# test-git-trailer.sh — self-contained test matrix for the kb-memory
# Kb-Session commit trailer (W0.3): git-dispatch/{dispatch,trailer-logic}.sh
# + install-git-trailer.sh + the repo-keyed marker written by
# kb-wake.sh / kb-recall.sh.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-git-trailer.sh
# No network, mktemp fixture repos only, cleans up on exit, exits
# nonzero if any assertion fails.
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DISPATCH_DIR="$HOOKS_DIR/git-dispatch"
INSTALLER="$HOOKS_DIR/install-git-trailer.sh"

command -v git >/dev/null 2>&1 || {
  echo "git not found — cannot run tests" >&2
  exit 1
}

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-trailer-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

# Isolate marker files from the real ~/.cache/kb, and never trust the
# ambient session env (this test itself very possibly runs inside a
# live Claude Code or Grok session).
export XDG_CACHE_HOME="$TMPROOT/xdg-cache"
unset CLAUDE_CODE_SESSION_ID
unset GROK_SESSION_ID

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

assert_status() {
  # assert_status <desc> <expected-exit-status> <actual-exit-status>
  if [ "$2" -eq "$3" ]; then
    ok "$1"
  else
    bad "$1 (expected exit $2, got $3)"
  fi
}

# --- fixture helpers --------------------------------------------------

new_repo() {
  local dir
  dir="$(mktemp -d "$TMPROOT/repo.XXXXXX")"
  git init -q "$dir"
  git -C "$dir" config user.email "test@example.com"
  git -C "$dir" config user.name "Test User"
  git -C "$dir" config commit.gpgsign false
  git -C "$dir" config tag.gpgsign false
  printf '%s' "$dir"
}

install_repo() {
  "$INSTALLER" "$1" >"$TMPROOT/install.log" 2>&1
}

# Same slug algorithm as git-dispatch/trailer-logic.sh + kb-wake.sh /
# kb-recall.sh — MUST stay in lockstep (see comments in those files).
kb_slugify() {
  local s
  s="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9' '-')"
  s="${s#-}"
  s="${s%-}"
  printf '%s' "$s"
}

repo_key_for() {
  local root
  root="$(git -C "$1" rev-parse --show-toplevel 2>/dev/null)"
  kb_slugify "$root"
}

# write_marker <repo> <session-id> [age-seconds]
write_marker() {
  local dir="$1" sid="$2" age="${3:-0}" key marker_dir now
  key="$(repo_key_for "$dir")"
  marker_dir="$XDG_CACHE_HOME/kb"
  mkdir -p "$marker_dir"
  now="$(date +%s)"
  printf '%s\n%s\n' "$sid" "$((now - age))" >"$marker_dir/current-session-repo-$key"
}

clear_marker() {
  local key
  key="$(repo_key_for "$1")"
  rm -f "$XDG_CACHE_HOME/kb/current-session-repo-$key"
}

trailer_of() {
  git -C "$1" log -1 --format='%(trailers:key=Kb-Session,valueonly)' 2>/dev/null
}

trailer_count() {
  trailer_of "$1" | grep -c . || true
}

# --- CT-F1 helpers: the opt-in `Kb-Memory:` trailer --------------------

mem_trailer_of() {
  git -C "$1" log -1 --format='%(trailers:key=Kb-Memory,valueonly)' 2>/dev/null
}

mem_trailer_count() {
  mem_trailer_of "$1" | grep -c . || true
}

# The hook asks the daemon for "memories minted this session" over HTTP.
# Tests must stay network-free, so a stub `curl` is put FIRST on PATH: it
# ignores every argument and prints the fixture body written by
# `stub_curl_body` (or exits non-zero when that file is absent, which is
# exactly how the hook sees an unreachable daemon).
STUB_BIN="$TMPROOT/stub-bin"
mkdir -p "$STUB_BIN"
cat >"$STUB_BIN/curl" <<EOF
#!/usr/bin/env bash
[ -f "$TMPROOT/curl-body" ] || exit 22
cat "$TMPROOT/curl-body"
EOF
chmod +x "$STUB_BIN/curl"

stub_curl_body() { printf '%s' "$1" >"$TMPROOT/curl-body"; }
stub_curl_down() { rm -f "$TMPROOT/curl-body"; }

# `{"memories":[{"id":"<id>",…},…]}` — the shape
# `GET /api/sessions/{sid}/memories` returns (routes::sessions::memories).
memories_json() {
  local out="" id
  for id in "$@"; do
    [ -n "$out" ] && out="$out,"
    out="$out{\"id\":\"$id\",\"kb\":\"notes\",\"title\":\"a memory\",\"path\":\"/srv/notes/m.html\",\"source_relative\":\"m.html\",\"mtime_unix\":1700000000}"
  done
  printf '{"memories":[%s]}' "$out"
}

# A commit with the stub curl on PATH (and a session id in the env).
commit_with_stub() {
  local dir="$1" sid="$2" msg="$3"
  CLAUDE_CODE_SESSION_ID="$sid" PATH="$STUB_BIN:$PATH" \
    git -C "$dir" commit --allow-empty -m "$msg" -q
}

opt_in_memory_trailers() { git -C "$1" config --local kb.memoryTrailers true; }

echo "== kb-memory Kb-Session trailer test matrix =="
echo "hooks dir: $HOOKS_DIR"
echo "tmp root:  $TMPROOT"
echo

# =======================================================================
# 1. env-var commit -> trailer present
# =======================================================================
r="$(new_repo)"
install_repo "$r"
CLAUDE_CODE_SESSION_ID="sess-env-1" git -C "$r" commit --allow-empty -m "t1" -q
assert_eq "env-var commit stamps trailer" "sess-env-1" "$(trailer_of "$r")"

# =======================================================================
# 1b. GROK_SESSION_ID (no Claude env) -> trailer present
# =======================================================================
r="$(new_repo)"
install_repo "$r"
GROK_SESSION_ID="sess-grok-1" env -u CLAUDE_CODE_SESSION_ID git -C "$r" commit --allow-empty -m "t1b" -q
assert_eq "GROK_SESSION_ID commit stamps trailer" "sess-grok-1" "$(trailer_of "$r")"

# =======================================================================
# 1c. both env vars set -> Claude wins
# =======================================================================
r="$(new_repo)"
install_repo "$r"
CLAUDE_CODE_SESSION_ID="sess-claude-wins" GROK_SESSION_ID="sess-grok-loses" \
  git -C "$r" commit --allow-empty -m "t1c" -q
assert_eq "CLAUDE_CODE_SESSION_ID wins over GROK_SESSION_ID" "sess-claude-wins" "$(trailer_of "$r")"

# =======================================================================
# 2. no env + fresh marker -> trailer
# =======================================================================
r="$(new_repo)"
install_repo "$r"
write_marker "$r" "sess-marker-fresh" 60 # 1 minute old
env -u CLAUDE_CODE_SESSION_ID git -C "$r" commit --allow-empty -m "t2" -q
assert_eq "fresh marker stamps trailer" "sess-marker-fresh" "$(trailer_of "$r")"

# =======================================================================
# 3. no env + stale marker -> none
# =======================================================================
r="$(new_repo)"
install_repo "$r"
write_marker "$r" "sess-marker-stale" 3000 # 50 minutes old (> 40 min gate)
env -u CLAUDE_CODE_SESSION_ID git -C "$r" commit --allow-empty -m "t3" -q
assert_empty "stale marker does NOT stamp a trailer" "$(trailer_of "$r")"

# =======================================================================
# 4. no env + no marker -> none
# =======================================================================
r="$(new_repo)"
install_repo "$r"
clear_marker "$r"
env -u CLAUDE_CODE_SESSION_ID git -C "$r" commit --allow-empty -m "t4" -q
assert_empty "no marker, no env -> no trailer" "$(trailer_of "$r")"

# =======================================================================
# 5. rebase in progress -> skipped (both rebase-merge and rebase-apply)
# =======================================================================
r="$(new_repo)"
install_repo "$r"
rp="$(cd "$r" && git rev-parse --git-path rebase-merge)"
(cd "$r" && mkdir -p "$rp")
CLAUDE_CODE_SESSION_ID="sess-during-rebase" git -C "$r" commit --allow-empty -m "t5a" -q
assert_empty "rebase-merge in progress -> no trailer" "$(trailer_of "$r")"
(cd "$r" && rmdir "$rp")

rp="$(cd "$r" && git rev-parse --git-path rebase-apply)"
(cd "$r" && mkdir -p "$rp")
CLAUDE_CODE_SESSION_ID="sess-during-rebase-2" git -C "$r" commit --allow-empty -m "t5b" -q
assert_empty "rebase-apply in progress -> no trailer" "$(trailer_of "$r")"
(cd "$r" && rmdir "$rp")

# =======================================================================
# 6. amend with different id in env -> two trailers (set-valued)
# =======================================================================
r="$(new_repo)"
install_repo "$r"
CLAUDE_CODE_SESSION_ID="sess-A" git -C "$r" commit --allow-empty -m "t6 base" -q
CLAUDE_CODE_SESSION_ID="sess-B" git -C "$r" commit --amend --no-edit --allow-empty -q
trailers="$(trailer_of "$r")"
assert_eq "amend w/ different id -> 2 trailers" "2" "$(trailer_count "$r")"
case "$trailers" in
*sess-A*sess-B* | *sess-B*sess-A*) ok "amend w/ different id -> both ids present" ;;
*) bad "amend w/ different id -> both ids present (got [$trailers])" ;;
esac

# =======================================================================
# 7. amend with same id -> one trailer (no duplicate)
# =======================================================================
r="$(new_repo)"
install_repo "$r"
CLAUDE_CODE_SESSION_ID="sess-C" git -C "$r" commit --allow-empty -m "t7 base" -q
CLAUDE_CODE_SESSION_ID="sess-C" git -C "$r" commit --amend --no-edit --allow-empty -q
assert_eq "amend w/ same id -> 1 trailer" "1" "$(trailer_count "$r")"
assert_eq "amend w/ same id -> id unchanged" "sess-C" "$(trailer_of "$r")"

# =======================================================================
# 8. pre-existing executable pre-commit + prepare-commit-msg hooks in
#    .git/hooks still run (chaining), AND a failing pre-commit aborts.
# =======================================================================
r="$(new_repo)"
install_repo "$r"
# --git-common-dir is printed relative to $r; resolve to an absolute path
# (worktree-correct — this is also how dispatch.sh finds it at runtime).
hooks_real_dir="$(git -C "$r" rev-parse --git-common-dir)"
hooks_real_dir="$(cd "$r" && cd "$hooks_real_dir" && pwd)/hooks"
mkdir -p "$hooks_real_dir"

cat >"$hooks_real_dir/pre-commit" <<EOF
#!/usr/bin/env bash
touch "$TMPROOT/precommit-ran"
exit 0
EOF
chmod +x "$hooks_real_dir/pre-commit"

cat >"$hooks_real_dir/prepare-commit-msg" <<EOF
#!/usr/bin/env bash
touch "$TMPROOT/prepare-ran"
exit 0
EOF
chmod +x "$hooks_real_dir/prepare-commit-msg"

rm -f "$TMPROOT/precommit-ran" "$TMPROOT/prepare-ran"
CLAUDE_CODE_SESSION_ID="sess-chain" git -C "$r" commit --allow-empty -m "t8 chain" -q
[ -f "$TMPROOT/precommit-ran" ] && ok "chaining: repo's own pre-commit ran" \
  || bad "chaining: repo's own pre-commit ran"
[ -f "$TMPROOT/prepare-ran" ] && ok "chaining: repo's own prepare-commit-msg ran" \
  || bad "chaining: repo's own prepare-commit-msg ran"
assert_eq "chaining: our trailer logic still ran too" "sess-chain" "$(trailer_of "$r")"

# Now make pre-commit fail -> commit must abort.
cat >"$hooks_real_dir/pre-commit" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$hooks_real_dir/pre-commit"
before_head="$(git -C "$r" rev-parse HEAD)"
set +e
CLAUDE_CODE_SESSION_ID="sess-should-not-land" git -C "$r" commit --allow-empty -m "t8 should fail" -q 2>"$TMPROOT/fail.log"
rc=$?
set -e 2>/dev/null || true
after_head="$(git -C "$r" rev-parse HEAD)"
if [ "$rc" -ne 0 ] && [ "$before_head" = "$after_head" ]; then
  ok "a failing chained pre-commit still aborts the commit"
else
  bad "a failing chained pre-commit still aborts the commit (rc=$rc, HEAD moved=$([ "$before_head" = "$after_head" ] && echo no || echo yes))"
fi

# =======================================================================
# 9. commit from a LINKED WORKTREE gets stamped
# =======================================================================
r="$(new_repo)"
install_repo "$r"
git -C "$r" commit --allow-empty -m "base for worktree" -q
wt="$TMPROOT/linked-worktree"
git -C "$r" worktree add -q -b wt-branch "$wt" >/dev/null 2>&1
CLAUDE_CODE_SESSION_ID="sess-worktree" git -C "$wt" commit --allow-empty -m "from worktree" -q
assert_eq "linked worktree commit stamps trailer" "sess-worktree" "$(trailer_of "$wt")"
git -C "$r" worktree remove --force "$wt" >/dev/null 2>&1 || true

# ========================================================================
# 10. `git commit --no-verify` still gets the trailer (prepare-commit-msg
#     runs regardless — documented git behavior, pinned here) while a
#     chained pre-commit is skipped.
# =========================================================================
r="$(new_repo)"
install_repo "$r"
git_common="$(git -C "$r" rev-parse --git-common-dir)"
git_common="$(cd "$r" && cd "$git_common" && pwd)"
mkdir -p "$git_common/hooks"
cat >"$git_common/hooks/pre-commit" <<EOF
#!/usr/bin/env bash
touch "$TMPROOT/noverify-precommit-ran"
exit 0
EOF
chmod +x "$git_common/hooks/pre-commit"
rm -f "$TMPROOT/noverify-precommit-ran"
CLAUDE_CODE_SESSION_ID="sess-noverify" git -C "$r" commit --allow-empty -m "t10" --no-verify -q
assert_eq "--no-verify commit still gets the trailer" "sess-noverify" "$(trailer_of "$r")"
[ ! -f "$TMPROOT/noverify-precommit-ran" ] && ok "--no-verify still skips the chained pre-commit" \
  || bad "--no-verify still skips the chained pre-commit"

# =======================================================================
# 11. installer: refuses a repo with a DIFFERENT existing core.hooksPath
# =======================================================================
r="$(new_repo)"
git -C "$r" config --local core.hooksPath ".husky"
set +e
"$INSTALLER" "$r" >"$TMPROOT/refuse.log" 2>&1
rc=$?
set -e 2>/dev/null || true
current="$(git -C "$r" config --local --get core.hooksPath)"
if [ "$rc" -ne 0 ] && [ "$current" = ".husky" ]; then
  ok "installer refuses a repo with a different core.hooksPath"
else
  bad "installer refuses a repo with a different core.hooksPath (rc=$rc, hooksPath=$current)"
fi

# =======================================================================
# 12. installer: idempotent install + uninstall round-trip
# =======================================================================
r="$(new_repo)"
install_repo "$r"
first="$(git -C "$r" config --local --get core.hooksPath)"
install_repo "$r"
second="$(git -C "$r" config --local --get core.hooksPath)"
assert_eq "installer is idempotent" "$first" "$second"

"$INSTALLER" --uninstall "$r" >"$TMPROOT/uninstall.log" 2>&1
after_uninstall="$(git -C "$r" config --local --get core.hooksPath 2>/dev/null || true)"
assert_empty "uninstall clears core.hooksPath" "$after_uninstall"

CLAUDE_CODE_SESSION_ID="sess-after-uninstall" git -C "$r" commit --allow-empty -m "t12 after uninstall" -q
assert_empty "after uninstall, no trailer is stamped" "$(trailer_of "$r")"

# =======================================================================
# CT-F1 — the opt-in `Kb-Memory:` trailer
# =======================================================================

# 13. DEFAULT OFF: memories exist, the daemon would answer, and still no
#     Kb-Memory trailer is stamped. This is the ruling, pinned.
r="$(new_repo)"
install_repo "$r"
stub_curl_body "$(memories_json abc123def456)"
commit_with_stub "$r" "sess-mem-off" "t13 default off"
assert_eq "default: no Kb-Memory trailer without the repo opt-in" "0" "$(mem_trailer_count "$r")"
assert_eq "default: the Kb-Session trailer is unaffected" "sess-mem-off" "$(trailer_of "$r")"

# 14. Opted in -> one trailer per memory minted this session, alongside
#     the session trailer.
r="$(new_repo)"
install_repo "$r"
opt_in_memory_trailers "$r"
stub_curl_body "$(memories_json abc123def456 0123456789ab)"
commit_with_stub "$r" "sess-mem-on" "t14 opted in"
assert_eq "opt-in: one Kb-Memory trailer per minted memory" "2" "$(mem_trailer_count "$r")"
mem="$(mem_trailer_of "$r")"
case "$mem" in
*abc123def456*) ok "opt-in: first memory id present" ;;
*) bad "opt-in: first memory id present (got [$mem])" ;;
esac
case "$mem" in
*0123456789ab*) ok "opt-in: second memory id present" ;;
*) bad "opt-in: second memory id present (got [$mem])" ;;
esac
assert_eq "opt-in: the Kb-Session trailer still lands too" "sess-mem-on" "$(trailer_of "$r")"

# 15. Amend with the SAME session: the session trailer dedupes (as
#     before) AND the memory trailers must not double up — the amend path
#     no longer short-circuits before the memory block.
r="$(new_repo)"
install_repo "$r"
opt_in_memory_trailers "$r"
stub_curl_body "$(memories_json abc123def456)"
commit_with_stub "$r" "sess-mem-amend" "t15 base"
CLAUDE_CODE_SESSION_ID="sess-mem-amend" PATH="$STUB_BIN:$PATH" \
  git -C "$r" commit --amend --no-edit --allow-empty -q
assert_eq "amend: Kb-Memory is not duplicated" "1" "$(mem_trailer_count "$r")"
assert_eq "amend: Kb-Session is still deduped" "1" "$(trailer_count "$r")"

# 16. Amend that mints a NEW memory mid-session: the set grows (the
#     trailer is set-valued, exactly like Kb-Session).
stub_curl_body "$(memories_json abc123def456 ffffffffffff)"
CLAUDE_CODE_SESSION_ID="sess-mem-amend" PATH="$STUB_BIN:$PATH" \
  git -C "$r" commit --amend --no-edit --allow-empty -q
assert_eq "amend: a newly minted memory is appended" "2" "$(mem_trailer_count "$r")"

# 17. Daemon unreachable -> the commit still lands, with the session
#     trailer and no memory trailers. Fail-open is the whole contract.
r="$(new_repo)"
install_repo "$r"
opt_in_memory_trailers "$r"
stub_curl_down
commit_with_stub "$r" "sess-mem-down" "t17 daemon down"
assert_eq "daemon down: commit still lands with its session trailer" "sess-mem-down" "$(trailer_of "$r")"
assert_eq "daemon down: no memory trailers" "0" "$(mem_trailer_count "$r")"

# 18. Only bare 12-lowercase-hex ids are stamped — a malformed id in the
#     response is dropped, never guessed at or truncated.
r="$(new_repo)"
install_repo "$r"
opt_in_memory_trailers "$r"
stub_curl_body '{"memories":[{"id":"ABC123DEF456"},{"id":"short"},{"id":"abc123def456"}]}'
commit_with_stub "$r" "sess-mem-malformed" "t18 malformed ids"
assert_eq "malformed ids are dropped, the valid one is stamped" "1" "$(mem_trailer_count "$r")"
assert_eq "the stamped id is the valid one" "abc123def456" "$(mem_trailer_of "$r")"

# 19. The per-commit cap holds (a commit message is a human artifact).
r="$(new_repo)"
install_repo "$r"
opt_in_memory_trailers "$r"
many=""
i=0
while [ "$i" -lt 25 ]; do
  many="$many $(printf 'aaaaaaaaaa%02d' "$i")"
  i=$((i + 1))
done
# shellcheck disable=SC2086
stub_curl_body "$(memories_json $many)"
commit_with_stub "$r" "sess-mem-cap" "t19 cap"
assert_eq "at most 20 Kb-Memory trailers per commit" "20" "$(mem_trailer_count "$r")"

# 20. The opt-in must be REPO-LOCAL: a value inherited from a global
#     config is deliberately not honoured (`git config --local --get`).
r="$(new_repo)"
install_repo "$r"
stub_curl_body "$(memories_json abc123def456)"
global_cfg="$TMPROOT/fake-global-gitconfig"
printf '[kb]\n\tmemoryTrailers = true\n' >"$global_cfg"
CLAUDE_CODE_SESSION_ID="sess-mem-global" PATH="$STUB_BIN:$PATH" \
  GIT_CONFIG_GLOBAL="$global_cfg" git -C "$r" commit --allow-empty -m "t20 global opt-in" -q
assert_eq "a GLOBAL opt-in does not enable memory trailers" "0" "$(mem_trailer_count "$r")"

echo
echo "== $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
