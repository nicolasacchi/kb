#!/usr/bin/env bash
# trailer-logic.sh — stamps a `Kb-Session: <id>` commit trailer (W0.3)
# and, when the repo opts in, one `Kb-Memory: <hex12>` per memory this
# session minted (CT-F1).
#
# Invoked by dispatch.sh ONLY for prepare-commit-msg, as a SEPARATE
# process (never sourced) so nothing here — including a stray `exit`
# — can affect the caller; dispatch.sh also wraps the call in
# `|| true`. This file is the second layer of that fail-open contract:
# every exit below is a plain `exit 0`, there's no `set -e`, and every
# external command that can fail is guarded.
#
# Purpose: future git commits made during a Claude Code (or codex/
# opencode) session get a `Kb-Session: <session-id>` trailer, so
# session<->commit joins in kb (docs/architecture-invariants.md #11)
# become exact instead of a transcript-grep guess. CT-F1 does the same
# for MEMORIES: a `Kb-Memory:` trailer turns "this memory is probably
# behind that work" into an exact id join (kb's `memory_commits`, V0038).
#
# CT-F1 IS OPT-IN, PER REPO, AND OFF BY DEFAULT (operator ruling,
# 2026-08-20). Opaque memory ids in a commit message are fine in a
# private repo and not something to default anyone into, so the gate is
# the cheapest honest one — a REPO-LOCAL git config key:
#
#     git config --local kb.memoryTrailers true      # opt in
#     git config --local --unset kb.memoryTrailers   # opt back out
#
# `--local` is deliberate: the value must live in THIS repo's own
# `.git/config` (never committed, never inherited from ~/.gitconfig), so
# opting one repo in can't silently leak ids out of another. See
# hooks/README.md ("Kb-Memory trailers") for the full write-up.
#
# Args (verbatim from git's prepare-commit-msg hook):
#   $1  path to the commit message file (required)
#   $2  commit source: message|template|merge|squash|commit|"" (optional)
#   $3  commit SHA-1 — only set for source=commit (-c/-C/--amend) (optional)
set -u

msg_file="${1:-}"
commit_source="${2:-}"
commit_sha="${3:-}"

[ -n "$msg_file" ] && [ -f "$msg_file" ] || exit 0

# --- source-arg gating ------------------------------------------------
# Stamp on an author-composed message (message/template/empty=default
# editor). `merge`/`squash` are never stamped (multi-parent / squashed
# history isn't "this session's own commit"). `commit` (-c/-C/--amend,
# which replays ANOTHER commit's message verbatim) is excluded UNLESS
# it is specifically `--amend` of the current HEAD ($3 == "HEAD" — the
# literal string git passes, verified empirically; `-c`/`-C <ref>`
# pass the given ref/sha instead) — that's the "amend semantics" case
# below, where we dedupe against whatever trailer the commit already
# carries rather than blindly re-stamping a copied, unrelated message.
case "$commit_source" in
  "" | message | template) ;;
  commit)
    [ "$commit_sha" = "HEAD" ] || exit 0
    ;;
  *) exit 0 ;;
esac

# Never mid-rebase — prepare-commit-msg fires for every replayed commit.
rebase_merge="$(git rev-parse --git-path rebase-merge 2>/dev/null)" || rebase_merge=""
[ -n "$rebase_merge" ] && [ -d "$rebase_merge" ] && exit 0
rebase_apply="$(git rev-parse --git-path rebase-apply 2>/dev/null)" || rebase_apply=""
[ -n "$rebase_apply" ] && [ -d "$rebase_apply" ] && exit 0

# --- session id resolution ---------------------------------------------
# (a) CLAUDE_CODE_SESSION_ID — verified live in Claude Code's Bash env.
# (b) GROK_SESSION_ID — Grok Build injects this on hook processes; some
#     tool/bash children inherit it too. Claude wins if both are set
#     (a nested session is rarer than a leftover Grok var).
# (c) fallback: the repo-keyed marker file kb-wake.sh / kb-recall.sh
#     (and the Grok adapters that wrap them) write, freshness-gated
#     to ~40 minutes so a long-dead session never mis-attributes a
#     much later commit.
sid="${CLAUDE_CODE_SESSION_ID:-}"
if [ -z "$sid" ]; then
  sid="${GROK_SESSION_ID:-}"
fi

if [ -z "$sid" ]; then
  # Slug algorithm MUST match kb-wake.sh / kb-recall.sh exactly (kept
  # in lockstep by hand, not a shared lib — those hooks are also
  # copyable standalone; see hooks/README.md "Mode 1: manual").
  kb_slugify() {
    local s
    s="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9' '-')"
    s="${s#-}"
    s="${s%-}"
    printf '%s' "$s"
  }

  root="$(git rev-parse --show-toplevel 2>/dev/null)" || root=""
  cwd="${root:-$PWD}"
  repo_key="$(kb_slugify "$cwd")"
  marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
  marker_file="$marker_dir/current-session-repo-$repo_key"

  if [ -n "$repo_key" ] && [ -r "$marker_file" ]; then
    marker_sid="$(sed -n '1p' "$marker_file" 2>/dev/null)"
    marker_ts="$(sed -n '2p' "$marker_file" 2>/dev/null)"
    case "$marker_ts" in
      '' | *[!0-9]*) marker_ts=0 ;;
    esac
    now="$(date +%s 2>/dev/null)" || now=0
    age=$((now - marker_ts))
    max_age=2400 # ~40 minutes
    if [ -n "$marker_sid" ] && [ "$age" -ge 0 ] && [ "$age" -le "$max_age" ]; then
      sid="$marker_sid"
    fi
  fi
