#!/usr/bin/env bash
# First-run smoke: install a PUBLISHED kb release exactly like a stranger would
# (scripts/install.sh), start the daemon on a spare loopback port against a
# sample corpus, and prove the first-run path works end to end: SPA served from
# <bin>/../share/kb/web/dist with no env var, keyword search finds a seeded doc,
# the file watcher picks up a newly written file, `kb daemon doctor` is healthy.
#
# One source of truth: the post-release workflow (.github/workflows/first-run.yml)
# runs this in every distro container, and scripts/ci/first-run-selftest.sh runs
# it against a fake release mirror + fake `kb` on every PR.
#
# Environment:
#   KB_VERSION            required. Release tag or bare version (v0.44 / 0.44).
#   KB_BASE_URL           optional. file:// or http mirror (selftest only).
#   LEG                   label printed in the RESULT line (default: local).
#   PREFIX                install root (default: a fresh temp dir).
#   CORPUS_DIR            sample corpus (default: $PREFIX/share/kb/sample-corpus).
#   SEARCH_TERM           term the sample corpus must match (default: borrow).
#   PORT                  daemon port (default: random 20000-59999; never 4000).
#   REQUIRE_PROVENANCE=1  fail unless install.sh printed "provenance OK".
#   DAEMON_WAIT / INDEX_WAIT / WATCH_WAIT   seconds (defaults 60 / 90 / 60).
#   SMOKE_PIDFILE         optional file that receives the daemon PID (selftest).
#   KEEP=1                keep temp dirs for debugging.
# Never sets the installer's insecure-skip override; an unverifiable download
# must fail this leg.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
install_sh="${INSTALL_SH:-$here/../install.sh}"
leg="${LEG:-local}"
total=8
n=0
daemon_pid=""
tmp="$(mktemp -d)"
prefix="${PREFIX:-$tmp/prefix}"
port="${PORT:-$((20000 + RANDOM % 40000))}"
if [ "$port" = "4000" ]; then port=4317; fi
url="http://127.0.0.1:$port"
log="$tmp/daemon.log"
term="${SEARCH_TERM:-borrow}"
daemon_wait="${DAEMON_WAIT:-60}"
index_wait="${INDEX_WAIT:-90}"
watch_wait="${WATCH_WAIT:-60}"
unset KB_INSECURE_SKIP_VERIFY KB_SPA_DIST

begin() { n=$((n + 1)); name="$1"; }
ok()    { printf 'STEP %s/%s %s ... ok\n' "$n" "$total" "$name"; }
fail()  {
  printf 'STEP %s/%s %s ... FAIL: %s\n' "$n" "$total" "$name" "$*"
  exit 1
}
tail_log() {
  if [ -s "$log" ]; then
    echo "---- daemon log (last 60 lines) ----"
    tail -n 60 "$log"
    echo "---- end daemon log ----"
  fi
}

teardown() {
  # Never pkill by name: only the PID we recorded. Failures here are warnings.
  if [ -n "$daemon_pid" ] && kill -0 "$daemon_pid" 2>/dev/null; then
    if [ -x "$prefix/bin/kb" ]; then
      "$prefix/bin/kb" daemon stop >/dev/null 2>&1 || true
    fi
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      kill -0 "$daemon_pid" 2>/dev/null || break
      sleep 1
    done
    if kill -0 "$daemon_pid" 2>/dev/null; then
      kill "$daemon_pid" 2>/dev/null || true
      sleep 2
    fi
    if kill -0 "$daemon_pid" 2>/dev/null; then
      kill -9 "$daemon_pid" 2>/dev/null || true
    fi
    if kill -0 "$daemon_pid" 2>/dev/null; then
      echo "warning: daemon pid $daemon_pid is still alive after teardown" >&2
    fi
  fi
  if [ "${KEEP:-}" != "1" ]; then rm -rf "$tmp"; fi
}
on_exit() {
  rc=$?
  teardown
  if [ "$rc" -eq 0 ]; then
    echo "RESULT $leg PASS"
  else
    echo "RESULT $leg FAIL"
  fi
  exit "$rc"
}
trap on_exit EXIT
trap 'exit 130' INT TERM

[ -n "${KB_VERSION:-}" ] || { echo "first-run-smoke: KB_VERSION is required" >&2; exit 2; }
kb="$prefix/bin/kb"

# 1. install via the real installer (fail closed; provenance when gh is signed in)
begin "install"
if ! env PREFIX="$prefix" KB_VERSION="$KB_VERSION" ${KB_BASE_URL:+KB_BASE_URL="$KB_BASE_URL"} \
     sh "$install_sh" >"$tmp/install.out" 2>&1; then
  sed 's/^/  | /' "$tmp/install.out"
  fail "install.sh failed"
fi
if grep -q 'provenance OK' "$tmp/install.out"; then
  echo "PROVENANCE $leg ok"
else
  echo "PROVENANCE $leg not-checked"
  if [ "${REQUIRE_PROVENANCE:-}" = "1" ]; then
    sed 's/^/  | /' "$tmp/install.out"
    fail "REQUIRE_PROVENANCE=1 but install.sh did not verify build provenance"
  fi
