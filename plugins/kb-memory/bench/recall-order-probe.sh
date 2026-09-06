#!/usr/bin/env bash
# recall-order-probe.sh — MR2 (SL6): does the POSITION of a memory inside the
# injected recall block change whether a model can use it?
#
# The design (docs/research/kb-slate-design-2026-09.html §11) calls order "the
# one free lever nobody has measured here". This is the instrument. It renders
# ONE frozen recall pack in each of kb-recall.sh's three layouts, buries the
# block behind ~20K tokens of real transcript so it sits where a hook injection
# sits, asks a question answerable only from the hit at rank k, and scores a
# normalized exact match.
#
#   packs × layouts × k × models
#     30   ×    3    × 3 ×   2     = 540 calls (the full run)
#      5   ×    3    × 3 ×   1     =  45 calls (--reduced, proves the harness)
#
# The renderer is `kb-recall.sh` ITSELF, driven with a fake `kb` on PATH that
# serves the frozen pack. There is deliberately no second implementation of the
# layouts here: the probe must measure the shipping bytes or it measures
# nothing.
#
# RESUMABLE by construction: one `results.jsonl` line per call, keyed on
# `<query_id>|<layout>|<k>|<model>`; a key already present is skipped. Kill it
# and re-run — it picks up where it stopped.
#
# `kb bench` is NOT the instrument for this and is not extended: it measures
# Recall@k / MRR / nDCG over ids and cannot see model behaviour.
#
# Usage:
#   ./recall-order-probe.sh --reduced            # 5 packs, claude only
#   ./recall-order-probe.sh                      # all 30 packs, both models
#   ./recall-order-probe.sh --models claude      # one model, all packs
#   ./recall-order-probe.sh --report-only        # rebuild report.md from results
#   ./recall-order-probe.sh --verify             # re-check the input invariants
#
# Env:
#   PROBE_OUT        output dir (default: ./run)
#   PROBE_CLAUDE_MODEL   default haiku
#   PROBE_CODEX_MODEL    default: codex's own configured model
#   PROBE_IO_CEILING     /proc/pressure/io full avg10 above which the probe
#                        waits (default 40 — the host throttle this box runs
#                        every build under)
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
RECALL="$HERE/../hooks/kb-recall.sh"
QUERIES="$HERE/probe-queries.jsonl"
QUESTIONS="$HERE/probe-questions.jsonl"
FILLER="$HERE/probe-filler.txt"
OUT="${PROBE_OUT:-$HERE/run}"
RESULTS="$OUT/results.jsonl"
REPORT="$OUT/report.md"
CLAUDE_MODEL="${PROBE_CLAUDE_MODEL:-haiku}"
CLAUDE2_MODEL="${PROBE_CLAUDE2_MODEL:-sonnet}"
IO_CEILING="${PROBE_IO_CEILING:-40}"

LAYOUTS="v1 v2 v2-last"
MODELS="claude codex"
# `claude2` is a SECOND Claude tier, used when the second-tokenizer lane
# (codex) is unavailable. It is a weaker substitute and the report never
# pretends otherwise: same tokenizer, same family, different capability.
MAX_PACKS=0
REPORT_ONLY=0
VERIFY_ONLY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --reduced) MAX_PACKS=5; MODELS="claude" ;;
    --packs) MAX_PACKS="$2"; shift ;;
    --models) MODELS="$(printf '%s' "$2" | tr ',' ' ')"; shift ;;
    --layouts) LAYOUTS="$(printf '%s' "$2" | tr ',' ' ')"; shift ;;
    --report-only) REPORT_ONLY=1 ;;
    --verify) VERIFY_ONLY=1 ;;
    -h | --help) sed -n '2,40p' "$0"; exit 0 ;;
    *) printf 'unknown flag %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

for f in "$RECALL" "$QUERIES" "$QUESTIONS" "$FILLER"; do
  [ -r "$f" ] || { printf 'missing input: %s\n' "$f" >&2; exit 2; }
