#!/usr/bin/env bash
# bench-nightly.sh — GC-A8: a non-blocking, repo-local search-quality
# regression run around `kb bench`.
#
# Boots a throwaway daemon (isolated KB_HOME, loopback-only, no bearer
# token needed — invariant #4) over the repo's own `docs/research`
# corpus, indexes it, runs `kb bench run` in **keyword (BM25-only)**
# mode against the committed `bench/queries/kb-docs.jsonl` gold set,
# and prints/compares the result. BM25-only is a deliberate choice
# (see docs/architecture-invariants.md #26 + kb_core::config
# `disable_embedder_fallback`): `mode=keyword` never touches
# `ctx.embedder`, so this script needs no kb-embedder binary, no ONNX
# Runtime, and no model download — it only builds `-p kb-cli` (the
# `kb` binary, which drives `kb_server::serve_loop` in-process for
# `kb daemon`). Cheap enough to run nightly on a modest runner.
#
# Usage:
#   scripts/bench-nightly.sh [--baseline PATH] [--out-dir DIR] [--skip-build]
#
# Env overrides:
#   KB_BIN       path to a pre-built `kb` binary (skips the cargo build)
#   BENCH_PORT   loopback port for the throwaway daemon (default 4173)
#   CORPUS_DIR   corpus to index (default: docs/research, repo-relative)
#   QUERIES      gold-set jsonl (default: bench/queries/kb-docs.jsonl)
#   KB_NAME      kb name in the throwaway config (default: kb-docs)
#
# Exits non-zero only on a hard failure to produce a report (build
# failure, daemon never came up, bench run errored) — a quality/speed
# regression is NOT a non-zero exit; the caller (the nightly workflow)
# reads the JSON/markdown for that and never fails the build on it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

BASELINE=""
OUT_DIR="bench-nightly-out"
SKIP_BUILD=0

while [ $# -gt 0 ]; do
  case "$1" in
    --baseline) BASELINE="$2"; shift 2 ;;
    --out-dir) OUT_DIR="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

BENCH_PORT="${BENCH_PORT:-4173}"
CORPUS_DIR="${CORPUS_DIR:-$REPO_ROOT/docs/research}"
QUERIES="${QUERIES:-$REPO_ROOT/bench/queries/kb-docs.jsonl}"
KB_NAME="${KB_NAME:-kb-docs}"
DAEMON_URL="http://127.0.0.1:${BENCH_PORT}"

mkdir -p "$OUT_DIR"
OUT_JSON="$OUT_DIR/report.json"
OUT_MD="$OUT_DIR/report.md"

log() { echo "[bench-nightly] $*" >&2; }

# ---- 1. build (fast profile) or reuse a caller-provided binary ----
if [ -n "${KB_BIN:-}" ]; then
  log "using pre-built KB_BIN=$KB_BIN"
elif [ "$SKIP_BUILD" -eq 1 ]; then
  KB_BIN="$REPO_ROOT/target/fast/kb"
  log "SKIP_BUILD set — expecting $KB_BIN to already exist"
else
  log "building kb (fast profile, kb-cli only — no ONNX / kb-embedder)"
  cargo build --profile fast -p kb-cli
  KB_BIN="$REPO_ROOT/target/fast/kb"
fi

if [ ! -x "$KB_BIN" ]; then
  echo "::error::kb binary not found/executable at $KB_BIN" >&2
  exit 1
fi

if [ ! -d "$CORPUS_DIR" ]; then
  echo "::error::corpus dir $CORPUS_DIR does not exist" >&2
  exit 1
fi
if [ ! -f "$QUERIES" ]; then
  echo "::error::gold-set queries file $QUERIES does not exist" >&2
  exit 1
fi

# ---- 2. isolated throwaway daemon (own KB_HOME, own config) ----
WORKDIR="$(mktemp -d /tmp/kb-bench-nightly.XXXXXX)"
export KB_HOME="$WORKDIR/home"
CONFIG="$WORKDIR/kb.toml"
LOG_FILE="$WORKDIR/daemon.log"
mkdir -p "$KB_HOME"

