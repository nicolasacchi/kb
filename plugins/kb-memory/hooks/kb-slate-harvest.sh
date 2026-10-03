#!/usr/bin/env bash
# kb-slate-harvest.sh — SL5: the grokclaude dispatcher's slate HARVEST
# adapter, the capture-adapter pattern (kb-capture-*.sh) applied to
# `kb-slate/1` instead of sessions. Design of record:
# docs/research/kb-slate-design-2026-09.html §12 "the dispatcher bridge".
#
# Invoked as `bash kb-slate-harvest.sh <job_dir> [--abandoned "<reason>"]`
# from grokclaude's `trigger_kb_slate`/`trigger_kb_slate_abandoned`
# (src/engine.rs), fire-and-forget, NEVER on the critical path of a job
# finishing — every failure mode below is a silent no-op, never a non-zero
# exit the caller could mistake for something worth surfacing.
#
# Normal mode (no `--abandoned`): reads `<job_dir>/meta.json` and
# `<job_dir>/report.json` with jq and posts ONE `found` per job (v0.44 W5b):
#   `job <id> (<title>): N findings, M questions — <top finding>`
# whose body lists up to 8 finding lines (+ up to 5 questions) and whose refs
# are `job:<id>` + `path:<job_dir>/report.json` (the fine-grained findings
# stay in report.json, reachable through that ref). The "top finding" is
# picked deterministically: observed-with-evidence first, then observed,
# then modeled/recommendation, each in report order.
#   - IDEMPOTENT: the `slate open --all --json` digest (already fetched for
#     the take lookup) is searched for an existing `found` carrying the same
#     `job:<id>` ref. Same line -> nothing is posted; a changed summary is
#     posted with `--supersedes <that seq>` instead of appended again.
#   - `open_questions` become an `ask` ONLY when the report marks them as
#     blocking a human decision (an object with `"blocking": true`, or an
#     entry of `blocking_questions[]`); a trailing `?` is appended if the
#     worker forgot one (the daemon 400s an ask without it). They are
#     skipped if an open ask with the same line already exists.
#   - `meta.json`'s `findings_error` (if any) is NOT posted as `tried`
#     (infrastructure noise, not a dead end): it becomes the reason of the
#     closing `kb slate done <seq> "…" --abandoned "findings error: …"`.
#   otherwise the take `post_kb_slate_take` opened at spawn is closed with a
#   plain `kb slate done <seq> "…"` (Finished; never live again).
#
# `--abandoned "<reason>"` mode (the three bypass exits — reap,
# session_close, a session-round worker failure — that never reach
# finish_ok_job/fail_job and so never run the normal harvest above): skips
# straight to `kb slate done <seq> "…" --abandoned "<reason>"`, which the
# daemon leaves OPEN but off the live path (D5: a `--abandoned` take still
# ages to `expired` on the ordinary silence clock — no daemon sweeper is
# needed or wanted; see rules matrix "Re-target matrix").
#
# The take's post number is never stored anywhere (not in meta.json, not
# in an env var): both modes re-resolve it fresh, every time, from
# `kb slate open --all --json`'s take section, matching on the SAME
# `job:<id>` ref `post_kb_slate_take` attached at spawn. No `kb` or no
# `jq` on PATH, a missing/malformed job_dir, or any daemon error along the
# way all degrade to a quiet no-op — this script has no caller to report
# failure to.
set -u

job_dir="${1:-}"
shift || true
abandoned_reason=""
if [ "${1:-}" = "--abandoned" ]; then
  abandoned_reason="${2:-abandoned}"
fi

command -v kb >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0
[ -n "$job_dir" ] && [ -d "$job_dir" ] || exit 0

meta="$job_dir/meta.json"
[ -f "$meta" ] || exit 0

job_id="$(jq -r '.id // empty' "$meta" 2>/dev/null)"
[ -n "$job_id" ] || exit 0

# W5/R8's own fake-worker convention: a fixture/test session id is
# prefixed `fake-`. Never touch the slate for one.
session_id="$(jq -r '.session_id // .grok_session_id // empty' "$meta" 2>/dev/null)"
case "$session_id" in
fake-*) exit 0 ;;
esac

backend="$(jq -r '.backend // "grok"' "$meta" 2>/dev/null)"
cwd="$(jq -r '.cwd // empty' "$meta" 2>/dev/null)"

cwd_args=()
[ -n "$cwd" ] && cwd_args=(--cwd "$cwd")
sid_args=()
[ -n "$session_id" ] && sid_args=(--session-id "$session_id")

# Cap a line at 190 chars (the wire cap is 200; leave room for the daemon's
# own formatting) without pretending to be UTF-8-exact — a best-effort
# bash bridge, not the daemon's own byte-precise validator.
trunc() {
  local s="$1"
  if [ "${#s}" -gt 190 ]; then
    printf '%s…' "${s:0:189}"
  else
    printf '%s' "$s"
  fi
}

post() { # kind line [extra kb-slate args...]
  local kind="$1"
  shift
  local line="$1"
  shift
  kb slate "$kind" "$line" --harness "$backend" --origin import --job "$job_id" \
    --ref "job:$job_id" "${cwd_args[@]}" "${sid_args[@]}" "$@" >/dev/null 2>&1
}

# Flatten whitespace so a multi-line claim stays one line.
flat() { printf '%s' "$1" | tr '\n\t' '  ' | tr -s ' '; }

