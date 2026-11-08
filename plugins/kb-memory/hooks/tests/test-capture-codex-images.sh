#!/usr/bin/env bash
# test-capture-codex-images.sh — T20: codex `input_image` becomes a
# mime-only blob_ref block, never the base64. Runs kb-capture-codex.sh with
# the REAL kb (the codex adapter has no bash fallback: without a kb it
# spools and writes nothing), so the landing path is the Rust capture
# engine's, and the assertions read the written session capture.
#
# Runnable standalone (needs a real kb: KB_BIN_DIR, else PATH):
#   KB_BIN_DIR=<dir-with-kb> bash plugins/kb-memory/hooks/tests/test-capture-codex-images.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CAPTURE="$HOOKS_DIR/kb-capture-codex.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-capture-codex-images-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT
# AH1: sandbox HOME/XDG/KB_* before anything can reach a kb (see lib-sandbox.sh).
. "$SCRIPT_DIR/lib-sandbox.sh"
kb_test_sandbox "$TMPROOT" || exit 1

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

REAL_PATH="$PATH"
[ -n "${KB_BIN_DIR:-}" ] && REAL_PATH="$KB_BIN_DIR:$PATH"
if ! PATH="$REAL_PATH" command -v kb >/dev/null 2>&1; then
  bad "codex images: a real kb binary is required (KB_BIN_DIR)"
  echo "passed=$PASS failed=$FAIL"; exit 1
fi

OUTDIR="$TMPROOT/sessions"
mkdir -p "$OUTDIR"
PATH="$REAL_PATH" KB_SESSIONS_DIR="$OUTDIR" bash "$CAPTURE" \
  "$SCRIPT_DIR/fixtures/codex-rollout-images.jsonl" >/dev/null 2>&1

out="$(find "$OUTDIR" -name 'session-*.html' | head -1)"
if [ -n "$out" ]; then
  ok "codex rollout with input_image captured"
else
  bad "codex rollout with input_image wrote nothing"
  echo "passed=$PASS failed=$FAIL"; exit 1
fi

body="$(sed -n '/<pre>/,/<\/pre>/p' "$out" | sed '1d;$s|</pre>$||')"

if printf '%s\n' "$body" | jq -e 'select(.type == "user") | .message.content[] | select(.type == "image") | .source | .type == "blob_ref" and .media_type == "image/png" and (has("sha256") == false)' >/dev/null 2>&1; then
  ok "input_image became a mime-only blob_ref (image/png, no sha256)"
else
  bad "image block shape wrong: $body"
fi

# The fixture's base64 never survives into the artifact.
if grep -q 'QUJDREVGRw==' "$out"; then
  bad "the fixture base64 leaked into the capture"
else
  ok "the fixture base64 is absent from the capture"
fi

echo
echo "== T20: codex image-only user message writes no record =="
ONLY_OUTDIR="$TMPROOT/sessions-only"
mkdir -p "$ONLY_OUTDIR"
PATH="$REAL_PATH" KB_SESSIONS_DIR="$ONLY_OUTDIR" bash "$CAPTURE" \
  "$SCRIPT_DIR/fixtures/codex-rollout-images-only.jsonl" >/dev/null 2>&1
only_out="$(find "$ONLY_OUTDIR" -name 'session-*.html' | head -1)"
if [ -n "$only_out" ]; then
  ok "image-only rollout still lands its adapter-meta capture"
else
  bad "image-only rollout wrote nothing (expected the adapter-meta capture)"
fi
if [ -n "$only_out" ]; then
  only_body="$(sed -n '/<pre>/,/<\/pre>/p' "$only_out" | sed '1d;$s|</pre>$||')"
else
  only_body=""
fi
if printf '%s\n' "$only_body" | jq -e 'select(.type == "user")' >/dev/null 2>&1; then
  bad "image-only codex message leaked a user record: $only_body"
else
  ok "image-only codex message writes no user record"
fi
if printf '%s\n' "$only_body" | jq -e '[.. | objects | select(.type == "image")] | length == 0' >/dev/null 2>&1; then
  ok "no image block survives from an image-only message"
else
  bad "image block leaked from an image-only message: $only_body"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
