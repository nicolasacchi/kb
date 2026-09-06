#!/usr/bin/env bash
# kb-code-annotations.sh — D4: surface the operator's `flag-for-agent`
# review annotations INTO Claude Code sessions, so a reader-side "flag for
# agent" becomes a live human -> agent code-review dialogue (operator flags
# in the kb-code reader -> the next session sees it -> replies/resolves via
# `kb-code annotate reply|resolve`).
#
# ONE script, TWO modes (dispatched on $1 — kept as one file rather than a
# kb-wake.sh/kb-recall.sh-style pair because both modes share the repo-
# matching + repos-cache plumbing near-verbatim, and hooks.json can already
# pass a literal extra argv token to a command):
#
#   (no arg)          PreToolUse (matcher "Edit|Write") — per-EDITED-FILE
#                      open flag-for-agent annotations, injected as
#                      hookSpecificOutput.additionalContext right before the
#                      edit lands, with the exact CLI commands to reply +
#                      resolve each one.
#   --session-start    SessionStart (matcher "startup|resume|clear", same
#                      matcher kb-memory's kb-wake.sh uses) — ONE summary
#                      line if the session's cwd sits inside a configured
#                      repo that has ANY open flag-for-agent annotations.
#                      No per-annotation dump here — that's the PreToolUse
#                      mode's job (or `kb-code annotations open` by hand).
#
# ---------------------------------------------------------------------------
# Same verified injection mechanism as kb-code-why.sh (see that file's own
# header note for the full citation): `hookSpecificOutput.additionalContext`
# on PreToolUse AND on SessionStart is the SAME field kb-memory's
# kb-recall.sh (UserPromptSubmit) and kb-wake.sh (SessionStart) already use
# in production — no new mechanism, just two more call sites of one already-
# verified contract.
# ---------------------------------------------------------------------------
#
# Deterministic and LLM-free, same discipline as kb-code-why.sh: fails OPEN
# everywhere. Any error (missing deps, daemon down, bad JSON, no repo match,
# no open flags) -> exit 0, nothing printed, the Edit/Write/session start
# proceeds unaffected.
#
# Env:
#   KB_CODE_ANNOTATIONS_HOOK=off|0|false|no   kill switch — exit 0
#                                              immediately. (Deliberately a
#                                              different value set than
#                                              kb-code-why.sh's
#                                              KB_CODE_WHY_HOOK=off|1|true|
#                                              yes — this hook's own spec.)
#   KB_CODE_DAEMON_URL                        kb-code daemon base (default
#                                              http://127.0.0.1:4747) — the
#                                              SAME var kb-code-why.sh uses,
#                                              so the two hooks share ONE
#                                              repos-cache file for the same
#                                              daemon url (see below) rather
#                                              than each keeping their own.
#   KB_CODE_TOKEN / KB_CODE_TOKEN_FILE        optional bearer for a
#                                              non-loopback publish (same
#                                              vars kb-code-why.sh uses).
#
# State:
#   ~/.cache/kb-code/repos-cache-<hash of the daemon url>.json
#     SHARED with kb-code-why.sh verbatim — same cache_dir + same "cksum of
#     the daemon url" hash formula, so this is literally the same file on
#     disk, never a second cache. Marker-style atomic write (.tmp -> mv),
#     5-minute TTL — copied from kb-code-why.sh rather than factored into a
#     shared lib (same "still works copied standalone" posture the rest of
#     this plugin's scripts follow).
#   ~/.cache/kb-code/annotations-hook-seen-<session_id>
#     One line per INJECTED annotation id (not per repo/path pair, unlike
#     kb-code-why.sh — an id is the right dedup grain here since a single
#     file can carry several distinct open flags). Its line count IS the
#     counter (global cap: 6/session). SessionStart's summary line writes
#     nothing here — it's not injecting an individual annotation, so
#     there's nothing to dedupe (mirrors kb-wake.sh, which never dedupes
#     its own summary either).

kill_switch="$(printf '%s' "${KB_CODE_ANNOTATIONS_HOOK:-}" | tr '[:upper:]' '[:lower:]')"
case "$kill_switch" in
off | 0 | false | no) exit 0 ;;
esac

command -v jq >/dev/null 2>&1 || exit 0
command -v curl >/dev/null 2>&1 || exit 0

input="$(cat)" || exit 0

mode="pretooluse"
case "${1:-}" in
--session-start) mode="session-start" ;;
esac

# Loopback daemons must not ride HTTP(S)_PROXY — same hygiene as
# kb-code-why.sh.
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy

kb_code_daemon="${KB_CODE_DAEMON_URL:-http://127.0.0.1:4747}"
# --max-time: 1.5s by default — a hook must NEVER block the agent; give-up-
# silently is the design. KB_CODE_HOOK_MAX_TIME exists for the TEST harness
# (a loaded parallel test box can push daemon responses past 1.5s, which
# flaked 6 hook tests on 2026-08-02 — prod default unchanged).
curl_common=(-fsS --max-time "${KB_CODE_HOOK_MAX_TIME:-1.5}" --noproxy "127.0.0.1,localhost")
if [ -n "${KB_CODE_TOKEN:-}" ]; then
  curl_common+=(-H "Authorization: Bearer ${KB_CODE_TOKEN}")
elif [ -n "${KB_CODE_TOKEN_FILE:-}" ] && [ -r "${KB_CODE_TOKEN_FILE}" ]; then
  _kb_code_tok="$(tr -d '\r\n' <"${KB_CODE_TOKEN_FILE}" 2>/dev/null)" || _kb_code_tok=""
  if [ -n "${_kb_code_tok}" ]; then
    curl_common+=(-H "Authorization: Bearer ${_kb_code_tok}")
  fi
  unset _kb_code_tok
fi

MAX_INJECTIONS=6
MAX_PER_FILE=2
INTENT="flag-for-agent"

cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb-code"
mkdir -p "$cache_dir" 2>/dev/null || exit 0

encode() { jq -rn --arg v "$1" '$v | @uri'; }

# --- repos list, 5-minute TTL, marker-style cache — SHARED with
# kb-code-why.sh (identical cache_dir/hash formula: same file). -------------
load_repos_json() {
  local daemon_hash repos_cache now_epoch fresh mtime age repos_body
  daemon_hash="$(printf '%s' "$kb_code_daemon" | cksum | cut -d' ' -f1)"
  repos_cache="$cache_dir/repos-cache-$daemon_hash.json"
  now_epoch="$(date +%s)"
  fresh=0
  if [ -f "$repos_cache" ]; then
    mtime="$(stat -c %Y "$repos_cache" 2>/dev/null || stat -f %m "$repos_cache" 2>/dev/null || echo 0)"
    age=$((now_epoch - mtime))
    [ "$age" -ge 0 ] && [ "$age" -lt 300 ] && fresh=1
  fi
  if [ "$fresh" -ne 1 ]; then
    repos_body="$(curl "${curl_common[@]}" "$kb_code_daemon/api/repos" 2>/dev/null)" || return 1
    [ -n "$repos_body" ] || return 1
    printf '%s' "$repos_body" >"$repos_cache.tmp" 2>/dev/null || return 1
    mv "$repos_cache.tmp" "$repos_cache" 2>/dev/null || return 1
  fi
  cat "$repos_cache" 2>/dev/null
}

