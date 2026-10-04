#!/usr/bin/env bash
# test-capture-adapters-spool.sh - v0.45 N4: the codex / opencode / kimi / omp /
# grok capture adapters share the claude hook's spool contract. They translate
# their harness transcript, hand it to `kb sessions capture`, and when that
# cannot run (kb fails, kb missing) they park the UNSCRUBBED translation in the
# private spool - dir 0700, files 0600, outside every corpus - instead of
# writing HTML themselves. The next successful capture (or --replay-spool)
# lands it through the scrubbed Rust path, the harness surviving in the
# adapter-meta record (enrich ladder rung 1), under a filename that carries the
# session's true start stamp.
#
# Needs the REAL `kb` binary (KB_BIN_DIR, set by the cargo harness, else PATH)
# for the replay/stamp cases and a fake failing `kb` for the failure ones.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-adapters-spool.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-adapters-spool-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

GH="ghp_0123456789abcdefghijklmnopqrstuvwxyzAB"
AWS="AKIAIOSFODNN7EXAMPLE"

REAL_PATH="$PATH"
[ -n "${KB_BIN_DIR:-}" ] && REAL_PATH="$KB_BIN_DIR:$PATH"
REAL_KB="$(PATH="$REAL_PATH" command -v kb || true)"

mkdir -p "$TMPROOT/failbin" "$TMPROOT/nokb" "$TMPROOT/home-empty"
printf '#!/usr/bin/env bash\nexit 1\n' >"$TMPROOT/failbin/kb"
chmod +x "$TMPROOT/failbin/kb"
ln -sf "$(command -v jq)" "$TMPROOT/nokb/jq"
NOKB_PATH="$TMPROOT/nokb:/usr/bin:/bin"

# --- fixtures (secrets planted in the user text) -----------------------------
ROLLOUT="$TMPROOT/rollout.jsonl"
cat >"$ROLLOUT" <<JSONL
{"timestamp":"2026-03-01T09:00:00.000Z","type":"session_meta","payload":{"id":"codex_sess_0001","timestamp":"2026-03-01T09:00:00.000Z","cwd":"/tmp/x","originator":"codex","cli_version":"0"}}
{"timestamp":"2026-03-01T09:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"use my key $AWS for s3"}]}}
{"timestamp":"2026-03-01T09:00:02.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"token $GH leaked"}}
JSONL
EXPORT="$TMPROOT/oc-export.json"
cat >"$EXPORT" <<JSON
{"info":{"id":"ses_oc0001","directory":"/tmp/x","title":"t","time":{"created":1772355600000}},"messages":[{"info":{"role":"user","time":{"created":1772355600000}},"parts":[{"type":"text","text":"my key is $AWS ok"}]},{"info":{"role":"assistant","time":{"created":1772355601000}},"parts":[{"type":"tool","tool":"bash","callID":"c1","state":{"status":"completed","input":{"command":"echo hi"},"output":"token $GH here"}}]}]}
JSON
KSDIR="$TMPROOT/kimi-home/sessions/wd_x_deadbeef/session_aaaaaaaa-0000-0000-0000-000000000001/agents/main"
mkdir -p "$KSDIR"
sed "s/commit the widget fix please/commit the widget fix please $AWS/; s/wrote \/tmp\/widget.py/token $GH leaked/" \
  "$SCRIPT_DIR/fixtures/kimi-wire-commit.jsonl" >"$KSDIR/wire.jsonl"
OMPS="$TMPROOT/omp-session.jsonl"
{
  title='{"type":"title","v":1,"title":"t"}'
  printf '%s%*s\n' "$title" "$((256 - ${#title} - 1))" ''
  sed "s/commit the widget fix please/commit the widget fix please $AWS token $GH/" "$SCRIPT_DIR/fixtures/omp-session-commit.jsonl"
} >"$OMPS"
GSD="$TMPROOT/grok-sd"
cp -r "$SCRIPT_DIR/fixtures/grok-session" "$GSD"
sed -i "s/Add a --dry-run flag to the fixture export command./Add a flag; my key is $AWS and $GH/" "$GSD/chat_history.jsonl"

ADAPTERS="codex opencode kimi omp grok"
sid_of() {
  case "$1" in
    codex) echo codex_sess_0001 ;;
    opencode) echo ses_oc0001 ;;
    kimi) echo session_aaaaaaaa-0000-0000-0000-000000000001 ;;
    omp) echo fx01a034-0000-0000-0000-000000000001 ;;
    grok) echo grok-sd ;;
  esac
}
stamp_of() {
  case "$1" in
    codex) echo 20260301T090000Z ;;
    opencode) echo 20260301T090000Z ;;
    kimi) echo 20260812T135500Z ;;
    omp) echo 20260824T100000Z ;;
    grok) echo 20260101T000000Z ;;
  esac
}

