#!/usr/bin/env bash
# Self-test for scripts/ci/first-run-smoke.sh. No network, no real binaries: a
# fake release mirror (file://) carries a stub `kb` that implements only the
# subcommands the smoke calls, backed by a tiny python3 HTTP responder. Proves
# the smoke's control flow, step reporting, RESULT line and teardown, and that
# it FAILS when the daemon never becomes healthy, search finds nothing, or the
# SPA root is missing, and that it never bypasses install.sh's fail-closed
# checks.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
smoke="$here/first-run-smoke.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fails=0
fail() { echo "FAIL: $*" >&2; fails=$((fails + 1)); }
pass() { echo "ok: $*"; }

case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;; aarch64|arm64) arch=aarch64 ;;
  *) echo "unsupported test host" >&2; exit 1 ;;
esac
ver="9.9.9"
pkg="kb-${ver}-${arch}-unknown-linux-gnu"
src="$work/src/$pkg"
mkdir -p "$work/mirror" "$src/share/kb/web/dist" "$src/share/kb/sample-corpus"
echo '<!doctype html><html><body><div id="root"></div></body></html>' > "$src/share/kb/web/dist/index.html"
echo '<html><head><title>Canon</title></head><body><p>drag to resolve the borrow checker</p></body></html>' > "$src/share/kb/sample-corpus/a.html"
printf '#!/bin/sh\nexit 0\n' > "$src/kb-embedder"

# --- the responder: /healthz, /, /api/search over the kb's path ---------------
cat > "$work/stub-server.py" <<'PY'
import hashlib, http.server, json, os, sys, re, urllib.parse
port, corpus = int(sys.argv[1]), sys.argv[2]
mode = os.environ.get("STUB_MODE", "ok")
pidfile = os.path.join(os.environ["KB_HOME"], "state", "kb-daemon.pid")
os.makedirs(os.path.dirname(pidfile), exist_ok=True)
open(pidfile, "w").write(str(os.getpid()))
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        sys.stderr.write("stub-daemon-log: " + (a[0] % a[1:]) + "\n")
    def send(self, code, body, ctype="text/html"):
        b = body.encode(); self.send_response(code)
        self.send_header("content-type", ctype); self.send_header("content-length", str(len(b)))
        self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        if u.path == "/healthz": return self.send(200, "ok", "text/plain")
        if u.path == "/":
            if mode == "spa404": return self.send(404, "not found", "text/plain")
            return self.send(200, '<html><body><div id="root"></div></body></html>')
        if u.path == "/api/search":
            q = urllib.parse.parse_qs(u.query).get("q", [""])[0].lower()
            hits = []
            if mode != "nohits":
                for f in sorted(os.listdir(corpus)):
                    if q and q in open(os.path.join(corpus, f)).read().lower():
                        hits.append({"id": hashlib.md5(f.encode()).hexdigest()[:12], "title": f, "path": "/corpus/" + f, "kb_category": None})
            return self.send(200, json.dumps({"hits": hits}), "application/json")
        self.send(404, "nope", "text/plain")
http.server.HTTPServer(("127.0.0.1", port), H).serve_forever()
PY

# --- the fake kb ----------------------------------------------------------------
cat > "$src/kb" <<'SH'
#!/bin/sh
# stub kb for first-run-selftest.sh
cfg="$KB_HOME/config/kb.toml"
case "$1" in
  --version) echo "kb 9.9.9-selftest" ;;
  add)
    mkdir -p "$KB_HOME/config"
    kbname=default; p="$2"
    [ "$3" = "--kb" ] && kbname="$4"
    if [ "${STUB_ADD:-}" = minimal ]; then
      printf '[kb.%s]\npath = "%s"\n' "$kbname" "$p" > "$cfg"
    else
      # Mirrors the real `kb add` (v0.45): it re-serialises the WHOLE config, so
      # [server] and [defaults] are present with their defaults on a fresh file.
      printf '[daemon]\n\n[server]\naddr = "127.0.0.1:4000"\nmdns = false\nparent_origin = "http://localhost:4000"\n\n[defaults]\ndisable_embedder_fallback = false\n\n[kb.%s]\npath = "%s"\nskip_patterns = []\n\n[kb.%s.ui]\n' "$kbname" "$p" "$kbname" > "$cfg"
    fi ;;
  daemon)
    case "$2" in
      stop) kill "$(cat "$KB_HOME/state/kb-daemon.pid" 2>/dev/null)" 2>/dev/null || exit 1 ;;
      doctor)
        ep="$3"; [ "$3" = "--endpoint" ] && ep="$4"
        curl -fsS --max-time 3 "$ep/healthz" >/dev/null ;;
      *)
        port="$(sed -n 's/^addr = "127.0.0.1:\([0-9]*\)"/\1/p' "$cfg")"
        corpus="$(sed -n 's/^path = "\(.*\)"/\1/p' "$cfg")"
        echo "stub daemon booting on $port"
        if [ "$port" = 4000 ] || [ -z "$port" ]; then
          echo "stub-daemon-log: refusing to bind port '$port' (smoke must move [server] addr off 4000)"; exit 5
        fi
        if ! grep -q '^disable_embedder_fallback = true' "$cfg"; then
          echo "stub-daemon-log: embedder fallback not disabled; would download a model"; exit 3
        fi
        if [ "${STUB_MODE:-}" = unhealthy ]; then
          echo "stub-daemon-log: wedged, never serves"; exec sleep 600
        fi
        exec python3 "$STUB_SERVER" "$port" "$corpus" ;;
    esac ;;
  search)
    q="$2"; url=""
    while [ $# -gt 0 ]; do [ "$1" = "--daemon" ] && url="$2"; shift; done
    # Emit EXACTLY what the real CLI prints: `print_hits_json` in
    # crates/kb-cli/src/commands/search.rs = serde_json::to_string_pretty of
    # {"hits":[{id,kb_category,path,title}],"ms","source"} (keys sorted).
    # There is no `source_relative`; do not invent fields here.
    curl -fsS --max-time 5 "$url/api/search?q=$q" | python3 -c '
import json, sys
d = json.load(sys.stdin)
print(json.dumps({"hits": d["hits"], "ms": 4, "source": sys.argv[1]}, indent=2, sort_keys=True))' "$url" ;;
  status) echo "stub status" ;;
  doctor) echo "PASS stub hooks" ;;
  *) echo "stub kb: unsupported: $*" >&2; exit 2 ;;
