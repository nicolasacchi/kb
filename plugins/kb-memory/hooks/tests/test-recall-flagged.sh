#!/usr/bin/env bash
# test-recall-flagged.sh — CT-C1: kb-recall.sh renders a "⚠ disputed:"
# prefix on hits whose `flagged` field is true (kb memory flag / an OPEN
# [kb-flag] comment), and stays byte-identical to the pre-CT-C1 shape for
# every hit that isn't flagged (absent, or explicitly false).
# Fake `kb` on PATH (only `kb recall` is exercised); `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-flagged.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-flagged-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

echo "== kb-recall.sh flagged-hit test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. a flagged hit gets the "⚠ disputed:" prefix ----------------------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Wrong port","kb":"main","id":"abc123def456","flagged":true}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out="$(printf '%s' '{"session_id":"s1","cwd":"/tmp","prompt":"what port?"}' | "$RECALL")"
case "$out" in
  *"- ⚠ disputed: Wrong port  [main]"*) ok "flagged hit renders the ⚠ disputed prefix" ;;
  *) bad "flagged hit renders the ⚠ disputed prefix (got: $out)" ;;
esac

# --- 2. an unflagged hit (no `flagged` key) is byte-identical to the ------
#        pre-CT-C1 "- <title>  [<kb>]" shape.
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
  *"- T1  [main]"*) ok "absent flagged field -> unchanged '- <title>' shape" ;;
  *) bad "absent flagged field -> unchanged '- <title>' shape (got: $out2)" ;;
esac
case "$out2" in
  *"disputed"*) bad "unflagged hit must NOT mention disputed (got: $out2)" ;;
  *) ok "unflagged hit never mentions disputed" ;;
esac
# MR1 — the line ends at the [kb] tag: the id's one home is the marker,
# which now also carries the rank. (Layout v1 still prints the old
# parenthetical; that shape is pinned by test-recall-layout.sh.)
case "$out2" in
  *"(id "*) bad "v2 line carries no (id …) parenthetical (got: $out2)" ;;
  *) ok "v2 line carries no (id …) parenthetical" ;;
esac
case "$out2" in
  *"<!--kb-recall/1 kb=main id=abc123def456 pos=1-->"*)
    ok "v2 marker carries kb, id and pos" ;;
  *) bad "v2 marker carries kb, id and pos (got: $out2)" ;;
esac

# --- 3. an explicit `"flagged":false` hit is ALSO byte-identical ----------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T2","kb":"main","id":"def456abc123","flagged":false}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out3="$(printf '%s' '{"session_id":"s3","cwd":"/tmp","prompt":"another question"}' | "$RECALL")"
case "$out3" in
  *"- T2  [main]"*) ok "explicit flagged:false -> unchanged '- <title>' shape" ;;
  *) bad "explicit flagged:false -> unchanged '- <title>' shape (got: $out3)" ;;
esac

# --- 4. mixed flagged + unflagged hits in ONE response: only the flagged --
#        row gets the prefix; the block's header line is untouched.
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Stale one","kb":"main","id":"111111111111","flagged":true},{"title":"Fine one","kb":"main","id":"222222222222"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out4="$(printf '%s' '{"session_id":"s4","cwd":"/tmp","prompt":"mixed"}' | "$RECALL")"
case "$out4" in
  *"Relevant memories from kb (recall — these persist across sessions):"*"- ⚠ disputed: Stale one"*"- Fine one  [main]"*)
    ok "mixed hits: flagged row prefixed, unflagged row + header line unchanged" ;;
  *) bad "mixed hits: flagged row prefixed, unflagged row + header line unchanged (got: $out4)" ;;
esac

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
