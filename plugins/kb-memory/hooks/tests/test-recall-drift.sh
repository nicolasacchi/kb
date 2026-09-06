#!/usr/bin/env bash
# test-recall-drift.sh — CT-C4: kb-recall.sh renders a
# " [⚠ N drift-flagged citation(s)]" SUFFIX (MR1/layout v2: directly after
# the "[<kb>]" tag; layout v1 kept it after the "(id …)" tail)
# on hits whose `drift_open` field is > 0 (open /kb-verify `[kb-drift]`
# comments), composing with CT-C1's "⚠ disputed:" and CT-C3's
# "✗ didn't work:" prefixes (prefix order unchanged — the drift marker is
# strictly a suffix), leaves hits
# without it (absent, or explicit 0) with the bare "- <title>  [<kb>]" line,
# and never renders `code_hints`
# (json consumers only — the hook line stays lean). Fake `kb` on PATH
# (only `kb recall` is exercised); `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-drift.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-drift-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

echo "== kb-recall.sh drift-suffix (open [kb-drift] comments) test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. a drift hit gets the suffix AFTER the "[<kb>]" tag ----------------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Actor read lanes","kb":"main","id":"abc123def456","drift_open":2}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out="$(printf '%s' '{"session_id":"s1","cwd":"/tmp","prompt":"how do lanes work?"}' | "$RECALL")"
case "$out" in
  *"- Actor read lanes  [main] [⚠ 2 drift-flagged citation(s)]"*)
    ok "drift hit renders the suffix after the [kb] tag" ;;
  *) bad "drift hit renders the suffix after the [kb] tag (got: $out)" ;;
esac

# --- 2. an ordinary hit (no `drift_open` key) renders the bare -----------
#        "- <title>  [<kb>]" v2 line, with no drift suffix at all.
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T1","kb":"main","id":"abc123def456"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out2="$(printf '%s' '{"session_id":"s2","cwd":"/tmp","prompt":"how do widgets work?"}' | "$RECALL")"
case "$out2" in
  *"- T1  [main]"*) ok "absent drift_open -> bare line shape" ;;
  *) bad "absent drift_open -> bare line shape (got: $out2)" ;;
esac
case "$out2" in
  *"drift-flagged"*) bad "ordinary hit must NOT mention drift (got: $out2)" ;;
  *) ok "ordinary hit never mentions drift" ;;
esac

# --- 3. an explicit `"drift_open":0` hit renders the same bare line -------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T2","kb":"main","id":"def456abc123","drift_open":0}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out3="$(printf '%s' '{"session_id":"s3","cwd":"/tmp","prompt":"another question"}' | "$RECALL")"
case "$out3" in
  *"- T2  [main]"*) ok "explicit drift_open:0 -> bare line shape" ;;
  *) bad "explicit drift_open:0 -> bare line shape (got: $out3)" ;;
esac
case "$out3" in
  *"drift-flagged"*) bad "drift_open:0 must NOT mention drift (got: $out3)" ;;
  *) ok "drift_open:0 never mentions drift" ;;
esac

# --- 4. the suffix composes with BOTH prefixes: a flagged + warns + -------
#        drifted hit keeps "⚠ disputed: ✗ didn't work:" prefix order AND
#        gets the suffix after the tail.
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Old port advice","kb":"main","id":"333333333333","flagged":true,"warns":true,"drift_open":1}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out4="$(printf '%s' '{"session_id":"s4","cwd":"/tmp","prompt":"ports again"}' | "$RECALL")"
case "$out4" in
  *"- ⚠ disputed: ✗ didn't work: Old port advice  [main] [⚠ 1 drift-flagged citation(s)]"*)
    ok "flagged+warns+drift: prefixes keep their order, suffix lands after the tail" ;;
  *) bad "flagged+warns+drift: prefixes keep their order, suffix lands after the tail (got: $out4)" ;;
esac

# --- 5. drift composes with each single prefix too --------------------------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Disputed drifted","kb":"main","id":"555555555555","flagged":true,"drift_open":3},{"title":"Failed drifted","kb":"main","id":"666666666666","warns":true,"drift_open":1}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out5="$(printf '%s' '{"session_id":"s5","cwd":"/tmp","prompt":"pairs"}' | "$RECALL")"
case "$out5" in
  *"- ⚠ disputed: Disputed drifted  [main] [⚠ 3 drift-flagged citation(s)]"*)
    ok "flagged+drift composes prefix AND suffix" ;;
  *) bad "flagged+drift composes prefix AND suffix (got: $out5)" ;;
esac
case "$out5" in
  *"- ✗ didn't work: Failed drifted  [main] [⚠ 1 drift-flagged citation(s)]"*)
    ok "warns+drift composes prefix AND suffix" ;;
  *) bad "warns+drift composes prefix AND suffix (got: $out5)" ;;
esac

# --- 6. the suffix sits BEFORE the ↳ summary continuation line -------------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"With summary","kb":"main","id":"777777777777","drift_open":1,"summary":"a gloss"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out6="$(printf '%s' '{"session_id":"s6","cwd":"/tmp","prompt":"summaries"}' | "$RECALL")"
case "$out6" in
  *"[main] [⚠ 1 drift-flagged citation(s)]"*"↳ a gloss"*"<!--kb-recall/1 kb=main id=777777777777 pos=1-->"*)
    ok "suffix lands on the title line, before the ↳ summary" ;;
  *) bad "suffix lands on the title line, before the ↳ summary (got: $out6)" ;;
esac

# --- 7. code_hints is NEVER rendered by the hook (json consumers only) ----
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Hinted","kb":"main","id":"888888888888","code_hints":["src/very-visible-path.rs","src/other.rs"],"code_hints_total":7}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out7="$(printf '%s' '{"session_id":"s7","cwd":"/tmp","prompt":"hints"}' | "$RECALL")"
case "$out7" in
  *"very-visible-path"*) bad "code_hints must NOT appear in the hook line (got: $out7)" ;;
  *) ok "code_hints never rendered — the hook line stays lean" ;;
esac
case "$out7" in
  *"- Hinted  [main]"*) ok "hinted hit keeps the ordinary line shape" ;;
  *) bad "hinted hit keeps the ordinary line shape (got: $out7)" ;;
esac

# --- 8. mixed drifted + ordinary hits in ONE response: each row keeps -----
#        its own tail; the block's header line is untouched.
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Rotten one","kb":"main","id":"111111111111","drift_open":2},{"title":"Fine one","kb":"main","id":"222222222222"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out8="$(printf '%s' '{"session_id":"s8","cwd":"/tmp","prompt":"mixed"}' | "$RECALL")"
case "$out8" in
  *"Relevant memories from kb (recall — these persist across sessions):"*"- Rotten one  [main] [⚠ 2 drift-flagged citation(s)]"*"- Fine one  [main]"*)
    ok "mixed hits: only the drifted row carries the suffix, header line unchanged" ;;
  *) bad "mixed hits: only the drifted row carries the suffix, header line unchanged (got: $out8)" ;;
esac

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