fi
ok

# 2. the installed tree is complete and the binary runs on this libc
begin "layout"
for f in bin/kb bin/kb-embedder share/kb/web/dist/index.html; do
  [ -e "$prefix/$f" ] || fail "missing $prefix/$f"
done
[ -d "$prefix/share/kb/sample-corpus" ] || fail "missing $prefix/share/kb/sample-corpus"
ver="$("$kb" --version 2>&1)" || fail "kb --version failed: $ver"
want="${KB_VERSION#v}"
case "$ver" in *"$want"*) ;; *) fail "kb --version '$ver' does not contain '$want'" ;; esac
ok

# 3. register a COPY of the sample corpus (the install tree is never mutated)
begin "kb add"
export KB_HOME="$tmp/home"
mkdir -p "$KB_HOME"
corpus_src="${CORPUS_DIR:-$prefix/share/kb/sample-corpus}"
[ -d "$corpus_src" ] || fail "corpus dir $corpus_src not found"
corpus="$tmp/corpus"
cp -R "$corpus_src" "$corpus"
"$kb" add "$corpus" --kb canon >"$tmp/add.out" 2>&1 || { cat "$tmp/add.out"; fail "kb add failed"; }
cfg="$KB_HOME/config/kb.toml"
[ -f "$cfg" ] || fail "kb add did not write $cfg"
grep -q '^\[kb\.canon\]' "$cfg" || fail "no [kb.canon] section in $cfg"
# spare loopback port (default 4000 is often taken on a dev box)
if ! grep -q '^\[server\]' "$cfg"; then
  printf '\n[server]\naddr = "127.0.0.1:%s"\n' "$port" >>"$cfg"
else
  fail "kb add wrote a [server] section; the smoke cannot pick a spare port"
fi
ok

# 4. start the daemon, wait for /healthz
begin "daemon starts"
"$kb" daemon >"$log" 2>&1 &
daemon_pid=$!
[ -z "${SMOKE_PIDFILE:-}" ] || echo "$daemon_pid" >"$SMOKE_PIDFILE"
healthy=0
i=0
while [ "$i" -lt "$daemon_wait" ]; do
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    tail_log
    fail "daemon exited before becoming healthy"
  fi
  if curl -fsS --max-time 3 "$url/healthz" >/dev/null 2>&1; then healthy=1; break; fi
  sleep 1; i=$((i + 1))
done
if [ "$healthy" != 1 ]; then tail_log; fail "no /healthz within ${daemon_wait}s"; fi
ok

# 5. the SPA is served from <bin>/../share/kb/web/dist with no env var
begin "spa served"
code="$(curl -sS --max-time 10 -o "$tmp/root.html" -w '%{http_code}' "$url/" 2>"$tmp/curl.err")" \
  || { cat "$tmp/curl.err"; fail "GET / failed"; }
[ "$code" = "200" ] || { tail_log; fail "GET / returned HTTP $code (SPA not found next to the binary?)"; }
grep -q '<div id="root"' "$tmp/root.html" || fail "GET / is not the SPA shell (no <div id=\"root\")"
ok

# search helper: prints the hit count for $1 (keyword mode: BM25, no model)
hits() {
  "$kb" search "$1" --kb canon --mode keyword --json --daemon "$url" 2>/dev/null \
    | grep -c '"source_relative"' || true
}

# 6. keyword search finds the seeded corpus once indexing completes
begin "search sample corpus"
found=0
i=0
while [ "$i" -lt "$index_wait" ]; do
  h="$(hits "$term")"
  if [ "${h:-0}" -ge 1 ]; then found=1; break; fi
  sleep 2; i=$((i + 2))
done
if [ "$found" != 1 ]; then tail_log; fail "keyword search for '$term' returned 0 hits within ${index_wait}s"; fi
ok

# 7. the watcher indexes a file written while the daemon runs
begin "watcher picks up new file"
tok="smokeunique$(date +%s)x$RANDOM"
printf '<!doctype html><html><head><meta charset="utf-8"><title>Smoke added</title></head><body><h1>Smoke added</h1><p>%s</p></body></html>\n' "$tok" >"$corpus/smoke-added.html"
found=0
i=0
while [ "$i" -lt "$watch_wait" ]; do
  h="$(hits "$tok")"
  if [ "${h:-0}" -ge 1 ]; then found=1; break; fi
  sleep 2; i=$((i + 2))
done
if [ "$found" != 1 ]; then tail_log; fail "new file not searchable within ${watch_wait}s"; fi
ok

# 8. daemon doctor must be healthy; `doctor --hooks` is informational only
begin "daemon doctor"
if ! "$kb" daemon doctor --endpoint "$url" --json >"$tmp/doctor.json" 2>&1; then
  cat "$tmp/doctor.json"; tail_log; fail "kb daemon doctor reported unhealthy"
fi
"$kb" doctor --hooks --daemon "$url" >"$tmp/hooks.out" 2>&1
echo "note: kb doctor --hooks exit $? (informational, non-gating)"
ok
exit 0