esac
SH
chmod +x "$src/kb" "$src/kb-embedder"
tar -czf "$work/mirror/$pkg.tar.gz" -C "$work/src" "$pkg"
sum="$(sha256sum "$work/mirror/$pkg.tar.gz" | awk '{print $1}')"
echo "$sum  $pkg.tar.gz" > "$work/mirror/$pkg.tar.gz.sha256"

# run_smoke <name> [VAR=val ...]  -> out in $work/<name>.out, rc in $rc, pid in $work/<name>.pid
run_smoke() {
  name="$1"; shift
  env -i HOME="$work/home" PATH="$PATH" \
    KB_BASE_URL="file://$work/mirror" KB_VERSION="$ver" LEG="selftest-$name" \
    STUB_SERVER="$work/stub-server.py" SMOKE_PIDFILE="$work/$name.pid" DAEMON_WAIT=20 INDEX_WAIT=6 WATCH_WAIT=20 \
    SEARCH_TERM=borrow "$@" bash "$smoke" >"$work/$name.out" 2>&1
  rc=$?
}
dead() { # dead <pidfile>: recorded daemon pid must not be alive
  [ -s "$1" ] || return 0
  ! kill -0 "$(cat "$1")" 2>/dev/null
}

# 1. happy path
run_smoke happy
if [ "$rc" = 0 ] && grep -q '^RESULT selftest-happy PASS$' "$work/happy.out"; then
  missing=""
  for i in 1 2 3 4 5 6 7 8; do grep -q "^STEP $i/8 .* ... ok$" "$work/happy.out" || missing="$missing $i"; done
  if [ -z "$missing" ]; then pass first_run_selftest_happy_path
  else fail "first_run_selftest_happy_path: steps not ok:$missing"; cat "$work/happy.out"; fi
else fail "first_run_selftest_happy_path rc=$rc"; cat "$work/happy.out"; fi
dead "$work/happy.pid" || fail "happy path left the daemon running"

# 1b. a config with NO [server]/[defaults] sections (older `kb add` shape) also works
run_smoke minimal STUB_ADD=minimal
if [ "$rc" = 0 ] && grep -q '^RESULT selftest-minimal PASS$' "$work/minimal.out"; then pass first_run_selftest_minimal_config_shape
else fail "first_run_selftest_minimal_config_shape rc=$rc"; cat "$work/minimal.out"; fi
dead "$work/minimal.pid" || fail "minimal run left the daemon running"

# 2. daemon never healthy -> fails within its timeout, log tail printed
start=$SECONDS
run_smoke unhealthy STUB_MODE=unhealthy DAEMON_WAIT=3
if [ "$rc" != 0 ] && grep -q '^RESULT selftest-unhealthy FAIL$' "$work/unhealthy.out" \
   && grep -q 'daemon log' "$work/unhealthy.out" && grep -q 'wedged, never serves' "$work/unhealthy.out" \
   && [ $((SECONDS - start)) -lt 60 ]; then pass first_run_selftest_unhealthy_daemon_fails
else fail "first_run_selftest_unhealthy_daemon_fails rc=$rc"; cat "$work/unhealthy.out"; fi
# 5 (failure path). The wedged daemon ignores `kb daemon stop`; teardown must still kill it.
if dead "$work/unhealthy.pid"; then pass first_run_selftest_teardown_kills_daemon_on_failure
else fail "first_run_selftest_teardown_kills_daemon: wedged daemon survived"; fi

