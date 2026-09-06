#!/usr/bin/env bash
# kb-code-why.sh — PreToolUse hook: inject trailer/exact-confidence
# provenance ("which session wrote this code") right before Claude edits a
# file, so it sees who/why before it touches the code.
#
# ---------------------------------------------------------------------------
# VERIFIED INJECTION MECHANISM (W5.3 step 1 — review-mandated)
#
# Checked against the current Claude Code hooks docs
# (https://code.claude.com/docs/en/hooks, redirected from
# docs.claude.com/en/docs/claude-code/hooks) AND cross-verified via the
# claude-code-guide agent: `PreToolUse` DOES support
# `hookSpecificOutput.additionalContext` (placed "next to the tool result" —
# i.e. injected right where the tool is about to run), matched on
# `tool_name` via a regex matcher ("Edit|Write" below), with the target file
# at `tool_input.file_path`. This is the SAME mechanism kb-memory's
# UserPromptSubmit `kb-recall.sh` already uses in production (`hookSpecific
# Output.hookEventName` + `additionalContext`), just on a different event.
# So the design target (PreToolUse on Edit/Write) IS the verified path —
# no UserPromptSubmit/PostToolUse fallback needed.
# ---------------------------------------------------------------------------
#
# Deterministic and LLM-free, same discipline as kb-recall.sh: fails OPEN
# everywhere. Any error (missing deps, daemon down, bad JSON, no match) ->
# exit 0, nothing printed, the Edit/Write proceeds unaffected. NEVER
# fabricates: only `confidence: trailer` or `confidence: exact` attributions
# are ever injected — a wrong session in context is worse than none, so
# `fuzzy`/`none` are always silent.
#
# Env:
#   KB_CODE_WHY_HOOK=off        kill switch — exit 0 immediately.
#   KB_CODE_DAEMON_URL          kb-code daemon base (default
#                                http://127.0.0.1:4747).
#   KB_CODE_TOKEN               optional bearer for a non-loopback publish
#                                (docker-proxy is not loopback to the
#                                daemon). Prefer KB_CODE_TOKEN_FILE.
#   KB_CODE_TOKEN_FILE          optional path to the same bearer (no
#                                newline). Used when KB_CODE_TOKEN is unset.
#   KB_DAEMON_URL                kb daemon base, used ONLY to build the
#                                "kb session" deep link (default
#                                http://127.0.0.1:4000) — same env var name
#                                kb-recall.sh already uses for the same
#                                daemon.
#
# State (per Claude Code session, capped + deduped — see the module doc in
# the W5.3 plan): `~/.cache/kb-code/why-hook-seen-<session_id>` records
# "<repo>\t<rel_path>" lines already injected this session (never repeated);
# its LINE COUNT doubles as the injection counter (global cap: 3/session).
# Repo list is cached 5 minutes in `~/.cache/kb-code/repos-cache-<hash of
# the daemon url>.json` (marker-style: write to .tmp, then atomic mv — same
# pattern kb-recall.sh's session marker uses).
#
# Deviation / known limitation: kb-code-server's `GET /api/why` response
# carries which kb CORPUS a resolved session lives in on the LINE-GRADE
# path only (`.attribution.kb` — see `provenance::why`'s `AttributionOut`);
# best-effort (absent when neither the join ladder nor its loopback-only
# enrichment follow-up resolved one) and NOT populated on the file-grade
# path (`.sessions[].kb`, no such field). The "kb session" link below is
# scoped with `?kb=` when `.attribution.kb` is present, else falls back to
# `?focus=` alone — best-effort, not always corpus-pre-scoped.

kill_switch="$(printf '%s' "${KB_CODE_WHY_HOOK:-}" | tr '[:upper:]' '[:lower:]')"
case "$kill_switch" in
off | 1 | true | yes) exit 0 ;;
esac

command -v jq >/dev/null 2>&1 || exit 0
command -v curl >/dev/null 2>&1 || exit 0

input="$(cat)" || exit 0

tool_name="$(printf '%s' "$input" | jq -r '.tool_name // empty' 2>/dev/null)"
case "$tool_name" in
Edit | Write) ;;
*) exit 0 ;;
esac

session_id="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
[ -n "$session_id" ] || exit 0

