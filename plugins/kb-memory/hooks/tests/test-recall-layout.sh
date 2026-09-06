#!/usr/bin/env bash
# test-recall-layout.sh — MR1 (SL6): KB_RECALL_LAYOUT selects the SHAPE of
# kb-recall.sh's injected block. Three values, one fixture:
#
#   v1       byte-identical to the pre-MR1 output, diffed against a
#            baseline captured from the unmodified hook
#            (fixtures/recall-layout-v1.txt). If this test ever fails, the
#            v1 branch of the jq filter was "tidied" — restore it; someone
#            downstream is pinning those bytes.
#   v2       the default. No `(id …)` parenthetical, drift suffix straight
#            after `[kb]`, depth by rank (320/320/200/title/title), and a
#            `pos=<rank>` pair in every marker. Asserted to be no LONGER
#            than v1 on the shared fixture — the whole point of the cut.
#   v2-last  v2 with the pack reversed (rank 1 prints LAST) while `pos`
#            still carries the true rank, so the ledger is layout-
#            independent. For the MR2 order probe only.
#
# An unknown value falls back to v2 with ONE stderr warning and still
# injects — a hook that refused would cost the turn its memories.
#
# Fake `kb` on PATH (only `kb recall` is exercised); `jq` is real.
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-layout.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"
PACK="$SCRIPT_DIR/fixtures/recall-pack-5.json"
BASELINE="$SCRIPT_DIR/fixtures/recall-layout-v1.txt"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-layout-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<EOF
#!/usr/bin/env bash
if [ "\$1" = "recall" ]; then cat "$PACK"; exit 0; fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

# KB_HOOK_FMT=kimi so the block is the BARE stdout — the envelope is
# KB_HOOK_FMT's job and orthogonal to the layout under test (asserted at
# the bottom).
n=0
render() {
  n=$((n + 1))
  printf '%s' "{\"session_id\":\"s-layout-$n\",\"cwd\":\"/tmp\",\"prompt\":\"q\"}" \
    | KB_HOOK_FMT=kimi KB_RECALL_LAYOUT="$1" "$RECALL" 2>"$TMPROOT/err-$n.txt"
}

echo "== kb-recall.sh KB_RECALL_LAYOUT test matrix =="
echo "tmp root: $TMPROOT"
echo

render v1 >"$TMPROOT/v1.txt"
render v2 >"$TMPROOT/v2.txt"
render v2-last >"$TMPROOT/v2last.txt"

# --- 1. v1 is byte-identical to the captured pre-MR1 baseline ------------
if diff -q "$BASELINE" "$TMPROOT/v1.txt" >/dev/null 2>&1; then
  ok "v1 is byte-identical to the pre-MR1 baseline fixture"
else
  bad "v1 is byte-identical to the pre-MR1 baseline fixture"
  diff -u "$BASELINE" "$TMPROOT/v1.txt" | head -20
fi

# --- 2. v1's marker carries NO pos (the pre-MR1 grammar) ----------------
case "$(cat "$TMPROOT/v1.txt")" in
  *"<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6-->"*) ok "v1 marker has no pos= pair" ;;
  *) bad "v1 marker has no pos= pair" ;;
esac

# --- 3. v2 drops the parenthetical entirely -----------------------------
case "$(cat "$TMPROOT/v2.txt")" in
  *"(id "*) bad "v2 drops the (id …) parenthetical" ;;
  *) ok "v2 drops the (id …) parenthetical" ;;
esac
case "$(cat "$TMPROOT/v2.txt")" in
  *", unread"* | *", read "*) bad "v2 drops the read/unread reading state" ;;
  *) ok "v2 drops the read/unread reading state" ;;
esac

# --- 4. v2 keeps the [kb] tag and puts drift straight after it ----------
case "$(cat "$TMPROOT/v2.txt")" in
  *"- demo-repo build cache setup  [kb]"*) ok "v2 keeps the two-space [kb] tag" ;;
  *) bad "v2 keeps the two-space [kb] tag" ;;
esac
case "$(cat "$TMPROOT/v2.txt")" in
  *"- ⚠ disputed: Wrong port for the staging service  [main] [⚠ 1 drift-flagged citation(s)]"*)
    ok "v2 drift suffix follows [kb] directly, after the disputed prefix" ;;
  *) bad "v2 drift suffix follows [kb] directly, after the disputed prefix" ;;
esac

# --- 5. pos= is present, sequential 1..5, in reading order ---------------
posv2="$(grep -o 'pos=[0-9]*' "$TMPROOT/v2.txt" | tr '\n' ' ')"
if [ "$posv2" = "pos=1 pos=2 pos=3 pos=4 pos=5 " ]; then
  ok "v2 markers carry pos=1..5 in order"
else
  bad "v2 markers carry pos=1..5 in order (got: $posv2)"
fi

# --- 6. depth by rank: 320/320/200/none/none ----------------------------
# The fixture's hit 1 summary is 313 chars — longer than v1's 220 cap and
# shorter than v2's 320, so a correct v2 renders it WHOLE and v1 truncates
# it. That single hit proves the deepening is real and not a no-op.
if grep -q 'rather than left to finish naturally\.$' "$TMPROOT/v2.txt"; then
  ok "v2 renders hit 1's summary past v1's 220-char cut"
else
  bad "v2 renders hit 1's summary past v1's 220-char cut"
fi
if grep -q 'rather than left to finish naturally\.$' "$TMPROOT/v1.txt"; then
  bad "v1 still truncates hit 1's summary at 220"
else
  ok "v1 still truncates hit 1's summary at 220"
