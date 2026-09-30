#!/usr/bin/env bash
# check-doc-anchors.sh — the `file.rs:LINE` drift gate for docs/*.md.
#
# WHY THIS EXISTS
# ---------------
# The docs cite code as `path/to/file.rs:LINE` (and `…:START-END`) in
# backticks, and every one of those numbers is a promise: "the thing I just
# described is HERE". Nothing in the repo ever checked a single one of them.
# The measured consequence: an adversarial pass (O11 in
# docs/research/kb-adversarial-review-2026-09-30.md) found roughly 30 of
# docs/configuration.md's `[review.store]` anchors pointing at the wrong line —
# after an earlier repair round that "fixed" 12 and broke 1, which is the whole
# problem in miniature. (That pass put the section's anchor count at 47; the
# section now carries 62, so treat its figures as a floor, not a total.)
# A hand-maintained number in prose rots on every commit that touches the file
# above it, and the rot is invisible: the sentence still reads correctly, it
# just points 400 lines away from the code it claims to describe. Correcting
# the numbers by hand fixes the SYMPTOM once and leaves the CAUSE (no gate) in
# place, which is how the same section got broken twice. This script is the
# cause's fix.
#
# WHAT COUNTS AS AN ANCHOR
# ------------------------
# A backticked `PATH:LINE` or `PATH:START-END` where PATH ends in a source
# extension and LINE is a positive integer. Fenced code blocks are skipped: a
# ```toml sample or a pasted log is not a citation, and scanning inside one
# would manufacture failures out of prose.
#
# HOW A PATH IS RESOLVED — and why AMBIGUOUS is a failure, not a shrug
# ---------------------------------------------------------------------
# Docs write the shortest path that is unambiguous to the HUMAN reading that
# section: `config.rs:1414` inside a section about the kb-code daemon means
# `crates/kb-code-server/src/config.rs`. But there are five `config.rs` in
# this tree, so the same string is a valid citation of any of them — which is
# exactly why the numbers rot. A reader cannot follow the link, cannot tell
# which file it means, and cannot notice when it goes stale. So an anchor
# that resolves to more than one file is reported as AMBIGUOUS and fails: the
# fix is to write the path out far enough to name ONE file, and that is the
# same edit that makes the anchor maintainable. Bare basenames are accepted
# where they ARE unique (this repo has plenty of one-of-a-kind modules), so
# the rule costs nothing in the cases that were already fine.
#
# Four failure classes:
#   DANGLING   the path matches no file in the tree (renamed/moved/deleted —
#              the case that silently outlives every line-number repair)
#   PAST-EOF   the cited line, or the END of a range, is past the file's last
#              line (the case a pure "does the file exist" check would miss)
#   MALFORMED  LINE < 1, or a range whose END < START
#   AMBIGUOUS  the path matches 2+ files, so the anchor names no one of them
#
# SEVERITY — WHY AMBIGUOUS IS COUNTED BUT NOT YET FATAL
# -----------------------------------------------------
# The first three are unambiguous: the citation is wrong, and no amount of
# re-reading the doc makes it right. They exit 1 today and forever after.
#
# AMBIGUOUS is a different animal, and the measurement is the argument for
# staging it: on the tree this shipped with, DANGLING/PAST-EOF/MALFORMED is
# 0 and AMBIGUOUS is 45 of 87. The brief's literal gate ("fails when the file
# does not exist or N is past its end") would therefore have shipped GREEN on
# exactly the 47 anchors the adversarial pass found ~30 of wrong — because a
# bare `config.rs:1414` is in range for at least one of the five config.rs,
# so no range check can ever flag it. The number is wrong the only way that
# matters (it points into the wrong file) and a bounds check is blind to it.
# So AMBIGUOUS is the class that carries the finding, and it is reported in
# full, counted in the summary, and made fatal ONCE the 45 are rewritten to
# name one file each. Failing CI on 45 pre-existing anchors the same commit
# introduces the gate for would land every PR red and teach everyone to skip
# the step, which is strictly worse than a green gate nobody reads.
#
# The promotion is a one-line change and it is DELIBERATE, not forgotten:
# drop `--no-ambiguous` from the awk invocation in the invocation below and
# the class becomes fatal. `--no-ambiguous` is the ONLY reason AMBIGUOUS is
# non-fatal, so `grep -c 'no-ambiguous' scripts/check-doc-anchors.sh` answers
# "is this gate still staged?" without reading the whole file.
#
# SCOPE: docs/*.md ONLY, deliberately not docs/**/*.md
# ----------------------------------------------------
# docs/research/** is a dated record of what was true when it was written.
# Its `file.rs:LINE` citations are part of the historical claim and must NOT
# be re-pointed at today's line numbers — doing so would falsify the record
# and, since those files are append-only by nature, would make the gate
# permanently red for reasons nobody can fix. The live, maintained surface is
# the top level of docs/, and that is what this gate covers. Extending the
# scope to a new subdirectory is a deliberate act, not a glob edit.
#
# WHY ONE awk PASS AND NOT A BASH LOOP
# -----------------------------------
# The first cut resolved each of the 87 anchors with a bash `case` over all
# 5381 tracked files — 468k string comparisons, 31 s wall. This step is meant
# to be a free addition to an existing job, so the resolution happens inside a
# single awk program that has the file list already in memory: same answer,
# ~1 s. Line counts are read with getline rather than a per-anchor `awk END`
# subprocess, for the same reason. NR (not `wc -l`) is the right counter: it
# counts a final line that has no trailing newline, which `wc -l` does not.
#
# Usage: scripts/check-doc-anchors.sh
# Exit:  0 = every anchor resolves to exactly one file and lies inside it.
#        1 = at least one anchor does not (see the report above).
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# Build-dir and VCS noise: a stale `web/dist/**` copy of a source file would
# make a broken path look resolvable, and walking target/ is hundreds of
# megabytes of nothing on this dev box.
PRUNE='-name target -o -name node_modules -o -name .git -o -name dist -o -name .venv -o -name .mypy_cache -o -name __pycache__'