cat > "$CONFIG" <<EOF
# Throwaway nightly-bench config — BM25-only (no embedding_model
# anywhere + disable_embedder_fallback so no kb-embedder subprocess is
# ever spawned; see kb_core::config::resolved_embedding_model).
[daemon]
name = "bench-nightly"

[server]
addr = "127.0.0.1:${BENCH_PORT}"

[defaults]
disable_embedder_fallback = true

[kb.${KB_NAME}]
path = "${CORPUS_DIR}"
EOF

log "starting throwaway daemon on ${DAEMON_URL} (KB_HOME=$KB_HOME)"
"$KB_BIN" daemon --config "$CONFIG" >"$LOG_FILE" 2>&1 &
DAEMON_PID=$!

cleanup() {
  if kill -0 "$DAEMON_PID" 2>/dev/null; then
    log "stopping daemon (pid $DAEMON_PID)"
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# ---- 3. wait for /healthz ----
log "waiting for /healthz..."
ok=0
for _ in $(seq 1 60); do
  if curl -fsS -o /dev/null "$DAEMON_URL/healthz" 2>/dev/null; then
    ok=1
    break
  fi
  if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
    log "daemon exited early; log follows:"
    cat "$LOG_FILE" >&2 || true
    exit 1
  fi
  sleep 1
done
if [ "$ok" -ne 1 ]; then
  log "daemon never answered /healthz within 60s; log follows:"
  cat "$LOG_FILE" >&2 || true
  exit 1
fi

# ---- 4. wait for indexing to settle (doc_count stops growing) ----
log "waiting for indexer to settle on ${KB_NAME}..."
# Floor estimate: counts only .html/.htm. The indexer also ingests
# Markdown notes (invariant #16), so the daemon's real doc_count is
# typically a bit higher — the "settle" check below only needs
# `doc_count >= expected`, so undercounting here is harmless.
expected=$(find "$CORPUS_DIR" -iname '*.html' -o -iname '*.htm' | wc -l)
log "expecting >= ${expected} html docs under ${CORPUS_DIR} (doc_count may be higher: notes etc.)"
prev=-1
stable_iters=0
for _ in $(seq 1 120); do
  cur=$(curl -fsS "$DAEMON_URL/api/kb/${KB_NAME}/stats" 2>/dev/null | grep -o '"doc_count":[0-9]*' | head -1 | grep -o '[0-9]*$' || echo 0)
  cur=${cur:-0}
  if [ "$cur" -ge "$expected" ] && [ "$cur" = "$prev" ]; then
    stable_iters=$((stable_iters + 1))
  else
    stable_iters=0
  fi
  prev="$cur"
  # Two consecutive stable polls at/above the expected count = settled.
  if [ "$stable_iters" -ge 2 ]; then
    break
  fi
  sleep 1
done
log "indexer settled at doc_count=${prev} (expected ${expected})"

# ---- 5. run the bench (keyword/BM25-only mode) ----
BASELINE_ARGS=()
if [ -n "$BASELINE" ] && [ -f "$BASELINE" ]; then
  log "comparing against baseline $BASELINE"
  BASELINE_ARGS=(--baseline "$BASELINE")
else
  log "no baseline supplied/found — running without Δ columns"
fi

log "running kb bench run (mode=keyword)..."
"$KB_BIN" bench run \
  --queries "$QUERIES" \
  --kbs "$KB_NAME" \
  --modes keyword \
  --k 1 --k 5 --k 10 \
  --daemon "$DAEMON_URL" \
  --output "$OUT_MD" \
  --json "$OUT_JSON" \
  "${BASELINE_ARGS[@]}"

log "report: $OUT_MD / $OUT_JSON"
echo "$OUT_MD"