done
command -v jq >/dev/null || { echo "jq required" >&2; exit 2; }
mkdir -p "$OUT"
: >>"$RESULTS"

# ─── input invariants ────────────────────────────────────────────────────────
# Re-checked on every run (cheap, and a silently-broken question set would
# produce a confident, meaningless verdict). Each answer must appear EXACTLY
# once across its whole pack and NOWHERE in the filler; the source hit must
# actually sit at rank k.
verify_inputs() {
  local bad=0 line qid k ans src
  while IFS= read -r line; do
    qid="$(printf '%s' "$line" | jq -r '.query_id')"
    k="$(printf '%s' "$line" | jq -r '.k')"
    ans="$(printf '%s' "$line" | jq -r '.answer')"
    src="$(printf '%s' "$line" | jq -r '.source_id')"
    local pack n
    pack="$(jq -r --arg q "$qid" 'select(.query_id==$q)|[.pack[]|(.title + " " + (.summary // "") + " " + .kb)]|join(" ")' "$QUERIES")"
    n="$(printf '%s' "$pack" | grep -oiwF -- "$ans" | wc -l)"
    [ "$n" = "1" ] || { printf 'NOT UNIQUE IN PACK: %s k=%s %s (%s hits)\n' "$qid" "$k" "$ans" "$n" >&2; bad=1; }
    if grep -qiwF -- "$ans" "$FILLER"; then
      printf 'LEAKS INTO FILLER: %s k=%s %s\n' "$qid" "$k" "$ans" >&2; bad=1
    fi
    local rank
    rank="$(jq -r --arg q "$qid" --arg s "$src" 'select(.query_id==$q)|[.pack[].id]|index($s)+1' "$QUERIES")"
    [ "$rank" = "$k" ] || { printf 'SOURCE NOT AT RANK: %s k=%s (at %s)\n' "$qid" "$k" "$rank" >&2; bad=1; }
  done <"$QUESTIONS"
  [ "$bad" = "0" ] && echo "input invariants: OK ($(wc -l <"$QUESTIONS") questions)"
  return "$bad"
}

if [ "$VERIFY_ONLY" = "1" ]; then
  verify_inputs
  exit $?
fi

# ─── host throttle ───────────────────────────────────────────────────────────
# This box is IO-bound on spinning RAID5 and other sessions build on it. Wait
# rather than pile on. Bounded at 5 minutes per check, and called once per
# (pack × layout) — nine calls — rather than per call: a per-call check that
# sat at exactly the ceiling would multiply a 540-call run by the wait, which
# is how a courtesy becomes a hang.
wait_for_io() {
  local waited=0 full
  while [ "$waited" -lt 300 ]; do
    full="$(awk '/^full/{for(i=1;i<=NF;i++) if ($i ~ /^avg10=/) {sub(/avg10=/,"",$i); print int($i)}}' /proc/pressure/io 2>/dev/null)"
    [ -n "$full" ] || return 0
    [ "$full" -lt "$IO_CEILING" ] && return 0
    printf '  … io pressure full avg10=%s ≥ %s, waiting 60s\n' "$full" "$IO_CEILING" >&2
    sleep 60
    waited=$((waited + 60))
  done
  return 0
}

# ─── the renderer: kb-recall.sh itself, fed the frozen pack ─────────────────
FAKEBIN="$(mktemp -d "${TMPDIR:-/tmp}/kb-probe-bin.XXXXXX")"
PACKFILE="$FAKEBIN/pack.json"
cat >"$FAKEBIN/kb" <<EOF
#!/usr/bin/env bash
if [ "\$1" = "recall" ]; then cat "$PACKFILE"; exit 0; fi
exit 0
EOF
chmod +x "$FAKEBIN/kb"
cleanup() { rm -rf "$FAKEBIN"; }
trap cleanup EXIT