# 3. zero search hits
run_smoke nohits STUB_MODE=nohits INDEX_WAIT=4
if [ "$rc" != 0 ] && grep -q 'STEP 6/8 .* FAIL' "$work/nohits.out" \
   && grep -q '^RESULT selftest-nohits FAIL$' "$work/nohits.out"; then pass first_run_selftest_zero_search_hits_fails
else fail "first_run_selftest_zero_search_hits_fails rc=$rc"; cat "$work/nohits.out"; fi
dead "$work/nohits.pid" || fail "nohits left the daemon running"

# 4. SPA root 404 (healthz fine) -> the SPA check is real
run_smoke spa404 STUB_MODE=spa404
if [ "$rc" != 0 ] && grep -q 'STEP 5/8 .* FAIL' "$work/spa404.out" \
   && grep -q '^RESULT selftest-spa404 FAIL$' "$work/spa404.out"; then pass first_run_selftest_spa_404_fails
else fail "first_run_selftest_spa_404_fails rc=$rc"; cat "$work/spa404.out"; fi
dead "$work/spa404.pid" || fail "spa404 left the daemon running"
if dead "$work/happy.pid" && dead "$work/nohits.pid" && dead "$work/spa404.pid"; then
  pass first_run_selftest_teardown_kills_daemon
fi

# 6. missing .sha256 -> install.sh fails closed, smoke fails at step 1
mv "$work/mirror/$pkg.tar.gz.sha256" "$work/sha.bak"
run_smoke nosum
mv "$work/sha.bak" "$work/mirror/$pkg.tar.gz.sha256"
if [ "$rc" != 0 ] && grep -q 'STEP 1/8 install ... FAIL' "$work/nosum.out" \
   && grep -q 'sidecar unavailable' "$work/nosum.out" && [ ! -s "$work/nosum.pid" ] \
   && ! grep -q 'KB_INSECURE_SKIP_VERIFY=' "$smoke"; then pass first_run_selftest_refuses_unverified
else fail "first_run_selftest_refuses_unverified rc=$rc"; cat "$work/nosum.out"; fi

# 6a. corrupted tarball (sidecar present but wrong) -> fails closed at step 1
cp "$work/mirror/$pkg.tar.gz.sha256" "$work/sha.bak"
echo "$(printf 0%.0s $(seq 1 64))  $pkg.tar.gz" > "$work/mirror/$pkg.tar.gz.sha256"
run_smoke badsum
mv "$work/sha.bak" "$work/mirror/$pkg.tar.gz.sha256"
if [ "$rc" != 0 ] && grep -q 'STEP 1/8 install ... FAIL' "$work/badsum.out" && [ ! -s "$work/badsum.pid" ]; then
  pass first_run_selftest_refuses_bad_checksum
else fail "first_run_selftest_refuses_bad_checksum rc=$rc"; cat "$work/badsum.out"; fi

# 6b. REQUIRE_PROVENANCE=1 without a signed-in gh -> fails at step 1
run_smoke noprov REQUIRE_PROVENANCE=1
if [ "$rc" != 0 ] && grep -q 'STEP 1/8 install ... FAIL: REQUIRE_PROVENANCE' "$work/noprov.out"; then
  pass first_run_selftest_require_provenance_fails_without_gh
else fail "REQUIRE_PROVENANCE not enforced rc=$rc"; cat "$work/noprov.out"; fi

# 6c. the hit predicate counts the REAL `kb search --json` shape. The golden is
# the shape from crates/kb-cli/src/commands/search.rs (print_hits_json).
hitsh="$here/first-run-hits.sh"
cat > "$work/real-hits.json" <<'JSON'
{
  "hits": [
    {"id": "07a5501697a0", "kb_category": null, "path": "/corpus/fullscreen-viz.html", "title": "Visualizing the Borrow Checker"},
    {"id": "17a5501697a1", "kb_category": "note", "path": "/corpus/b.html", "title": "B"},
    {"id": "27a5501697a2", "kb_category": null, "path": "/corpus/c.html", "title": "C"},
    {"id": "37a5501697a3", "kb_category": null, "path": "/corpus/d.html", "title": "D"}
  ],
  "ms": 4,
  "source": "http://127.0.0.1:4000"
}
JSON
printf '{\n  "hits": [],\n  "ms": 1,\n  "source": "x"\n}\n' > "$work/empty-hits.json"
printf 'Error: connection refused (id: not a hit)\n' > "$work/err-hits.txt"
if [ "$(bash "$hitsh" "$work/real-hits.json")" = 4 ] && [ "$(bash "$hitsh" "$work/empty-hits.json")" = 0 ] \
   && [ "$(bash "$hitsh" "$work/err-hits.txt")" = 0 ] && [ "$(bash "$hitsh" "$work/missing")" = 0 ]; then
  pass first_run_selftest_hit_predicate_counts_real_shape
else fail "first_run_selftest_hit_predicate_counts_real_shape: helper miscounts the real shape"; fi
if [ "$fails" -ne 0 ]; then echo "first-run selftest: $fails FAILED" >&2; exit 1; fi
echo "first-run selftest OK"