# run_adapter <adapter> <PATH> <sessions dir> <spool dir> [env -i home]
run_adapter() {
  local a="$1" p="$2" d="$3" sp="$4" home="${5:-}" script="$HOOKS_DIR/kb-capture-$1.sh" arg
  case "$a" in
    codex) arg="$ROLLOUT" ;;
    opencode) arg="$EXPORT" ;;
    kimi) arg="$KSDIR/wire.jsonl" ;;
    omp) arg="$OMPS" ;;
    grok) arg="" ;;
  esac
  local -a cmd=(bash "$script")
  if [ "$a" = grok ]; then cmd+=(--session-dir "$GSD" --cwd /tmp/x); else cmd+=("$arg"); fi
  if [ -n "$home" ]; then
    env -i PATH="$p" HOME="$home" KB_SESSIONS_DIR="$d" KB_CAPTURE_SPOOL="$sp" XDG_CACHE_HOME="$TMPROOT/xdg-$a" "${cmd[@]}" >/dev/null 2>&1
  else
    PATH="$p" KB_SESSIONS_DIR="$d" KB_CAPTURE_SPOOL="$sp" XDG_CACHE_HOME="$TMPROOT/xdg-$a" "${cmd[@]}" >/dev/null 2>&1
  fi
}

n_files() { find "$1" -type f 2>/dev/null | wc -l | tr -d ' '; }

spool_item() { # spool dir -> first .jsonl
  find "$1" -maxdepth 1 -name '*.jsonl' 2>/dev/null | head -1
}

