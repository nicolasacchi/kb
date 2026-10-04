#!/usr/bin/env bash
# Minimal stand-in for `kb sessions capture` used by the adapter tests that
# only care about the TRANSLATION (not the Rust engine): writes
# session-<stamp|now>-<hook_sid_key(sid)>.html around the html-escaped
# transcript, reusing an existing file of the same key. Everything else exits 0.
# `kb sessions scrub` is identity. The real engine is covered by
# test-capture-adapters-spool.sh against the real binary.
if [ "${1:-}" = "sessions" ] && [ "${2:-}" = "scrub" ]; then exec cat; fi
if [ "${1:-}" = "sessions" ] && [ "${2:-}" = "capture" ]; then
  shift 2
  t="" sid="" stamp="" out=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --transcript) t="$2"; shift 2 ;;
      --session-id) sid="$2"; shift 2 ;;
      --stamp) stamp="$2"; shift 2 ;;
      --out) out="$2"; shift 2 ;;
      --cwd) shift 2 ;;
      *) shift ;;
    esac
  done
  [ -n "$t" ] && [ -f "$t" ] && [ -n "$out" ] || exit 1
  . "${HOOKS_DIR:?HOOKS_DIR must point at the hooks dir}/kb-hook-lib.sh"
  key="$(hook_sid_key "$sid")"
  mkdir -p "$out"
  f=""
  for c in "$out"/session-????????T??????Z-"$key.html"; do [ -f "$c" ] && f="$c"; done
  [ -n "$f" ] || f="$out/session-${stamp:-$(date -u +%Y%m%dT%H%M%SZ)}-$key.html"
  {
    printf '<!DOCTYPE html>\n<html><head><meta name="kb-session" content="%s"></head><body>\n<pre>' "$key"
    sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' "$t"
    printf '</pre>\n</body></html>\n'
  } >"$f.tmp" && mv -f "$f.tmp" "$f"
  exit 0
fi
exit 0
