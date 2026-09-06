#!/usr/bin/env bash
# Stop hook — capture the conversation transcript verbatim into the
# [kb.sessions] corpus when the agent finishes responding.
#
# Deterministic and LLM-free: NO extraction, NO summarisation. It wraps
# the raw JSONL transcript in an HTML <pre> and drops it in the sessions
# corpus dir; the daemon's watcher indexes it like any other artifact.
# (`Stop` — not `SessionEnd` — is the "agent finished" event; it carries
# `transcript_path`.)
#
# W0.4 — the wrap+write step now PREFERS `kb sessions capture` (the Rust
# engine, `kb-cli/src/commands/sessions_capture.rs`) when a `kb` binary is on
# PATH: it writes the IDENTICAL envelope through `</pre>` (byte-for-byte —
# `recover_jsonl_from_capture`'s round-trip invariant), plus an additive
# commit-resolution tail block a bash heredoc can't build (one `git show -s`
# per detected sha, resolving the true subject/author/parents/trailers). The
# bash path below is the PERMANENT fallback — no `kb` on PATH, or the `kb`
# call itself fails for any reason — so a capture never silently stops
# working; it just loses commit resolution.
#
# Set KB_SESSIONS_DIR to the [kb.sessions] source path. Unset → no-op.
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0

input="$(cat)"
tpath="$(printf '%s' "$input" | jq -r '.transcript_path // empty' 2>/dev/null)"
[ -n "$tpath" ] && [ -f "$tpath" ] || exit 0

# Raw (un-sanitised) hint values for `kb sessions capture` — it recovers the
# canonical session id from the transcript's own `sessionId` itself
# (invariant #11) and only falls back to `--session-id` when that's absent,
# so this need not be sanitised here.
raw_sid="$(printf '%s' "$input" | jq -r '.session_id // "session"' 2>/dev/null)"
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"

if command -v kb >/dev/null 2>&1; then
  if kb sessions capture \
       --transcript "$tpath" \
       --session-id "$raw_sid" \
       ${cwd:+--cwd "$cwd"} \
       --out "$KB_SESSIONS_DIR" \
       >/dev/null 2>&1; then
    exit 0
  fi
  # Any failure (unreadable transcript, no `git`, unexpected shape, …) falls
  # through to the bash path below — never leave the session uncaptured.
fi

mkdir -p "$KB_SESSIONS_DIR" || exit 0
ts="$(date -u +%Y%m%dT%H%M%SZ)"
# Sanitise to filename-safe chars, then bound length WITHOUT cutting a UUID
# (36 chars). `cut -c1-24` truncated the id mid-UUID, which broke `claude -r`
# and the session↔memories link (the kb-wake marker keeps the full id).
sid="$(printf '%s' "$raw_sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"

# One file per session, overwritten on every capture: re-captures of a live
# session UPDATE its artifact instead of accumulating near-duplicate
# cumulative snapshots (392 files / 47 GB of lance state by 2026-07-03,
# incl. the 2026-07-02 nofile outage). Reuse the existing file so the
# original start timestamp survives in the name (`sessions.rs` derives
# started_at from it); newest match wins if pre-fix duplicates remain.
out=""
for f in "$KB_SESSIONS_DIR"/session-*-"$sid.html"; do
  [ -f "$f" ] && out="$f"
done
[ -n "$out" ] || out="$KB_SESSIONS_DIR/session-$ts-$sid.html"

# 2026-08-21 ci-host incident hardening — refuse an oversized RAW transcript in
# this bash fallback path too (`kb sessions capture` above already refuses
# one on its own terms; this guards the case where `kb` is absent from
# PATH, or where that very refusal is what caused the fallthrough to here).
# A Stop hook must never fail the session — skip the capture, don't fail it.
tsize="$(stat -c %s "$tpath" 2>/dev/null || wc -c <"$tpath" 2>/dev/null)"
if [ -n "$tsize" ] && [ "$tsize" -gt 50331648 ]; then
  echo "kb-capture.sh: skipping oversized transcript ($tsize bytes > 48MiB cap): $tpath" >&2
  exit 0
fi

# HTML-escape the transcript and wrap it verbatim. Write to a temp path and
# mv into place: the replace is atomic, so the watcher never ingests a
# half-written multi-MB transcript.
esc="$(sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' "$tpath")" || exit 0
tmp="$out.tmp"
cat >"$tmp" <<EOF || { rm -f "$tmp"; exit 0; }
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Session transcript $ts</title>
<meta name="kb-category" content="memory-session">
<meta name="kb-decay" content="fast">
<meta name="kb-session" content="$sid">
</head><body>
<h1>Session transcript $ts</h1>
<pre>$esc</pre>
</body></html>
EOF
mv -f "$tmp" "$out" 2>/dev/null || rm -f "$tmp"