file_path="$(printf '%s' "$input" | jq -r '.tool_input.file_path // empty' 2>/dev/null)"
[ -n "$file_path" ] || exit 0
# No pre-existing file -> nothing to attribute (a brand-new Write, or an
# Edit that will fail anyway). Also rejects anything non-absolute up front
# ([[ pattern below only recognizes a leading "/"; Windows is out of scope
# per this repo's own cross-platform stance).
case "$file_path" in
/*) ;;
*) exit 0 ;;
esac
[ -f "$file_path" ] || exit 0

# Loopback daemons must not ride HTTP(S)_PROXY — same hygiene as
# kb-recall.sh.
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy

kb_code_daemon="${KB_CODE_DAEMON_URL:-http://127.0.0.1:4747}"
kb_daemon="${KB_DAEMON_URL:-http://127.0.0.1:4000}"
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

MAX_INJECTIONS=3

cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb-code"
mkdir -p "$cache_dir" 2>/dev/null || exit 0
seen_file="$cache_dir/why-hook-seen-$session_id"

# --- global cap -------------------------------------------------------------
if [ -f "$seen_file" ]; then
  seen_count="$(wc -l <"$seen_file" 2>/dev/null | tr -d '[:space:]')"
  [ -n "$seen_count" ] || seen_count=0
  if [ "$seen_count" -ge "$MAX_INJECTIONS" ] 2>/dev/null; then
    exit 0
  fi
fi

# --- repo list, 5-minute TTL, marker-style cache -----------------------------
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
  repos_body="$(curl "${curl_common[@]}" "$kb_code_daemon/api/repos" 2>/dev/null)" || exit 0
  [ -n "$repos_body" ] || exit 0
  printf '%s' "$repos_body" >"$repos_cache.tmp" 2>/dev/null || exit 0
  mv "$repos_cache.tmp" "$repos_cache" 2>/dev/null || exit 0
fi
repos_json="$(cat "$repos_cache" 2>/dev/null)" || exit 0
[ -n "$repos_json" ] || exit 0

# --- longest-prefix repo match + repo-relative path -------------------------
# Done in jq (not bash parameter-expansion prefix stripping) so a repo path
# containing glob-special characters can never be misread as a pattern.
match_json="$(printf '%s' "$repos_json" | jq -c --arg fp "$file_path" '
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
' 2>/dev/null)"
[ -n "$match_json" ] && [ "$match_json" != "null" ] || exit 0
repo_name="$(printf '%s' "$match_json" | jq -r '.name // empty' 2>/dev/null)"
rel_path="$(printf '%s' "$match_json" | jq -r '.rel // empty' 2>/dev/null)"
[ -n "$repo_name" ] && [ -n "$rel_path" ] || exit 0

# --- dedupe: never repeat an (repo, path) pair already injected this session
dedupe_key="$repo_name	$rel_path"
if [ -f "$seen_file" ] && grep -qxF "$dedupe_key" "$seen_file" 2>/dev/null; then
  exit 0
fi

# --- best-effort line number (Edit only) — locate old_string's first
# non-blank line in the CURRENT on-disk file (pre-edit, since PreToolUse
# fires before the tool runs) via a fixed-string grep. Falls back to a
# file-grade query when it can't be found (new content, ambiguous, Write).
line=""
if [ "$tool_name" = "Edit" ]; then
  old_string="$(printf '%s' "$input" | jq -r '.tool_input.old_string // empty' 2>/dev/null)"
  if [ -n "$old_string" ]; then
    first_line="$(printf '%s' "$old_string" | awk 'NF{print; exit}')"
    if [ -n "$first_line" ]; then
      line="$(grep -n -F -m1 -- "$first_line" "$file_path" 2>/dev/null | head -n1 | cut -d: -f1)"
    fi
  fi
fi

# --- the why query ------------------------------------------------------
encode() { jq -rn --arg v "$1" '$v | @uri'; }
why_url="$kb_code_daemon/api/why?repo=$(encode "$repo_name")&path=$(encode "$rel_path")"
if [ -n "$line" ]; then
  why_url="$why_url&line=$(encode "$line")"
fi
why_body="$(curl "${curl_common[@]}" "$why_url" 2>/dev/null)" || exit 0
[ -n "$why_body" ] || exit 0

jget() { printf '%s' "$why_body" | jq -r "$1" 2>/dev/null; }

is_line_grade="$(jget 'has("attribution")')"
if [ "$is_line_grade" = "true" ]; then
  confidence="$(jget '.attribution.confidence // "none"')"
  hit_session_id="$(jget '.attribution.session_id // empty')"
  display_name="$(jget '.attribution.display_name // empty')"
  hit_kb="$(jget '.attribution.kb // empty')"
  author_time="$(jget '.region.author_time // empty')"
  decision="$(jget '(.kb_context.decisions[0].prompt // .kb_context.decisions[0].answer) // empty')"
else
  confidence="$(jget '.sessions[0].confidence // "none"')"
  hit_session_id="$(jget '.sessions[0].session_id // empty')"
  display_name="$(jget '.sessions[0].display_name // empty')"
  hit_kb=""
  author_time=""
  decision=""
fi

# --- confidence gate: trailer/exact ONLY, never fuzzy/none -------------------
case "$confidence" in
trailer | exact) ;;
*) exit 0 ;;
esac
[ -n "$hit_session_id" ] || exit 0

# --- build the 3-5 line context block ---------------------------------------
sanitize() {
  # collapse to one line, trim, cap length.
  local s max
  s="$(printf '%s' "$1" | tr '\n\t\r' '   ' | tr -s ' ')"
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  max="${2:-180}"
  if [ "${#s}" -gt "$max" ]; then
    s="${s:0:$((max - 1))}…"
  fi
  printf '%s' "$s"
}

name="$display_name"
[ -n "$name" ] || name="session $hit_session_id"
name="$(sanitize "$name" 120)"

date_str=""
if [ -n "$author_time" ] && [ "$author_time" != "null" ]; then
  date_str="$(date -d "@$author_time" '+%Y-%m-%d' 2>/dev/null || date -r "$author_time" '+%Y-%m-%d' 2>/dev/null || echo "")"
fi

link="$kb_daemon/sessions?focus=$(encode "$hit_session_id")"
if [ -n "$hit_kb" ] && [ "$hit_kb" != "null" ]; then
  link="$link&kb=$(encode "$hit_kb")"
fi

block="kb-code provenance — ${rel_path}:"
if [ -n "$date_str" ]; then
  block="$block
- Session: ${name} (committed ${date_str})"
else
  block="$block
- Session: ${name}"
fi
if [ -n "$decision" ] && [ "$decision" != "null" ]; then
  block="$block
- Decision: $(sanitize "$decision" 200)"
fi
block="$block
- kb session: ${link}"

jq -n --arg ctx "$block" \
  '{hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $ctx}}' 2>/dev/null || exit 0

printf '%s\n' "$dedupe_key" >>"$seen_file" 2>/dev/null || true