# --- longest-prefix repo match against an absolute path, in jq (not bash
# parameter-expansion prefix stripping — see kb-code-why.sh's own comment on
# why: a repo path with glob-special characters can never be misread as a
# pattern). Prints `{name, rel}` JSON, or nothing on no match. -------------
match_repo() {
  local fp="$1"
  printf '%s' "$repos_json" | jq -c --arg fp "$fp" '
    (.repos // [])
    | map(.path |= rtrimstr("/"))
    | map(select(
        . as $r
        | ($fp == $r.path) or ($fp | startswith($r.path + "/"))
      ))
    | sort_by(.path | length)
    | last
    | if . == null then empty
      else { name: .name, rel: (if $fp == .path then "" else $fp[(.path | length) + 1:] end) }
      end
  ' 2>/dev/null
}

# =============================================================================
# SessionStart mode — one summary line, no per-annotation detail, no dedup.
# =============================================================================
if [ "$mode" = "session-start" ]; then
  cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)"
  [ -n "$cwd" ] || cwd="$PWD"
  case "$cwd" in
  /*) ;;
  *) exit 0 ;;
  esac

  repos_json="$(load_repos_json)" || exit 0
  [ -n "$repos_json" ] || exit 0

  match_json="$(match_repo "$cwd")"
  [ -n "$match_json" ] && [ "$match_json" != "null" ] || exit 0
  repo_name="$(printf '%s' "$match_json" | jq -r '.name // empty' 2>/dev/null)"
  [ -n "$repo_name" ] || exit 0

  open_url="$kb_code_daemon/api/annotations/open?repo=$(encode "$repo_name")&intent=$(encode "$INTENT")"
  open_body="$(curl "${curl_common[@]}" "$open_url" 2>/dev/null)" || exit 0
  [ -n "$open_body" ] || exit 0

  count="$(printf '%s' "$open_body" | jq -r '(.annotations // []) | length' 2>/dev/null)"
  [ -n "$count" ] || exit 0
  [ "$count" -gt 0 ] 2>/dev/null || exit 0

  summary="kb-code: ${count} operator flag(s) awaiting action in ${repo_name} — list: kb-code annotations open --repo ${repo_name} --intent flag-for-agent"

  jq -n --arg ctx "$summary" \
    '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $ctx}}' 2>/dev/null || exit 0
  exit 0
fi

# =============================================================================
# PreToolUse mode — per-edited-file open flags, deduped + capped per session.
# =============================================================================

tool_name="$(printf '%s' "$input" | jq -r '.tool_name // empty' 2>/dev/null)"
case "$tool_name" in
Edit | Write) ;;
*) exit 0 ;;
esac

session_id="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
[ -n "$session_id" ] || exit 0

file_path="$(printf '%s' "$input" | jq -r '.tool_input.file_path // empty' 2>/dev/null)"
[ -n "$file_path" ] || exit 0
# Non-absolute paths never match a configured repo's own absolute path
# (Windows is out of scope per this repo's own cross-platform stance — same
# as kb-code-why.sh).
case "$file_path" in
/*) ;;
*) exit 0 ;;
esac
[ -f "$file_path" ] || exit 0

seen_file="$cache_dir/annotations-hook-seen-$session_id"

# --- global cap: bail before any network call once the session is capped ---
seen_count=0
if [ -f "$seen_file" ]; then
  seen_count="$(wc -l <"$seen_file" 2>/dev/null | tr -d '[:space:]')"
  [ -n "$seen_count" ] || seen_count=0
fi
if [ "$seen_count" -ge "$MAX_INJECTIONS" ] 2>/dev/null; then
  exit 0
fi
remaining=$((MAX_INJECTIONS - seen_count))
[ "$remaining" -gt "$MAX_PER_FILE" ] && remaining="$MAX_PER_FILE"

repos_json="$(load_repos_json)" || exit 0
[ -n "$repos_json" ] || exit 0

match_json="$(match_repo "$file_path")"
[ -n "$match_json" ] && [ "$match_json" != "null" ] || exit 0
repo_name="$(printf '%s' "$match_json" | jq -r '.name // empty' 2>/dev/null)"
rel_path="$(printf '%s' "$match_json" | jq -r '.rel // empty' 2>/dev/null)"
[ -n "$repo_name" ] && [ -n "$rel_path" ] || exit 0

open_url="$kb_code_daemon/api/annotations/open?repo=$(encode "$repo_name")&intent=$(encode "$INTENT")"
open_body="$(curl "${curl_common[@]}" "$open_url" 2>/dev/null)" || exit 0
[ -n "$open_body" ] || exit 0

# --- ids already injected this session, so they're never repeated ---------
seen_ids_json="[]"
if [ -f "$seen_file" ]; then
  seen_ids_json="$(jq -R -s -c 'split("\n") | map(select(length > 0))' "$seen_file" 2>/dev/null)"
  [ -n "$seen_ids_json" ] || seen_ids_json="[]"
fi

# --- client-filter to THIS file's repo-relative path, drop already-seen
# ids, cap at the remaining per-call budget. ---------------------------------
matches_json="$(printf '%s' "$open_body" | jq -c --arg rp "$rel_path" --argjson seen "$seen_ids_json" --argjson n "$remaining" '
  (.annotations // [])
  | map(select(.id as $id | .path == $rp and (($seen | index($id)) == null)))
  | .[0:$n]
' 2>/dev/null)"
[ -n "$matches_json" ] && [ "$matches_json" != "null" ] && [ "$matches_json" != "[]" ] || exit 0

# --- format: one block per annotation, blank-line separated ----------------
context="$(printf '%s' "$matches_json" | jq -r --arg rp "$rel_path" --arg repo "$repo_name" '
  def clean: gsub("[\n\r\t]"; " ") | gsub(" +"; " ") | sub("^ "; "") | sub(" $"; "");
  def trunc200: if (length > 200) then (.[0:199] + "…") else . end;
  [ .[] |
    (.body // "" | clean | trunc200) as $body |
    (.line // 0) as $line |
    (.id // "") as $id |
    (.reply_count // 0) as $rc |
    ("kb-code operator flag on \($rp):\($line) (id \($id)):\n- \"\($body)\"\n- after addressing it: kb-code annotate reply \($id) -m \"<what you did>\" --repo \($repo) --path \($rp) && kb-code annotate resolve \($id)"
      + (if $rc > 0 then "\n- (thread has \($rc) replies — read them first: kb-code annotations \($rp) --repo \($repo))" else "" end))
  ] | join("\n\n")
' 2>/dev/null)"
[ -n "$context" ] || exit 0

jq -n --arg ctx "$context" \
  '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $ctx}}' 2>/dev/null || exit 0

printf '%s' "$matches_json" | jq -r '.[].id' 2>/dev/null >>"$seen_file" 2>/dev/null || true