# One digest fetch serves the idempotency lookup AND the take lookup (design
# §12: nothing is ever stored to find a post again).
digest_json="$(kb slate open --all --json "${cwd_args[@]}" 2>/dev/null)" || digest_json=""

findings_error=""
if [ -z "$abandoned_reason" ]; then
  findings_error="$(jq -r '.findings_error // empty' "$meta" 2>/dev/null)"
  report="$job_dir/report.json"
  if [ -f "$report" ]; then
    n_find="$(jq -r '[.findings[]?] | length' "$report" 2>/dev/null)"
    n_q="$(jq -r '[.open_questions[]?] | length' "$report" 2>/dev/null)"
    n_find="${n_find:-0}"
    n_q="${n_q:-0}"
    if [ "$n_find" -gt 0 ] || [ "$n_q" -gt 0 ]; then
      title="$(flat "$(jq -r '.title // .brief // .name // empty' "$meta" 2>/dev/null)")"
      [ "${#title}" -gt 40 ] && title="${title:0:39}…"
      # deterministic top finding: observed+evidence, observed, the rest
      top="$(flat "$(jq -r '[.findings[]?] | sort_by(
          if (.claim_type // "observed") == "observed"
             and ((.evidence[0].path // "") != "") then 0
          elif (.claim_type // "observed") == "observed" then 1
          else 2 end) | (.[0].claim // empty)' "$report" 2>/dev/null)")"
      head_txt="job $job_id"
      [ -n "$title" ] && head_txt="$head_txt ($title)"
      line="${head_txt}: ${n_find} findings, ${n_q} questions"
      [ -n "$top" ] && line="$line — $top"
      line="$(trunc "$line")"

      body="$(jq -r '
        def flat: gsub("\\s+"; " ");
        def q: if type == "string" then . else (.question // .text // "") end;
        ([.findings[]? | "- [" + (.claim_type // "observed") + "] " + ((.claim // "") | flat)
            + (if ((.evidence[0].path // "") != "") then " (" + .evidence[0].path + ")" else "" end)]
          | .[:8]) as $f
        | ([.open_questions[]? | "- " + (q | flat)] | .[:5]) as $q
        | ($f | join("\n"))
          + (if ($q | length) > 0 then "\n\nQuestions:\n" + ($q | join("\n")) else "" end)' \
        "$report" 2>/dev/null)"
      body="${body}"$'\n\n'"Full report: $job_dir/report.json"
      [ "${#body}" -gt 1900 ] && body="${body:0:1899}…"

      prev_seq=""
      prev_line=""
      if [ -n "$digest_json" ]; then
        prev="$(printf '%s' "$digest_json" | jq -r --arg ref "job:$job_id" \
          '[(.sections.found_idea // [])[] | select(.kind == "found")
            | select(any((.refs // [])[]?; .raw == $ref))] | sort_by(.seq) | last
            | if . == null then empty else "\(.seq)\t\(.line)" end' 2>/dev/null)"
        prev_seq="${prev%%$'\t'*}"
        [ -n "$prev" ] && prev_line="${prev#*$'\t'}"
      fi

      if [ -n "$prev_seq" ] && [ "$prev_line" = "$line" ]; then
        : # already harvested with the same summary: nothing to append
      elif [ -n "$prev_seq" ]; then
        post found "$line" --ref "path:$job_dir/report.json" --body="$body" --supersedes "$prev_seq"
      else
        post found "$line" --ref "path:$job_dir/report.json" --body="$body"
      fi
    fi

    # Only questions the report marks as blocking a human decision -> ask.
    while IFS= read -r q; do
      [ -n "$q" ] || continue
      line="$(trunc "$(flat "$q")")"
      case "$line" in
      *\?) : ;;
      *) line="${line}?" ;;
      esac
      if [ -n "$digest_json" ] && printf '%s' "$digest_json" | jq -e --arg l "$line" \
        'any((.sections.ask // [])[]?; .line == $l)' >/dev/null 2>&1; then
        continue
      fi
      post ask "$line"
    done < <(jq -r '(.blocking_questions[]? ),
      (.open_questions[]? | select(type == "object" and .blocking == true)
        | (.question // .text // empty))' "$report" 2>/dev/null)
  fi
fi

# Close the take `post_kb_slate_take` opened at spawn (subject job:<id>,
# ref job:<id>) — resolve its post number from the digest fetched above.
[ -n "$digest_json" ] || exit 0
take_seq="$(printf '%s' "$digest_json" | jq -r --arg ref "job:$job_id" \
  '(.sections.take // [])[] | select((.refs // [])[]?.raw == $ref) | .seq' 2>/dev/null | head -n1)"
[ -n "$take_seq" ] || exit 0

if [ -n "$abandoned_reason" ]; then
  kb slate done "$take_seq" "job $job_id: $abandoned_reason" --abandoned "$abandoned_reason" \
    --harness "$backend" "${cwd_args[@]}" "${sid_args[@]}" >/dev/null 2>&1 || true
elif [ -n "$findings_error" ]; then
  reason="$(trunc "findings error: $(flat "$findings_error")")"
  kb slate done "$take_seq" "job $job_id: $reason" --abandoned "$reason" \
    --harness "$backend" "${cwd_args[@]}" "${sid_args[@]}" >/dev/null 2>&1 || true
else
  kb slate done "$take_seq" "job $job_id finished" \
    --harness "$backend" "${cwd_args[@]}" "${sid_args[@]}" >/dev/null 2>&1 || true
fi

exit 0
