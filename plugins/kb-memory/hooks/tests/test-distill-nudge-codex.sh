#!/usr/bin/env bash
# test-distill-nudge-codex.sh — self-contained test matrix for
# kb-distill-nudge-codex.sh (MI-W0.3): the codex-side twin of
# kb-distill-nudge.sh, reading the codex rollout JSONL encoding
# (session_meta / response_item / function_call / function_call_output)
# instead of a Claude transcript. Runs the REAL script against mktemp
# fixture rollouts, with a fake `kb` on PATH (only `command -v kb` needs
# to succeed — this script never actually calls it; `jq` is real).
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-distill-nudge-codex.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
NUDGE="$HOOKS_DIR/kb-distill-nudge-codex.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-distill-nudge-codex-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"
export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

run_nudge() {
  local tpath="$1" cache="$2"
  export XDG_CACHE_HOME="$cache"
  mkdir -p "$cache"
  printf '%s' "{\"transcript_path\":\"$tpath\"}" | "$NUDGE"
}

mkdir -p "$TMPROOT/fire" "$TMPROOT/suppress"

# fire: exec_command runs `git commit`, no kb-remember success marker
# anywhere in the rollout.
cat >"$TMPROOT/fire/rollout.jsonl" <<'JSONL'
{"timestamp":"2026-08-01T00:00:00.000Z","type":"session_meta","payload":{"id":"codex-sess-1","timestamp":"2026-08-01T00:00:00.000Z","cwd":"/tmp/proj","originator":"codex_cli","cli_version":"0.99.0"}}
{"timestamp":"2026-08-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"commit the change"}]}}
{"timestamp":"2026-08-01T00:00:02.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call_1","arguments":"{\"cmd\":\"git commit -m 'wip'\"}"}}
{"timestamp":"2026-08-01T00:00:03.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"[main abc1234] wip\n"}}
JSONL

# suppress: same commit, PLUS a successful `kb remember` (shell-array
# command form, exercising the OTHER arguments encoding).
cat >"$TMPROOT/suppress/rollout.jsonl" <<'JSONL'
{"timestamp":"2026-08-01T00:00:00.000Z","type":"session_meta","payload":{"id":"codex-sess-2","timestamp":"2026-08-01T00:00:00.000Z","cwd":"/tmp/proj","originator":"codex_cli","cli_version":"0.99.0"}}
{"timestamp":"2026-08-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"commit the change"}]}}
{"timestamp":"2026-08-01T00:00:02.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call_1","arguments":"{\"cmd\":\"git commit -m 'wip'\"}"}}
{"timestamp":"2026-08-01T00:00:03.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"[main abc1234] wip\n"}}
{"timestamp":"2026-08-01T00:00:04.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"call_2","arguments":"{\"command\":[\"bash\",\"-lc\",\"kb remember 'fact'\"]}"}}
{"timestamp":"2026-08-01T00:00:05.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_2","output":"remembered abc123def456  (/kb/memory/x.html)\n"}}
JSONL

echo "== kb-distill-nudge-codex.sh test matrix (MI-W0.3) =="
echo "tmp root: $TMPROOT"
echo

out="$(run_nudge "$TMPROOT/fire/rollout.jsonl" "$TMPROOT/cache-fire")"
if printf '%s' "$out" | grep -q '"systemMessage"'; then
  ok "codex commit, no success marker -> fires"
else
  bad "codex commit, no success marker -> fires (got: $out)"
fi

out2="$(run_nudge "$TMPROOT/suppress/rollout.jsonl" "$TMPROOT/cache-suppress")"
if [ -z "$out2" ]; then
  ok "codex commit + success marker -> suppressed"
else
  bad "codex commit + success marker -> suppressed (got: $out2)"
fi

# Once-per-session marker: a second run against the same rollout+cache
# must stay silent.
out3="$(run_nudge "$TMPROOT/fire/rollout.jsonl" "$TMPROOT/cache-fire")"
if [ -z "$out3" ]; then
  ok "marker caps the nudge at once per session"
else
  bad "marker caps the nudge at once per session (got: $out3)"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