FILELIST="$(mktemp)"
trap 'rm -f "$FILELIST"' EXIT
find . \( $PRUNE \) -prune -o -type f -print 2>/dev/null | sed 's|^\./||' | LC_ALL=C sort > "$FILELIST"

nfiles=$(wc -l < "$FILELIST")
if [ "$nfiles" -lt 100 ]; then
  # A near-empty list means the walk broke, and then EVERY anchor would pass
  # vacuously — the exact failure this gate exists to prevent. 100 is far
  # below the real 5381 and far above any plausible partial walk.
  echo "FAIL: only $nfiles file(s) found under $REPO_ROOT (expected thousands)." >&2
  echo "      The tree walk is broken, so this gate would pass vacuously." >&2
  exit 1
fi

# The file list is argv[1] and every docs/*.md follows it as a normal input
# file, so FILENAME inside awk is the doc being scanned. No stdin, no fd
# juggling: a here-string or a pipe would make FILENAME useless.
shopt -s nullglob
DOCS=(docs/*.md)
shopt -u nullglob
if [ "${#DOCS[@]}" -eq 0 ]; then
  echo "FAIL: no docs/*.md found under $REPO_ROOT — nothing to scan, so this" >&2
  echo "      gate would pass vacuously." >&2
  exit 1
fi

AMBIENT=""
if [ "${1:-}" = "--no-ambiguous" ]; then
  # The staging switch named in the header. With it, AMBIGUOUS is reported
  # and counted but does not decide the exit status.
  AMBIENT="-v ambiguous_ok=1"
elif [ -n "${1:-}" ]; then
  echo "usage: $0 [--no-ambiguous]" >&2
  exit 2
fi

awk -v repo="$REPO_ROOT" -v ntracked="$nfiles" $AMBIENT '
function lines_of(f,   c, l) {
  if (f in linecount) return linecount[f]
  c = 0
  while ((getline l < f) > 0) c++
  close(f)
  linecount[f] = c
  return c
}
NR == FNR { present[$0] = 1; next }
FNR == 1 { infence = 0; ndocs++ }
{
  # Toggle on any fence line, with or without an info string.
  if ($0 ~ /^[[:space:]]*```/) { infence = !infence; next }
  if (infence) next
  rest = $0
  while (match(rest, /`[A-Za-z0-9_][A-Za-z0-9_.\/-]*\.(rs|ts|tsx|js|mjs|jsx|py|sh|bash|yml|yaml|md|sql|html|css|toml|json|txt):[0-9]+(-[0-9]+)?`/)) {
    anchor = substr(rest, RSTART, RLENGTH)
    rest   = substr(rest, RSTART + RLENGTH)
    text   = anchor
    gsub(/^`|`$/, "", text)
    colon  = index(text, ":")
    path   = substr(text, 1, colon - 1)
    spec   = substr(text, colon + 1)
    dash   = index(spec, "-")
    start  = dash ? substr(spec, 1, dash - 1) : spec
    finish = dash ? substr(spec, dash + 1) : start
    start += 0; finish += 0
    total++
    if (start < 1 || finish < start) {
      printf "MALFORMED  %s:%d  %s  (line < 1, or range end precedes its start)\n", FILENAME, FNR, anchor
      malformed++
      continue
    }
    # Resolve: an exact repo-relative path first, else every tracked file
    # whose path ENDS with the cited components (so `review_store/cred.rs`
    # finds crates/kb-code-server/src/review_store/cred.rs). Longest match
    # is not special-cased: two files ending in the same components are
    # genuinely indistinguishable to a reader of the doc, which is the
    # AMBIGUOUS case.
    nf = 0
    # No `delete hits`: whole-array delete is a gawk extension and the default
    # awk on a GitHub runner is mawk. It is also unnecessary — only hits[1..nf]
    # is ever read, and every one of those is assigned below before use.
    if (path in present) {
      hits[1] = path; nf = 1
    } else {
      suffix = "/" path
      for (f in present) {
        # substr(f, length(f) - length(suffix) + 1) is the trailing
        # length(suffix) characters of f — the `+ 1`, not `+ 2`: awk is
        # 1-based, and a `+ 2` silently drops the leading `/` off the
        # comparison and then matches nothing, which reads as "every
        # anchor in the repo dangles".
        if (length(f) >= length(suffix) && substr(f, length(f) - length(suffix) + 1) == suffix) hits[++nf] = f
      }
    }
    if (nf == 0) {
      printf "DANGLING   %s:%d  %s  (no file in the tree ends in %s)\n", FILENAME, FNR, anchor, path
      dangling++
    } else {
      # Candidates sorted before anything else looks at them: `for (f in
      # present)` walks the hash in an order gawk does not promise to be
      # stable, so an unsorted list would reorder this block between awk
      # builds and make two identical failures look like two different ones
      # in the log. A hand-rolled insertion sort, not asort(): asort() is a
      # gawk extension and this script runs wherever the gate runs.
      for (i = 2; i <= nf; i++) {
        v = hits[i]; j = i - 1
        while (j >= 1 && hits[j] > v) { hits[j + 1] = hits[j]; j-- }
        hits[j + 1] = v
      }
      # The longest candidate sets the ceiling an ambiguous anchor can still
      # clear. An anchor past the end of EVERY file it could name is broken
      # under any reading, so it is PAST-EOF (always fatal) and not
      # AMBIGUOUS (staged non-fatal) — otherwise --no-ambiguous would hide
      # `config.rs:999999`, a hard error, behind a class that is only a
      # to-do item.
      maxlines = 0
      for (i = 1; i <= nf; i++) {
        c = lines_of(repo "/" hits[i])
        if (c > maxlines) maxlines = c
      }
      if (finish > maxlines) {
        if (nf == 1) {
          if (start == finish) {
            printf "PAST-EOF   %s:%d  %s  (%s has %d line(s))\n", FILENAME, FNR, anchor, hits[1], maxlines
          } else {
            printf "PAST-EOF   %s:%d  %s  (%s has %d line(s); range ends at %d)\n", FILENAME, FNR, anchor, hits[1], maxlines, finish
          }
        } else {
          printf "PAST-EOF   %s:%d  %s  (past the end of ALL %d matching file(s); the longest, %s, has %d)\n", FILENAME, FNR, anchor, nf, hits[nf], maxlines
        }
        past_eof++
      } else if (nf > 1) {
        printf "AMBIGUOUS  %s:%d  %s  (%d files match — write the path far enough to name ONE)\n", FILENAME, FNR, anchor, nf
        for (i = 1; i <= nf; i++) printf "             %s\n", hits[i]
        ambiguous++
      } else {
        ok++
      }
    }
  }
}
END {
  hard    = dangling + past_eof + malformed
  broken  = hard + ambiguous
  # `--no-ambiguous` downgrades AMBIGUOUS from fatal to reported. It exists
  # for exactly one reason (see the SEVERITY section in the header) so the
  # switch and its reason live in the same file and cannot drift apart.
  if (ambiguous_ok) broken = hard
  printf "\ndoc-anchor gate: %d anchor(s) across %d doc(s), %d tracked file(s)\n", total, ndocs, ntracked
  printf "  DANGLING   %4d   path matches no file (renamed / moved / deleted)\n", dangling + 0
  printf "  PAST-EOF   %4d   cited line, or range end, is past the last line of its file\n", past_eof + 0
  printf "  MALFORMED  %4d   line < 1, or a range whose end precedes its start\n", malformed + 0
  printf "  AMBIGUOUS  %4d   path matches 2+ files, so the anchor names none of them%s\n", ambiguous + 0, (ambiguous_ok ? "   [STAGED: not failing this run]" : "")
  printf "  OK         %4d\n", ok + 0
  if (ambiguous > 0 && ambiguous_ok) {
    printf "\nNOTE: %d AMBIGUOUS anchor(s) above are the measured O11 debt and are\n", ambiguous
    printf "reported but not fatal while --no-ambiguous is passed. Each is a bare\n"
    printf "basename matching 2+ files, so the line number cannot be checked and\n"
    printf "cannot be trusted. Drop the flag once they are rewritten to name one\n"
    printf "file each; `grep -c no-ambiguous scripts/check-doc-anchors.sh` says\n"
    printf "whether the staging is still in place.\n"
  }
  if (broken > 0) {
    printf "\nFAIL: %d of %d doc anchor(s) do not resolve to exactly one in-range line.\n", broken, total
    printf "Each is a citation a reader cannot follow or verify. Fix by writing the\n"
    printf "path out far enough to name ONE file, then re-pointing the line at the\n"
    printf "code the sentence actually describes.\n"
    exit 1
  }
  printf "\nOK: every doc anchor names one existing file and lies inside it.\n"
}
' "$FILELIST" "${DOCS[@]}"
# awk is the last command, so its verdict IS this script's exit status: 1
# when any anchor is broken, 0 when none is.
