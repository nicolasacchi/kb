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
# `<job_dir>/report.json` with jq and posts, in order:
#   - `findings[]` with `claim_type` absent or `"observed"` -> `found`
#     (with `--ref path:<first evidence path>`; a finding with no evidence
#     path degrades to `idea` instead of risking the daemon's "found needs
#     a ref" 400 on an otherwise-good harvest)
#   - `findings[]` with `claim_type` `"modeled"` or `"recommendation"` -> `idea`
#   - `open_questions[]` -> `ask` (a trailing `?` is appended if the worker
#     forgot one — the daemon 400s an ask without it)
#   - `meta.json`'s `findings_error` (if any) -> `tried … --failed …`
#   then closes the take `post_kb_slate_take` opened at spawn with a plain
#   `kb slate done <seq> "…"` (Finished; never live again).
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
    --ref "job:$job_id" "${cwd_args[@]}" "${sid_args[@]}" "$@" >/dev/null 2>&1 || true
}

if [ -z "$abandoned_reason" ]; then
  report="$job_dir/report.json"
  if [ -f "$report" ]; then
    # observed findings -> found (needs a ref; degrades to idea without one)
    while IFS=$'\t' read -r claim ev_path; do
      [ -n "$claim" ] || continue
      line="$(trunc "$claim")"
      if [ -n "$ev_path" ]; then
        post found "$line" --ref "path:$ev_path"
      else
        post idea "$line"
      fi
    done < <(jq -r '.findings[]? | select((.claim_type // "observed") == "observed") |
      [(.claim // ""), (.evidence[0].path // "")] | @tsv' "$report" 2>/dev/null)

    # modeled / recommendation -> idea
    while IFS= read -r claim; do
      [ -n "$claim" ] || continue
      post idea "$(trunc "$claim")"
    done < <(jq -r '.findings[]? | select((.claim_type // "") == "modeled" or
      (.claim_type // "") == "recommendation") | (.claim // empty)' "$report" 2>/dev/null)

    # open_questions -> ask (must end in "?")
    while IFS= read -r q; do
      [ -n "$q" ] || continue
      line="$(trunc "$q")"
      case "$line" in
      *\?) : ;;
      *) line="${line}?" ;;
      esac
      post ask "$line"
    done < <(jq -r '.open_questions[]? // empty' "$report" 2>/dev/null)
  fi

  findings_error="$(jq -r '.findings_error // empty' "$meta" 2>/dev/null)"
  if [ -n "$findings_error" ]; then
    post tried "job $job_id findings error" --failed "$(trunc "$findings_error")"
  fi
fi

# Close the take `post_kb_slate_take` opened at spawn (subject job:<id>,
# ref job:<id>) — resolve its post number fresh every time; nothing is
# ever stored to look it up (design §12 "take on spawn").
digest_json="$(kb slate open --all --json "${cwd_args[@]}" 2>/dev/null)" || digest_json=""
[ -n "$digest_json" ] || exit 0
take_seq="$(printf '%s' "$digest_json" | jq -r --arg ref "job:$job_id" \
  '(.sections.take // [])[] | select((.refs // [])[]?.raw == $ref) | .seq' 2>/dev/null | head -n1)"
[ -n "$take_seq" ] || exit 0

if [ -n "$abandoned_reason" ]; then
  kb slate done "$take_seq" "job $job_id: $abandoned_reason" --abandoned "$abandoned_reason" \
    --harness "$backend" "${cwd_args[@]}" "${sid_args[@]}" >/dev/null 2>&1 || true
else
  kb slate done "$take_seq" "job $job_id finished" \
    --harness "$backend" "${cwd_args[@]}" "${sid_args[@]}" >/dev/null 2>&1 || true
fi

exit 0