render_block() { # $1=pack json, $2=layout
  printf '%s' "$1" >"$PACKFILE"
  printf '%s' '{"session_id":"probe","cwd":"/tmp","prompt":"probe"}' \
    | PATH="$FAKEBIN:$PATH" XDG_CACHE_HOME="$FAKEBIN/cache" \
      KB_HOOK_FMT=kimi KB_RECALL_LAYOUT="$2" "$RECALL" 2>/dev/null
}

# ─── scoring: normalized exact match ────────────────────────────────────────
# Case-folded, punctuation-stripped, whitespace-collapsed. A model that
# answers "RATIFIED." or " ratified " is right; one that answers "the word is
# ratified" is not, and the prompt says so explicitly.
normalize() {
  printf '%s' "$1" \
    | tr '[:upper:]' '[:lower:]' \
    | tr -d '"'"'"'`*_.,;:!?()[]{}' \
    | tr -s '[:space:]' ' ' \
    | sed 's/^ *//; s/ *$//'
}

# ─── model runners ──────────────────────────────────────────────────────────
# Both are pinned NON-THINKING and hermetic: KB_DAEMON_URL points at a dead
# port so this repo's own kb hooks fail silently instead of injecting a REAL
# recall block into the probe's context (that contamination would invalidate
# every number here), KB_SESSIONS_DIR is cleared so no probe turn is captured
# into the sessions corpus, and KB_BEAT=0 so the live registry never sees one.
PROBE_ENV=(env KB_DAEMON_URL=http://127.0.0.1:1 KB_SESSIONS_DIR= KB_BEAT=0)

run_claude() { # stdin = prompt
  timeout 300 "${PROBE_ENV[@]}" claude -p \
    --model "$CLAUDE_MODEL" \
    --strict-mcp-config \
    --no-session-persistence 2>/dev/null
}

run_claude2() { # stdin = prompt
  timeout 300 "${PROBE_ENV[@]}" claude -p \
    --model "$CLAUDE2_MODEL" \
    --strict-mcp-config \
    --no-session-persistence 2>/dev/null
}

run_codex() { # stdin = prompt
  local last="$FAKEBIN/codex-last.txt" args=()
  : >"$last"
  [ -n "${PROBE_CODEX_MODEL:-}" ] && args+=(-m "$PROBE_CODEX_MODEL")
  timeout 600 "${PROBE_ENV[@]}" codex exec \
    --sandbox read-only --skip-git-repo-check \
    "${args[@]}" -o "$last" - >/dev/null 2>&1
  cat "$last" 2>/dev/null
}

# A model counts as available only when it actually ANSWERS. `codex` in
# particular can be installed, on PATH and out of quota — a state that looks
# healthy to `command -v` and returns an empty string to every call, which
# would have filled the run with 270 errors before anyone noticed.
model_available() {
  case "$1" in
    claude | claude2) command -v claude >/dev/null || return 1 ;;
    codex) { command -v codex >/dev/null && command -v codexclaude >/dev/null; } || return 1 ;;
    *) return 1 ;;
  esac
  local probe
  probe="$(printf '%s' 'Reply with exactly the word: PONG' | "run_$1" 2>/dev/null)"
  [ -n "$(printf '%s' "$probe" | tr -d '[:space:]')" ]
}

model_label() {
  case "$1" in
    claude) printf 'claude/%s' "$CLAUDE_MODEL" ;;
    claude2) printf 'claude/%s' "$CLAUDE2_MODEL" ;;
    codex) printf 'codex/%s' "${PROBE_CODEX_MODEL:-default}" ;;
    *) printf '%s' "$1" ;;
  esac
}

# ─── the run ────────────────────────────────────────────────────────────────
have_key() { grep -qF "\"key\":\"$1\"" "$RESULTS"; }

ACTIVE=""
for m in $MODELS; do
  if model_available "$m"; then
    ACTIVE="$ACTIVE $m"
  else
    printf 'model %s unavailable (not installed, or installed and not answering — e.g. out of quota) — skipped, and the report says so\n' "$m" >&2
  fi
