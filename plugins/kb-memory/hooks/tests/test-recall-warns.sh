#!/usr/bin/env bash
# test-recall-warns.sh — CT-C3: kb-recall.sh renders a "✗ didn't work:"
# prefix on hits whose `warns` field is true (a failed-outcome memory,
# `kb remember --failed`), composes it with CT-C1's "⚠ disputed:" prefix
# (disputed FIRST when a hit is both), and stays byte-identical to the
# pre-CT-C3 shape for every hit that isn't warned (absent, or explicitly
# false). Fake `kb` on PATH (only `kb recall` is exercised); `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-warns.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-warns-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

echo "== kb-recall.sh warns-hit (failed outcome) test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. a warns hit gets the "✗ didn't work:" prefix ----------------------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Bind port 4000 locally","kb":"main","id":"abc123def456","warns":true}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out="$(printf '%s' '{"session_id":"s1","cwd":"/tmp","prompt":"what port?"}' | "$RECALL")"
case "$out" in
  *"- ✗ didn't work: Bind port 4000 locally  [main]"*) ok "warns hit renders the ✗ didn't-work prefix" ;;
  *) bad "warns hit renders the ✗ didn't-work prefix (got: $out)" ;;
esac

# --- 2. an ordinary hit (no `warns` key) is byte-identical to the ---------
#        pre-CT-C3 "- <title>  [<kb>]" shape.
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
# MR1 layout v2 — the id lives only in the marker, which now also carries
# the rank as `pos=`. (v1 still prints the old parenthetical; that shape is
# pinned byte-for-byte by test-recall-layout.sh.)
case "$out2" in
  *"(id "*) bad "v2 line carries no (id …) parenthetical (got: $out2)" ;;
  *) ok "v2 line carries no (id …) parenthetical" ;;
esac
case "$out2" in
  *"<!--kb-recall/1 kb=main id=abc123def456 pos=1-->"*)
    ok "v2 marker carries kb, id and pos" ;;
  *) bad "v2 marker carries kb, id and pos (got: $out2)" ;;
esac
case "$out2" in
  *"- T1  [main]"*) ok "absent warns field -> unchanged '- <title>' shape" ;;
  *) bad "absent warns field -> unchanged '- <title>' shape (got: $out2)" ;;
esac
case "$out2" in
  *"didn't work"*) bad "ordinary hit must NOT mention didn't work (got: $out2)" ;;
  *) ok "ordinary hit never mentions didn't work" ;;
esac

# --- 3. an explicit `"warns":false` hit is ALSO byte-identical ------------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T2","kb":"main","id":"def456abc123","warns":false}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out3="$(printf '%s' '{"session_id":"s3","cwd":"/tmp","prompt":"another question"}' | "$RECALL")"
case "$out3" in
  *"- T2  [main]"*) ok "explicit warns:false -> unchanged '- <title>' shape" ;;
  *) bad "explicit warns:false -> unchanged '- <title>' shape (got: $out3)" ;;
esac

# --- 4. a hit that is BOTH flagged and warns composes disputed-first ------
#        (the pinned composition: "- ⚠ disputed: ✗ didn't work: <title>").
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Old port advice","kb":"main","id":"333333333333","flagged":true,"warns":true}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out4="$(printf '%s' '{"session_id":"s4","cwd":"/tmp","prompt":"ports again"}' | "$RECALL")"
case "$out4" in
  *"- ⚠ disputed: ✗ didn't work: Old port advice  [main]"*)
    ok "flagged+warns hit composes '⚠ disputed: ✗ didn't work:' in that order" ;;
  *) bad "flagged+warns hit composes '⚠ disputed: ✗ didn't work:' in that order (got: $out4)" ;;
esac

# --- 5. mixed warns + flagged + ordinary hits in ONE response: each row ---
#        keeps its own prefix; the block's header line is untouched.
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"Dead end","kb":"main","id":"111111111111","warns":true},{"title":"Disputed one","kb":"main","id":"444444444444","flagged":true},{"title":"Fine one","kb":"main","id":"222222222222"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out5="$(printf '%s' '{"session_id":"s5","cwd":"/tmp","prompt":"mixed"}' | "$RECALL")"
case "$out5" in
  *"Relevant memories from kb (recall — these persist across sessions):"*"- ✗ didn't work: Dead end"*"- ⚠ disputed: Disputed one"*"- Fine one  [main]"*)
    ok "mixed hits: each row keeps its own prefix, header line unchanged" ;;
  *) bad "mixed hits: each row keeps its own prefix, header line unchanged (got: $out5)" ;;
esac

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
