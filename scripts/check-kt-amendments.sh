#!/usr/bin/env bash
# check-kt-amendments.sh — compile-free gate over the KT (2026-11) carve-outs.
#
# WHY THIS EXISTS
# ---------------
# T22 amended five numbered invariants (#4, #11, #13, #23, #24) with
# "KT amendment (2026-11)" paragraphs and mirrored one-line pointers in
# the CLAUDE.md index plus the phone-DRIVE carve-out in the mobile mirrors
# (CLAUDE.md, README.md, docs/web-ui.md). The originating brief listed the
# wording checks as a builder-run checklist, not a CI gate, so deleting,
# re-labelling or reverting a carve-out left CI green. This script closes
# that gap at the cause: it asserts on COMMITTED TEXT (grep/awk only, no
# compiler, no build) that every KT pointer the rulings require is still
# present, and that the two T11-decided refinements of #4 (same-host Origin
# admission and the bearer/registry-token refusal) have not been reverted
# to the pre-T11 wording.
#
# WHAT IS CHECKED (fatal, exit 1 on any miss)
#   * docs/architecture-invariants.md: the body of each of sections #4,
#     #11, #13, #23 and #24 (up to the next "### N." heading) contains
#     the literal "KT amendment (2026-11)".
#   * Same file, section #4 only: the implemented Origin rule is written
#     down — parent Origin OR http(s) scheme+Host equal to the request
#     Host, with artifact-suffix and KB_DEV_ORIGIN_ANY rejected — and
#     the decided bearer/registry-token refusal is recorded
#     (refuse_token_callers, escape hatch), NOT left as an open decision.
#   * Same file: the "### 4." heading itself names the terminal-tier
#     exception (it must not read as a universal bypass to a
#     heading-only reader).
#   * CLAUDE.md: the one-liner items 4, 11, 13, 23 and 24 each carry
#     their KT pointer ("*KT" plus the section's "see #N" back-reference),
#     and item 4's heading names the terminal-tier exception.
#   * Mobile mirrors keep the phone-DRIVE carve-out: CLAUDE.md and
#     README.md keep the "phone may DRIVE/drive a live terminal"
#     sentences, docs/web-ui.md keeps the terminal-view cross-reference
#     into architecture-invariants #11.
#
# Failure messages quote the missing token verbatim so a revert names its
# own fix. A deletion of any asserted string fails; so does the
# pre-FXT22 wording of #4 (which under-described the admitted origins and
# left the bearer refusal undecided).
#
# Usage: scripts/check-kt-amendments.sh            (exit 1 on any miss)
#        scripts/check-kt-amendments.sh --self-test
set -uo pipefail

# Section-body extractor: prints lines of FILE between the "### N." heading
# and the next "### M." heading (or EOF). Uses only POSIX awk on committed
# text — deliberately allocation-light and compile-free, like the
# doc-anchor gate's awk pass.
section_body() { # $1=file $2=number
  awk -v n="$2" '
    /^### / {
      heading = 1
      if ($0 ~ "^### " n "\\. ") in_sec = 1; else in_sec = 0
      next
    }
    heading == 1 && in_sec == 1 { print }
  ' "$1"
}

# $1=file $2=number $3=fixed needle -> rc 0 iff needle found in the body.
# Deliberately NOT a pipeline into grep -q: under `set -o pipefail` a
# closing grep -q SIGPIPEs the awk writer and the pipeline reports
# failure even when the needle IS present (the failure mode this gate
# must not have). Command-substitution + shell substring is immune.
section_has() {
  local body="$3"
  body="$(section_body "$1" "$2")"
  case "$body" in
    *"$3"*) return 0 ;;
    *) return 1 ;;
  esac
}

# CLAUDE.md one-liner extractor: prints the "N." index line. Items are
# right-aligned single-column (one physical line each): single-digit
# items carry a leading space (" 4. ..."), two-digit items do not
# ("11. ...") — the pattern must accept both.
claude_item() { # $1=file $2=number
  awk -v n="$2" '
    $0 ~ "^ ?" n "\\. " { print; exit }
  ' "$1"
}