done
ACTIVE="${ACTIVE# }"
[ -n "$ACTIVE" ] || { echo "no model available" >&2; exit 2; }

if [ "$REPORT_ONLY" = "0" ]; then
  verify_inputs || { echo "input invariants failed — refusing to run" >&2; exit 3; }
  filler="$(cat "$FILLER")"
  qids="$(jq -r '.query_id' "$QUERIES")"
  [ "$MAX_PACKS" -gt 0 ] && qids="$(printf '%s\n' "$qids" | head -n "$MAX_PACKS")"
  total=0
  done_n=0
  for qid in $qids; do
    pack="$(jq -c --arg q "$qid" 'select(.query_id==$q)|{hits:.pack}' "$QUERIES")"
    for layout in $LAYOUTS; do
      block="$(render_block "$pack" "$layout")"
      [ -n "$block" ] || { printf 'EMPTY BLOCK %s/%s — skipping\n' "$qid" "$layout" >&2; continue; }
      wait_for_io
      while IFS= read -r qline; do
        k="$(printf '%s' "$qline" | jq -r '.k')"
        question="$(printf '%s' "$qline" | jq -r '.question')"
        expected="$(printf '%s' "$qline" | jq -r '.answer')"
        source_id="$(printf '%s' "$qline" | jq -r '.source_id')"
        for model in $ACTIVE; do
          total=$((total + 1))
          key="$qid|$layout|$k|$model"
          have_key "$key" && continue
          prompt="$filler

$block

$question"
          started="$(date +%s)"
          raw="$(printf '%s' "$prompt" | "run_$model")"
          # An EMPTY answer is a transport failure (a timeout, a refusal, a
          # dropped last-message file), not a wrong answer. Retry once, then
          # record it as an ERROR: scoring it as a miss would silently
          # penalise whichever model happened to be flakier that hour, which
          # is exactly the confound this probe must not have.
          if [ -z "$(printf '%s' "$raw" | tr -d '[:space:]')" ]; then
            sleep 5
            raw="$(printf '%s' "$prompt" | "run_$model")"
          fi
          elapsed=$(($(date +%s) - started))
          got="$(normalize "$raw")"
          want="$(normalize "$expected")"
          if [ -z "$got" ]; then
            error=true
            correct=false
          else
            error=false
            if [ "$got" = "$want" ]; then correct=true; else correct=false; fi
          fi
          jq -nc --arg key "$key" --arg query_id "$qid" --arg layout "$layout" \
            --argjson k "$k" --arg model "$model" --arg source_id "$source_id" \
            --arg expected "$expected" --arg got "$got" --argjson correct "$correct" \
            --arg model_label "$(model_label "$model")" \
            --argjson error "$error" \
            --argjson elapsed_s "$elapsed" --argjson prompt_chars "${#prompt}" \
            --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            '{key:$key,query_id:$query_id,layout:$layout,k:$k,model:$model,
              model_label:$model_label,source_id:$source_id,expected:$expected,
              got:$got,correct:$correct,error:$error,elapsed_s:$elapsed_s,
              prompt_chars:$prompt_chars,at:$at}' >>"$RESULTS"
          done_n=$((done_n + 1))
          printf '%s  %-7s k=%s %-6s %ss  %s\n' "$qid" "$layout" "$k" "$model" "$elapsed" \
            "$(if [ "$error" = "true" ]; then echo "! empty answer (recorded as error, not a miss)";
               elif [ "$correct" = "true" ]; then echo ✓;
               else echo "✗ want=$want got=${got:0:40}"; fi)"
        done
      done < <(jq -c --arg q "$qid" 'select(.query_id==$q)' "$QUESTIONS")
    done
  done
  printf '\n%s new call(s) of %s planned; results in %s\n' "$done_n" "$total" "$RESULTS"
fi