fi

[ -n "$sid" ] || exit 0

# --- trailer append (shared) -------------------------------------------
# `git interpret-trailers --if-exists=add` (never `replace`: a trailer
# here is SET-VALUED — a commit can legitimately carry more than one
# session/memory). Falls back to appending by hand — before git's own
# "# Please enter the commit message" comment block, if present — when
# `interpret-trailers` is unavailable or fails. Returns non-zero only if
# even the fallback couldn't write; every caller ignores that (fail-open).
add_trailer() {
  local trailer="$1" tmp
  if git interpret-trailers --if-exists=add --in-place \
    --trailer "$trailer" "$msg_file" 2>/dev/null; then
    return 0
  fi
  tmp="$(mktemp 2>/dev/null)" || return 1
  awk -v trailer="$trailer" '
    BEGIN { done = 0 }
    /^# / && !done { print trailer; print ""; done = 1 }
    { print }
    END { if (!done) { print ""; print trailer } }
  ' "$msg_file" >"$tmp" 2>/dev/null && mv "$tmp" "$msg_file" 2>/dev/null || {
    rm -f "$tmp" 2>/dev/null
    return 1
  }
}

# Every value already present for one trailer key, one per line.
existing_values() {
  git interpret-trailers --parse "$msg_file" 2>/dev/null \
    | sed -n "s/^$1: *//p"
}

# --- amend semantics: set-valued trailer --------------------------------
# Same id already present -> no-op. Different (or absent) -> add a new
# `Kb-Session:` line (never replace — a commit can legitimately span
# more than one session, e.g. an --amend from a later session).
#
# NB: this used to `exit 0` on a match. It now only SKIPS the session
# stamp, because CT-F1's memory block below must still run on an amend
# whose session trailer is already there.
existing_ids="$(existing_values Kb-Session)"

session_present=0
while IFS= read -r line; do
  [ "$line" = "$sid" ] && session_present=1
done <<EOF
$existing_ids
EOF

[ "$session_present" -eq 1 ] || add_trailer "Kb-Session: $sid"

# --- CT-F1: `Kb-Memory:` trailers (opt-in, per repo, default OFF) -------
# Gate first — one `git config` read, and the overwhelmingly common
# answer is "no", so an un-opted-in repo pays nothing beyond it (no
# daemon call, no parsing). `--local` keeps the opt-in in THIS repo's own
# config: a global/system value is deliberately NOT honoured.
mem_enabled="$(git config --local --get --bool kb.memoryTrailers 2>/dev/null)" || mem_enabled=""
[ "$mem_enabled" = "true" ] || exit 0

command -v curl >/dev/null 2>&1 || exit 0

# The session id goes into a URL path segment. Anything outside the
# id-shaped charset is refused rather than escaped — a session id is a
# uuid in every harness we capture, so this can only fire on something
# already wrong.
case "$sid" in
  '' | *[!A-Za-z0-9._-]*) exit 0 ;;
esac

# Loopback daemon must not ride HTTP(S)_PROXY (same hazard kb-recall.sh
# documents: a VPN alias routing everything through 127.0.0.1:8892).
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy

# "Memories minted THIS session" = every artifact carrying this session's
# `<meta name="kb-session">`, which is exactly what
# `GET /api/sessions/{sid}/memories` lists (a lance scan on `kb_session`,
# fanned out across corpora). Deliberately NOT `/api/sessions/{sid}`:
# that one 404s until the session has been CAPTURED, and the whole point
# is to stamp commits made mid-session. Hard 2s cap, and any failure
# (daemon down, no hits, bad JSON) just means no memory trailers.
kb_url="${KB_DAEMON_URL:-http://127.0.0.1:4000}"
resp="$(curl -fsS --max-time 2 "$kb_url/api/sessions/$sid/memories" 2>/dev/null)" || exit 0
[ -n "$resp" ] || exit 0

# jq is NOT required here (a git hook should stay dependency-light): the
# id field is a fixed 12-lowercase-hex shape, so one anchored `grep -o`
# is both sufficient and safe — a value that isn't exactly 12 hex chars
# can't be extracted at all, and kb's own parse-back
# (`sessions::memory_ids_from_trailers`) re-validates the same grammar.
ids="$(printf '%s' "$resp" | grep -o '"id":"[0-9a-f]\{12\}"' 2>/dev/null \
  | sed 's/^"id":"//; s/"$//' | sort -u)"
[ -n "$ids" ] || exit 0

# Already-stamped ids (an --amend re-runs this hook) are skipped, and the
# whole set is capped: a commit message is a human artifact, and a
# session that minted dozens of memories should not bury its own subject
# under a wall of opaque ids. The cap is silent by design — the ids are a
# convenience join, never the record (kb's `memories` list is).
existing_mem="$(existing_values Kb-Memory)"
stamped=0
max_trailers=20
for mem_id in $ids; do
  [ "$stamped" -ge "$max_trailers" ] && break
  seen=0
  while IFS= read -r line; do
    [ "$line" = "$mem_id" ] && seen=1
  done <<EOF
$existing_mem
EOF
  [ "$seen" -eq 1 ] && continue
  add_trailer "Kb-Memory: $mem_id" || break
  stamped=$((stamped + 1))
done

exit 0