# Fixed-string presence in FILE, tolerant of the mirrors' text wrap:
# prose wraps across physical lines, so join lines before matching.
file_has_wrapped() { # $1=file $2=phrase
  local joined
  joined="$(tr '\n' ' ' < "$1")"
  case "$joined" in
    *"$2"*) return 0 ;;
    *) return 1 ;;
  esac
}

fail=0
note_fail() { printf 'KT-GATE FAIL  %s\n' "$1" >&2; fail=1; }

check_tree() { # $1=repo root (invariants + CLAUDE + mirrors live below it)
  local root="$1"
  local inv="$root/docs/architecture-invariants.md"
  local claude="$root/CLAUDE.md"
  local readme="$root/README.md"
  local webui="$root/docs/web-ui.md"
  local n
  local line

  # --- 1. each KT section carries the dated amendment marker ---
  for n in 4 11 13 23 24; do
    if section_has "$inv" "$n" "KT amendment (2026-11)"; then
      :
    else
      note_fail "docs/architecture-invariants.md #${n}: missing 'KT amendment (2026-11)' in the section body"
    fi
  done

  # --- 2. section #4 records the IMPLEMENTED Origin rule (FXT22 fix 1) ---
  if section_has "$inv" 4 "same-host Origin"; then :; else note_fail "docs/architecture-invariants.md #4: missing same-host Origin admission ('same-host Origin')"; fi
  if section_has "$inv" 4 "KB_DEV_ORIGIN_ANY"; then :; else note_fail "docs/architecture-invariants.md #4: missing KB_DEV_ORIGIN_ANY rejection"; fi

  # --- 3. section #4 records the DECIDED bearer refusal (FXT22 fix 2) ---
  if section_has "$inv" 4 "refuse_token_callers"; then :; else note_fail "docs/architecture-invariants.md #4: missing decided 'refuse_token_callers' posture"; fi
  if section_has "$inv" 4 "escape hatch"; then :; else note_fail "docs/architecture-invariants.md #4: missing 'escape hatch' (reverse-proxy) wording"; fi
  local body4
  body4="$(section_body "$inv" 4)"
  case "$body4" in
    *"An optional refusal of bearer/registry-token callers is an open"*)
      note_fail "docs/architecture-invariants.md #4: still carries the undecided-bearer wording ('An optional refusal ... is an open')"
      ;;
  esac

  # --- 4. the ### 4. heading names the terminal-tier exception (FXT22 fix 4) ---
  if grep -qE '^### 4\. .*terminal tier' "$inv"; then
    :
  else
    note_fail "docs/architecture-invariants.md '### 4.' heading: does not name the terminal-tier exception (heading-only readers get a universal bypass)"
  fi

  # --- 5. CLAUDE.md index one-liners carry the KT pointers ---
  for n in 4 11 13 23 24; do
    line="$(claude_item "$claude" "$n")"
    case "$line" in
      *"*KT"*#"${n}"*) ;;
      *) note_fail "CLAUDE.md item ${n}: missing KT pointer ('*KT ... see #${n}')";;
    esac
  done
  # item 4's heading itself must name the exception (FXT22 fix 4)
  if claude_item "$claude" 4 | grep -qF -e "(except terminal tier, KT)"; then
    :
  else
    note_fail "CLAUDE.md item 4: heading does not name the terminal-tier exception ('(except terminal tier, KT)')"
  fi

  # --- 6. mobile mirrors keep the phone-DRIVE carve-out ---
  if file_has_wrapped "$claude" "phone may also DRIVE a live terminal"; then
    :
  else
    note_fail "CLAUDE.md mobile non-goal: lost 'phone may also DRIVE a live terminal' carve-out"
  fi
  if file_has_wrapped "$readme" "phone may drive a live agent terminal"; then
    :
  else
    note_fail "README.md mobile non-goal: lost 'phone may drive a live agent terminal' carve-out"
  fi
  if file_has_wrapped "$webui" "A terminal view for the operator's own sessions"; then
    :
  else
    note_fail "docs/web-ui.md mobile: lost 'A terminal view for the operator's own sessions' cross-reference"
  fi
  if file_has_wrapped "$webui" "architecture-invariants"; then
    :
  else
    note_fail "docs/web-ui.md mobile: terminal-view cross-reference lost its architecture-invariants pointer"
  fi

  return "$fail"
}