# ─── report ─────────────────────────────────────────────────────────────────
# Percentages only, with the n they came from. No verdict is computed here
# beyond the design's own rule, which is stated as arithmetic: the default
# flips to v2-last only on a >= 5 point win over v2 on EVERY model that ran.
{
  packs="$(jq -r '.query_id' "$RESULTS" | sort -u | wc -l)"
  calls="$(wc -l <"$RESULTS")"
  models_run="$(jq -r '.model' "$RESULTS" | sort -u | tr '\n' ' ')"
  layouts_run="$(jq -r '.layout' "$RESULTS" | sort -u | tr '\n' ' ')"
  printf '# Recall order probe — results\n\n'
  printf -- '- run at: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf -- '- packs: %s · calls: %s · models: %s · layouts: %s\n' \
    "$packs" "$calls" "$models_run" "$layouts_run"
  printf -- '- lanes: %s\n' "$(jq -r '.model + " (" + (.model_label // "?") + ")"' "$RESULTS" | sort -u | tr '\n' ' ')"
  printf -- '- filler: %s chars (~20K tokens) from one captured session transcript\n\n' \
    "$(wc -c <"$FILLER")"

  printf '## Accuracy by layout × model\n\n'
  printf 'Accuracy is over SCORED calls — a call whose model returned nothing '
  printf '(a transport failure, retried once) is counted as an error, never as a miss.\n\n'
  printf '| model | layout | correct | scored | %% | errors |\n|---|---|---:|---:|---:|---:|\n'
  jq -rs 'group_by(.model + "|" + .layout)[]
          | (map(select(.error != true))) as $sc
          | {m:.[0].model, l:.[0].layout, c:($sc|map(select(.correct))|length),
             n:($sc|length), e:(map(select(.error == true))|length)}
          | "| \(.m) | \(.l) | \(.c) | \(.n) | \(if .n>0 then ((.c*1000/.n|round)/10|tostring) else "—" end) | \(.e) |"' "$RESULTS"

  printf '\n## Accuracy by layout × target rank k\n\n'
  printf '| model | k | v1 | v2 | v2-last | n per layout |\n|---|---:|---:|---:|---:|---:|\n'
  jq -rs 'map(select(.error != true))
          | group_by(.model + "|" + (.k|tostring))[]
          | . as $g
          | ($g[0].model) as $m | ($g[0].k) as $k
          | (reduce $g[] as $r ({}; .[$r.layout] += [$r]))
          | . as $by
          | def pct($l): if ($by[$l]|length) > 0
              then ((($by[$l]|map(select(.correct))|length)*1000/($by[$l]|length)|round)/10|tostring)
              else "—" end;
            "| \($m) | \($k) | \(pct("v1")) | \(pct("v2")) | \(pct("v2-last")) | \([$by[]|length]|max) |"' "$RESULTS"

  printf '\n## The flip rule\n\n'
  printf 'The design flips the default to `v2-last` ONLY on a **≥ 5 point** win over\n'
  printf '`v2` on **every** model that ran. Otherwise `v2` stays the default.\n\n'
  printf '| model | v2 %% | v2-last %% | delta (pts) | ≥ 5? |\n|---|---:|---:|---:|:--:|\n'
  jq -rs 'map(select(.error != true))
          | group_by(.model)[]
          | . as $g | ($g[0].model) as $m
          | (reduce $g[] as $r ({}; .[$r.layout] += [$r])) as $by
          | (if ($by["v2"]|length) > 0 then (($by["v2"]|map(select(.correct))|length)*1000/($by["v2"]|length)|round)/10 else null end) as $a
          | (if ($by["v2-last"]|length) > 0 then (($by["v2-last"]|map(select(.correct))|length)*1000/($by["v2-last"]|length)|round)/10 else null end) as $b
          | if ($a == null or $b == null) then "| \($m) | — | — | — | — |"
            else "| \($m) | \($a) | \($b) | \(((($b-$a)*10)|round)/10) | \(if ($b-$a) >= 5 then "yes" else "no" end) |"
            end' "$RESULTS"

  printf '\n## Median latency and prompt size\n\n'
  printf '| model | median s | median prompt chars |\n|---|---:|---:|\n'
  jq -rs 'group_by(.model)[]
          | . as $g | ($g[0].model) as $m
          | (($g|map(.elapsed_s)|sort)[($g|length)/2|floor]) as $t
          | (($g|map(.prompt_chars)|sort)[($g|length)/2|floor]) as $c
          | "| \($m) | \($t) | \($c) |"' "$RESULTS"

  printf '\n## Format compliance, and accuracy among replies that complied\n\n'
  printf 'The prompt asks for ONE word. A reply longer than five words did not answer\n'
  printf 'in the requested form — in this run those are almost entirely refusals: the\n'
  printf 'stronger lane repeatedly read the injected block as a prompt-injection\n'
  printf 'pattern and said so in a paragraph instead of answering. The classification\n'
  printf 'is purely by LENGTH, applied identically to every layout, so it cannot\n'
  printf 'favour one. `compliant %%` separates "answered the wrong word" from\n'
  printf '"declined to answer in the requested form", which are different failures.\n\n'
  printf '| lane | layout | verbose | compliant n | compliant %% | answer present anywhere %% |\n'
  printf '|---|---|---:|---:|---:|---:|\n'
  jq -rs 'map(select(.error != true))
          | group_by((.model_label // .model) + "|" + .layout)[]
          | . as $g
          | ($g | map(select((.got | split(" ") | length) > 5))) as $verbose
          | ($g | map(select((.got | split(" ") | length) <= 5))) as $comp
          | ($g | map(select(. as $r | ($r.got | ascii_downcase)
                                      | contains($r.expected | ascii_downcase)))) as $has
          | "| \($g[0].model_label // $g[0].model) | \($g[0].layout) | \($verbose|length) | \($comp|length) | \(if ($comp|length) > 0 then (((($comp|map(select(.correct))|length)*1000/($comp|length))|round)/10|tostring) else "—" end) | \(((($has|length)*1000/($g|length))|round)/10) |"' "$RESULTS"

  printf '\n## Compliant accuracy by target rank k\n\n'
  printf 'The order question, with the refusals removed. n per cell in brackets.\n\n'
  printf '| lane | k | v1 | v2 | v2-last |\n|---|---:|---|---|---|\n'
  jq -rs 'map(select((.error != true) and ((.got | split(" ") | length) <= 5)))
          | group_by((.model_label // .model) + "|" + (.k|tostring))[]
          | . as $g
          | ($g[0].model_label // $g[0].model) as $m | ($g[0].k) as $k
          | (reduce $g[] as $r ({}; .[$r.layout] += [$r])) as $by
          | def cell($l): if (($by[$l]|length) // 0) > 0
              then ((((($by[$l]|map(select(.correct))|length)*1000/($by[$l]|length))|round)/10|tostring)
                    + " (" + (($by[$l]|length)|tostring) + ")")
              else "—" end;
            "| \($m) | \($k) | \(cell("v1")) | \(cell("v2")) | \(cell("v2-last")) |"' "$RESULTS"

  printf '\n## Misses, by rank\n\n'
  printf '| query | layout | k | model | expected | got |\n|---|---|---:|---|---|---|\n'
  jq -rs 'map(select((.correct|not) and (.error != true))) | sort_by(.k, .query_id)[]
          | "| \(.query_id) | \(.layout) | \(.k) | \(.model) | `\(.expected)` | `\(.got[0:40])` |"' "$RESULTS"

  printf '\n## Errors (empty answer after one retry — excluded from accuracy)\n\n'
  printf '| query | layout | k | model |\n|---|---|---:|---|\n'
  jq -rs 'map(select(.error == true)) | sort_by(.model, .k, .query_id)[]
          | "| \(.query_id) | \(.layout) | \(.k) | \(.model) |"' "$RESULTS"
} >"$REPORT"

printf 'report: %s\n' "$REPORT"