fi
summaries_v2="$(grep -c '    ↳ ' "$TMPROOT/v2.txt" || true)"
if [ "$summaries_v2" = "3" ]; then
  ok "v2 renders exactly 3 summary lines (hits 4-5 are title-only)"
else
  bad "v2 renders exactly 3 summary lines (got: $summaries_v2)"
fi

# --- 7. the byte budget: v2 is never longer than v1 on this pack --------
b1="$(wc -c <"$TMPROOT/v1.txt")"
b2="$(wc -c <"$TMPROOT/v2.txt")"
if [ "$b2" -le "$b1" ]; then
  ok "v2 block ($b2 B) is no longer than v1 ($b1 B) on the shared fixture"
else
  bad "v2 block ($b2 B) is LONGER than v1 ($b1 B) on the shared fixture"
fi

# --- 8. v2-last: same bytes, reversed reading order, pos preserved ------
if [ "$(wc -c <"$TMPROOT/v2last.txt")" = "$b2" ]; then
  ok "v2-last is the same byte count as v2 (a reorder, not a reshape)"
else
  bad "v2-last is the same byte count as v2"
fi
posv2l="$(grep -o 'pos=[0-9]*' "$TMPROOT/v2last.txt" | tr '\n' ' ')"
if [ "$posv2l" = "pos=5 pos=4 pos=3 pos=2 pos=1 " ]; then
  ok "v2-last reverses the reading order while pos keeps the true rank"
else
  bad "v2-last reverses the reading order while pos keeps the true rank (got: $posv2l)"
fi
# The top hit is the LAST bullet, and it still carries its deep summary.
last_bullet="$(grep '^- ' "$TMPROOT/v2last.txt" | tail -1)"
case "$last_bullet" in
  "- demo-repo build cache setup  [kb]") ok "v2-last prints rank 1 last" ;;
  *) bad "v2-last prints rank 1 last (got: $last_bullet)" ;;
esac
if grep -q 'rather than left to finish naturally\.$' "$TMPROOT/v2last.txt"; then
  ok "v2-last keeps rank 1's 320-char depth despite printing it last"
else
  bad "v2-last keeps rank 1's 320-char depth despite printing it last"
fi

# --- 9. an unknown value warns ONCE on stderr and renders v2 ------------
render bogus >"$TMPROOT/bogus.txt"
err="$(cat "$TMPROOT/err-$n.txt")"
case "$err" in
  *"unknown KB_RECALL_LAYOUT bogus"*) ok "unknown layout warns on stderr" ;;
  *) bad "unknown layout warns on stderr (got: $err)" ;;
esac
if [ "$(printf '%s\n' "$err" | grep -c 'unknown KB_RECALL_LAYOUT')" = "1" ]; then
  ok "the unknown-layout warning fires exactly once"
else
  bad "the unknown-layout warning fires exactly once"
fi
if diff -q "$TMPROOT/v2.txt" "$TMPROOT/bogus.txt" >/dev/null 2>&1; then
  ok "unknown layout falls back to v2's exact bytes"
else
  bad "unknown layout falls back to v2's exact bytes"
fi

# --- 10. an UNSET / empty layout is v2, and never warns -----------------
unset_out="$(printf '%s' '{"session_id":"s-unset","cwd":"/tmp","prompt":"q"}' \
  | KB_HOOK_FMT=kimi "$RECALL" 2>"$TMPROOT/err-unset.txt")"
if [ "$unset_out" = "$(cat "$TMPROOT/v2.txt")" ]; then
  ok "an unset KB_RECALL_LAYOUT is v2 (the default)"
else
  bad "an unset KB_RECALL_LAYOUT is v2 (the default)"
fi
empty_out="$(printf '%s' '{"session_id":"s-empty","cwd":"/tmp","prompt":"q"}' \
  | KB_HOOK_FMT=kimi KB_RECALL_LAYOUT= "$RECALL" 2>>"$TMPROOT/err-unset.txt")"
if [ "$empty_out" = "$(cat "$TMPROOT/v2.txt")" ]; then
  ok "an EMPTY KB_RECALL_LAYOUT is v2, not an unknown value"
else
  bad "an EMPTY KB_RECALL_LAYOUT is v2, not an unknown value"
fi
if [ -s "$TMPROOT/err-unset.txt" ]; then
  bad "the default path is silent on stderr (got: $(cat "$TMPROOT/err-unset.txt"))"
else
  ok "the default path is silent on stderr"
fi

# --- 11. layout is orthogonal to KB_HOOK_FMT ----------------------------
# Same v2 block, wrapped in the Claude envelope instead of bare stdout.
envelope="$(printf '%s' '{"session_id":"s-env","cwd":"/tmp","prompt":"q"}' \
  | KB_RECALL_LAYOUT=v2 "$RECALL")"
if printf '%s' "$envelope" | jq -e '.hookSpecificOutput.hookEventName == "UserPromptSubmit"
     and (.hookSpecificOutput.additionalContext | contains("pos=1"))' >/dev/null 2>&1; then
  ok "KB_RECALL_LAYOUT is orthogonal to KB_HOOK_FMT (v2 inside the envelope)"
else
  bad "KB_RECALL_LAYOUT is orthogonal to KB_HOOK_FMT (got: $envelope)"
fi

# --- 12. an EMPTY hit list injects nothing in every layout --------------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then echo '{"hits":[]}'; exit 0; fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
empty_all=1
for l in v1 v2 v2-last; do
  o="$(render "$l")"
  [ -z "$o" ] || empty_all=0
done
if [ "$empty_all" = "1" ]; then
  ok "an empty hit list injects nothing, in all three layouts"
else
  bad "an empty hit list injects nothing, in all three layouts"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