if [ "${1:-}" = "--self-test" ]; then
  # Prove the gate FAILS on the reverted wording and PASSES on the real
  # tree. Mutations are made on COPIES under a temp dir; the repo is never
  # written. Each mutation must flip the gate to failure — a gate that
  # cannot catch a deletion is worse than no gate.
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  TREE="$tmp/tree"; mkdir -p "$TREE/docs"
  cp "$root/docs/architecture-invariants.md" "$TREE/docs/"
  cp "$root/CLAUDE.md" "$TREE/"
  cp "$root/README.md" "$TREE/"
  cp "$root/docs/web-ui.md" "$TREE/docs/"
  # self-test harness: run check_tree on the copy; expect rc=$2.
  expect() { # $1=label $2=expected-rc
    local rc=0
    ( set +e; check_tree "$TREE" >/dev/null 2>&1; echo "$?" > "$tmp/rc" )
    rc="$(cat "$tmp/rc")"
    if [ "$rc" -ne "$2" ]; then
      echo "SELFTEST FAIL: $1 -> rc=$rc, want $2" >&2
      fail=1
    else
      echo "selftest ok: $1"
    fi
  }
  expect "pristine tree passes" 0
  # 1. deleting a KT amendment marker must fail.
  sed -i 's/KT amendment (2026-11)/KT amendment/g' "$TREE/docs/architecture-invariants.md"
  expect "KT markers deleted -> fail" 1
  cp "$root/docs/architecture-invariants.md" "$TREE/docs/"
  # 2. reverting the same-host admission must fail.
  sed -i 's/same-host Origin//g' "$TREE/docs/architecture-invariants.md"
  expect "same-host wording reverted -> fail" 1
  cp "$root/docs/architecture-invariants.md" "$TREE/docs/"
  # 3. reinstating the undecided-bearer wording must fail.
  # 3. reinstating the undecided-bearer wording in section #4 must fail.
  sed -i 's/Everything else about #4/Everything else about #4 (old wording reinstated: An optional refusal of bearer\/registry-token callers is an open decision)/' "$TREE/docs/architecture-invariants.md"
  expect "undecided-bearer wording reinstated -> fail" 1
  cp "$root/docs/architecture-invariants.md" "$TREE/docs/"
  # 4. dropping the heading exception must fail.
  sed -i 's/ (except terminal tier, KT)//g' "$TREE/docs/architecture-invariants.md"
  expect "invariants heading exception dropped -> fail" 1
  cp "$root/docs/architecture-invariants.md" "$TREE/docs/"
  # 5. dropping a CLAUDE KT pointer must fail.
  sed -i 's/\*KT:\*/REMOVED:/g' "$TREE/CLAUDE.md"
  expect "CLAUDE KT pointers removed -> fail" 1
  cp "$root/CLAUDE.md" "$TREE/"
  # 6. dropping the phone-DRIVE carve-out must fail.
  sed -i 's/DRIVE a live terminal/REMOVED/g' "$TREE/CLAUDE.md"
  expect "phone-DRIVE carve-out removed -> fail" 1
  cp "$root/CLAUDE.md" "$TREE/"
  # 7. dropping the web-ui cross-reference must fail.
  sed -i "s/A terminal view for the operator's own sessions/REMOVED/" "$TREE/docs/web-ui.md"
  expect "web-ui terminal cross-reference removed -> fail" 1
  if [ "$fail" -eq 0 ]; then echo "check-kt-amendments self-test ok"; fi
  exit "$fail"
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
check_tree "$root" && echo "KT-amendment gate: #4/#11/#13/#23/#24 amendments, CLAUDE pointers and mobile carve-outs present"
exit "$fail"