for a in $ADAPTERS; do
  SID="$(sid_of "$a")"
  STAMP="$(stamp_of "$a")"

  echo "== $a: kb fails =="
  D="$TMPROOT/$a-fail/sessions"; SP="$TMPROOT/$a-fail/spool"; mkdir -p "$D"
  run_adapter "$a" "$TMPROOT/failbin:$PATH" "$D" "$SP"
  item="$(spool_item "$SP")"
  if [ "$(n_files "$D")" = 0 ] && [ -n "$item" ] \
    && [ "$(stat -c %a "$SP" 2>/dev/null)" = 700 ] && [ "$(stat -c %a "$item" 2>/dev/null)" = 600 ] \
    && head -1 "$item" | jq -e --arg h "$a" '.type == "adapter-meta" and .harness == $h' >/dev/null 2>&1 \
    && grep -qx "session_id=$SID" "${item%.jsonl}.meta" \
    && grep -qx "stamp=$STAMP" "${item%.jsonl}.meta"; then
    ok "${a}_failed_capture_spools_translated_jsonl_not_corpus"
  else
    bad "${a}_failed_capture_spools_translated_jsonl_not_corpus (corpus files: $(n_files "$D"), item: ${item:-none}, meta: $(cat "${item%.jsonl}.meta" 2>/dev/null | tr '\n' ' '))"
  fi
  if grep -rq "$GH\|$AWS" "$D" 2>/dev/null; then bad "$a: no raw secret in the corpus on failure"; else ok "$a: no raw secret in the corpus on failure"; fi

  echo "== $a: kb missing =="
  D="$TMPROOT/$a-nokb/sessions"; SP="$TMPROOT/$a-nokb/spool"; mkdir -p "$D"
  run_adapter "$a" "$NOKB_PATH" "$D" "$SP" "$TMPROOT/home-empty"
  if [ "$(n_files "$D")" = 0 ] && [ -n "$(spool_item "$SP")" ]; then
    ok "${a}_never_writes_unscrubbed_html_when_kb_absent"
  else
    bad "${a}_never_writes_unscrubbed_html_when_kb_absent (corpus files: $(n_files "$D"), spool: $(spool_item "$SP"))"
  fi

  if [ -z "$REAL_KB" ]; then
    bad "$a: replay cases need a real kb binary (KB_BIN_DIR)"
    continue
  fi

  echo "== $a: replay lands scrubbed, harness intact, start stamp in the name =="
  D="$TMPROOT/$a-fail/sessions"; SP="$TMPROOT/$a-fail/spool"
  PATH="$REAL_PATH" KB_CAPTURE_SPOOL="$SP" kb sessions capture --replay-spool --out "$D" >/dev/null 2>&1
  f="$(ls "$D"/session-*.html 2>/dev/null | head -1)"
  if [ -n "$f" ] && ! grep -q "$GH\|$AWS" "$f" && grep -q '\[redacted:' "$f" \
    && grep -q "\"harness\":\"$a\"" "$f" && [ -z "$(spool_item "$SP")" ]; then
    ok "${a}_replay_lands_scrubbed_with_harness"
  else
    bad "${a}_replay_lands_scrubbed_with_harness (file: ${f:-none}, spool left: $(spool_item "$SP"))"
  fi
  case "$(basename "${f:-x}")" in
    session-"$STAMP"-*) ok "${a}_replayed_filename_uses_session_start_stamp" ;;
    *) bad "${a}_replayed_filename_uses_session_start_stamp (got $(basename "${f:-none}"), want session-$STAMP-*)" ;;
  esac

  echo "== $a: the next success replays a pending spool =="
  D="$TMPROOT/$a-next/sessions"; SP="$TMPROOT/$a-next/spool"; mkdir -p "$D"
  run_adapter "$a" "$TMPROOT/failbin:$PATH" "$D" "$SP"
  OTHER="$TMPROOT/other-$a.jsonl"
  printf '{"sessionId":"sess-other-%s","type":"user","timestamp":"2026-03-01T10:00:00.000Z","message":{"role":"user","content":"hello"},"promptSource":"typed"}\n' "$a" >"$OTHER"
  printf '{"session_id":"sess-other-%s","transcript_path":"%s","cwd":"%s"}' "$a" "$OTHER" "$TMPROOT" \
    | PATH="$REAL_PATH" KB_SESSIONS_DIR="$D" KB_CAPTURE_SPOOL="$SP" bash "$HOOKS_DIR/kb-capture.sh" >/dev/null 2>&1
  if [ "$(ls "$D"/session-*.html 2>/dev/null | wc -l | tr -d ' ')" = 2 ] && [ -z "$(spool_item "$SP")" ] \
    && ! grep -rq "$GH\|$AWS" "$D"; then
    ok "${a}_next_success_replays_pending_spool"
  else
    bad "${a}_next_success_replays_pending_spool (html: $(ls "$D" | tr '\n' ' '), spool: $(spool_item "$SP"))"
  fi

  echo "== $a: a direct success writes the stamp-named file and drops its own spool item =="
  D="$TMPROOT/$a-ok/sessions"; SP="$TMPROOT/$a-ok/spool"; mkdir -p "$D"
  run_adapter "$a" "$TMPROOT/failbin:$PATH" "$D" "$SP"
  run_adapter "$a" "$REAL_PATH" "$D" "$SP"
  f="$(ls "$D"/session-*.html 2>/dev/null | head -1)"
  case "$(basename "${f:-x}")" in
    session-"$STAMP"-*) st_ok=1 ;;
    *) st_ok=0 ;;
  esac
  if [ "$st_ok" = 1 ] && [ "$(ls "$D"/session-*.html | wc -l | tr -d ' ')" = 1 ] && [ -z "$(spool_item "$SP")" ]; then
    ok "${a}_first_capture_filename_uses_session_start_stamp"
  else
    bad "${a}_first_capture_filename_uses_session_start_stamp (files: $(ls "$D" | tr '\n' ' '), spool: $(spool_item "$SP"))"
  fi
done

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
