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
# SEVERITY — every class is fatal
# -------------------------------
# DANGLING, PAST-EOF, MALFORMED and AMBIGUOUS all exit 1. AMBIGUOUS was staged
# non-fatal (`--no-ambiguous`) from 2026-09-30 until the 45-anchor debt was
# rewritten the same day; the flag is gone from this script and the awk
# program no longer has a staged mode, so there is nothing to grep for.
#
# THE CLASS THE BOUNDS CHECK CANNOT SEE — "right file, WRONG line"
# ----------------------------------------------------------------
# An anchor that names the right file but a line that has since moved is in
# range, resolves to one file, and passes every check above. Two such anchors
# sat green in docs/web-internals.md (`CommentsPanelProps` cited at
# CommentsPanel.tsx:13, defined at :95). So an anchor may CARRY ITS CLAIM:
#
#     `Symbol` (`path.rs:N`)            symbol BEFORE the anchor
#     `path.rs:N` (`Symbol`)            symbol right AFTER it (also without parens)
#
# and the gate then asserts the identifier occurs, as a whole word, within
# [N-3, END+3] of the cited file. Otherwise SYMBOL (fatal), naming the nearest
# line where the identifier really is. Only the EXPLICIT adjacent pairing is
# checked -- not "any identifier on the line" -- so a long table row that
# names several things cannot manufacture false positives.
#
# `--fix` rewrites each SYMBOL failure's N to the symbol's current definition
# line (first `fn|struct|enum|trait|type|const|class|function|interface ...
# Symbol`, else the first whole-word occurrence), keeping a range's width.
# It edits the docs in place, never docs/research/** (the scope rule below).
#
# An anchor with NO adjacent symbol is WEAK: it resolves and is in range, but
# nothing can ever tell whether the line is still the right one. WEAK is not
# fatal per anchor; it is held under WEAK_CEILING below, which may only go
# DOWN (being under it is a printed note, never a failure): pairing an anchor with its symbol lowers the real count, and the next
# person lowers the constant to match. A NEW unpaired anchor raises the count
# and fails the build, so the debt cannot grow quietly.
#
# SOURCE COMMENTS (A10.f5): the same adjacent-pair check runs over comment
# lines in tracked *.rs/*.ts/*.tsx under crates/, web/src, web-code/src and
# tests/. There only PAIRED citations are judged (a comment's `src/lib.rs:42`
# is usually an illustrative example, not a promise), so an unpaired
# `file:line` in a comment is neither WEAK nor failing -- but one that carries
# a symbol cannot drift unnoticed.
#
# `pinned by` / `file.rs::test_name` citations are resolved by the sibling
# scripts/check-pinned-by.sh, which `just doc-anchors` runs after this one.
#
# SCOPE: docs/*.md plus every tracked CLAUDE.md -- NOT docs/**/*.md
# -----------------------------------------------------------------
# (The CLAUDE.md files are loaded into every agent session and used to be
# ungated.)
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
# Usage: scripts/check-doc-anchors.sh [--fix]
#        scripts/check-doc-anchors.sh --self-test
# Exit:  0 = every anchor resolves to exactly one file, lies inside it, and
#            every symbol-paired anchor lands on its symbol; WEAK <= ceiling.
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
# every tracked CLAUDE.md (not the untracked CLAUDE.local.md), outside docs/research
while IFS= read -r f; do DOCS+=("$f"); done < <(git ls-files 'CLAUDE.md' '*/CLAUDE.md' | grep -v '^docs/research/' || true)
if [ "${#DOCS[@]}" -eq 0 ]; then
  echo "FAIL: no docs/*.md found under $REPO_ROOT — nothing to scan, so this" >&2
  echo "      gate would pass vacuously." >&2
  exit 1
fi

FIX=0
case "${1:-}" in
  "") ;;
  --fix) FIX=1 ;;
  --self-test) exec "$SCRIPT_DIR/check-doc-anchors-selftest.sh" ;;
  *) echo "usage: $0 [--fix | --self-test]" >&2; exit 2 ;;
esac

# The number of anchors with no adjacent `Symbol` pairing, as of this commit.
# It may only go DOWN (see the header): pair an anchor, then lower this.
WEAK_CEILING=84

# Source-comment citations: only files that contain a `file:LINE`-shaped
# token at all (the full tree would make the awk pass walk hundreds of
# thousands of lines for nothing).
SRCLIST="$(mktemp)"
FIXFILE="$(mktemp)"
trap 'rm -f "$FILELIST" "$SRCLIST" "$FIXFILE"' EXIT
# KB_ANCHOR_SRC_PATHS (space separated pathspecs) exists for the self-test's scratch tree.
read -r -a SRC_PATHS <<< "${KB_ANCHOR_SRC_PATHS:-crates/*.rs web/src/*.ts web/src/*.tsx web-code/src/*.ts web-code/src/*.tsx tests/*.ts}"
git grep -lE '[A-Za-z0-9_]+\.(rs|ts|tsx|js|py|sh):[0-9]+' -- "${SRC_PATHS[@]}" 2>/dev/null | LC_ALL=C sort > "$SRCLIST" || true

awk -v repo="$REPO_ROOT" -v ntracked="$nfiles" -v weak_ceiling="$WEAK_CEILING" -v fixfile="$FIXFILE" '
# Lazily load a file into L[f,i] / N[f]; NR-style count (a final line without a
# trailing newline still counts, which `wc -l` would miss).
function load(f,   c, l) {
  if (f in N) return N[f]
  c = 0
  while ((getline l < f) > 0) { c++; L[f, c] = l }
  close(f)
  N[f] = c
  return c
}
function lines_of(f) { return load(f) }
# Whole-word occurrence of identifier `sym` on line text `t`.
function has_word(t, sym) {
  return match(t, "(^|[^A-Za-z0-9_])" sym "([^A-Za-z0-9_]|$)")
}
# Last identifier segment of `A::b`, `A#b`, `A.b`.
function last_seg(x,   i) {
  while ((i = match(x, /(::|[#.])/)) > 0) x = substr(x, RSTART + RLENGTH)
  return x
}
# Is there a definition-looking line for sym? Returns the line number or 0.
function def_line(f, sym,   i, n, t, re) {
  n = load(f)
  re = "(^|[^A-Za-z0-9_])(fn|struct|enum|trait|type|const|static|mod|union|interface|class|function|def|let|var|macro_rules!)[[:space:]]+(mut[[:space:]]+)?" sym "([^A-Za-z0-9_]|$)"
  for (i = 1; i <= n; i++) if (match(L[f, i], re)) return i
  return 0
}
function first_word(f, sym,   i, n) {
  n = load(f)
  for (i = 1; i <= n; i++) if (has_word(L[f, i], sym)) return i
  return 0
}
# Nearest whole-word line to `near`, searching the whole file; 0 when absent.
function nearest_word(f, sym, near,   i, n, best, bd, d) {
  n = load(f); best = 0; bd = 1e9
  for (i = 1; i <= n; i++) if (has_word(L[f, i], sym)) {
    d = i > near ? i - near : near - i
    if (d < bd) { bd = d; best = i }
  }
  return best
}
FILENAME == ARGV[1] { present[$0] = 1; next }
FILENAME == ARGV[2] { srcset[$0] = 1; next }
FNR == 1 { infence = 0; if (FILENAME in srcset) insrc = 1; else { insrc = 0; ndocs++ } }
{
  if (insrc) {
    # Only comment text; a `file.rs:12` in code (a string literal in a test)
    # is data, not a promise.
    if ($0 !~ /(\/\/|^[[:space:]]*\*|^[[:space:]]*#)/) next
  } else {
    # Toggle on any fence line, with or without an info string.
    if ($0 ~ /^[[:space:]]*```/) { infence = !infence; next }
    if (infence) next
  }
  rest = $0
  while (match(rest, /`[A-Za-z0-9_][A-Za-z0-9_.\/-]*\.(rs|ts|tsx|js|mjs|jsx|py|sh|bash|yml|yaml|md|sql|html|css|toml|json|txt):[0-9]+(-[0-9]+)?`/)) {
    anchor = substr(rest, RSTART, RLENGTH)
    pre    = substr($0, 1, length($0) - length(rest) + RSTART - 1)
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

    # Adjacent symbol pairing: `Sym` (`path:N`)  |  `path:N` (`Sym`) / `path:N` `Sym`
    sym = ""
    if (match(pre, /`[A-Za-z_][A-Za-z0-9_]*((::|[#.])[A-Za-z_][A-Za-z0-9_]*)*`[[:space:]]*(\(|@|at )[[:space:]]*$/)) {
      t = substr(pre, RSTART, RLENGTH)
      sub(/^`/, "", t); sub(/`[[:space:]]*(\(|@|at )[[:space:]]*$/, "", t)
      sym = last_seg(t)
    } else if (match(rest, /^[[:space:]]*\(?`[A-Za-z_][A-Za-z0-9_]*((::|[#.])[A-Za-z_][A-Za-z0-9_]*)*`/)) {
      t = substr(rest, RSTART, RLENGTH)
      sub(/^[[:space:]]*\(?`/, "", t); sub(/`$/, "", t)
      sym = last_seg(t)
    }

    # A one- or two-letter "symbol" is a key binding or a prose word (`u`, `gO`),
    # not an identifier; in source comments require code-shaped names
    # (snake_case or mixed case) so ordinary prose never pairs.
    if (sym != "" && length(sym) < 3) sym = ""
    if (insrc && sym != "" && sym !~ /_/ && sym !~ /[a-z][A-Z]/ && sym !~ /^[A-Z][a-z]+[A-Z]/) sym = ""
    if (!insrc) total++
    if (start < 1 || finish < start) {
      if (!insrc) { printf "MALFORMED  %s:%d  %s  (line < 1, or range end precedes its start)\n", FILENAME, FNR, anchor; malformed++ }
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
    if (insrc) {
      # Source comments: judge ONLY a symbol-paired citation of a uniquely
      # resolved, in-range file (see the header). Everything else is skipped.
      if (sym == "" || nf != 1) continue
      srcpairs++
      if (finish > lines_of(repo "/" hits[1])) continue
    } else if (nf == 0) {
      printf "DANGLING   %s:%d  %s  (no file in the tree ends in %s)\n", FILENAME, FNR, anchor, path
      dangling++
      continue
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
      # under any reading, so it is PAST-EOF and not AMBIGUOUS.
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
        continue
      } else if (nf > 1) {
        printf "AMBIGUOUS  %s:%d  %s  (%d files match — write the path far enough to name ONE)\n", FILENAME, FNR, anchor, nf
        for (i = 1; i <= nf; i++) printf "             %s\n", hits[i]
        ambiguous++
        continue
      }
    }

    # nf == 1 and in range: the symbol check.
    if (sym == "") { weak++; continue }
    f1 = repo "/" hits[1]
    load(f1)
    lo = start - 3; if (lo < 1) lo = 1
    hi = finish + 3; if (hi > N[f1]) hi = N[f1]
    found = 0
    for (i = lo; i <= hi; i++) if (has_word(L[f1, i], sym)) { found = 1; break }
    if (found) { paired++; continue }
    # Wrong line. Name where the symbol really is.
    near = nearest_word(f1, sym, start)
    where = FILENAME ":" FNR
    if (near > 0) {
      d = def_line(f1, sym); if (d == 0) d = first_word(f1, sym)
      printf "SYMBOL     %s  %s  (`%s` is not within 3 lines of %s; defined/used at line %d, nearest use at %d)\n", where, anchor, sym, spec, d, near
      width = finish - start
      newspec = width > 0 ? d "-" (d + width) : d
      printf "%s\t%d\t%s\t%s\n", FILENAME, FNR, anchor, "`" path ":" newspec "`" >> fixfile
    } else {
      printf "SYMBOL     %s  %s  (`%s` does not occur anywhere in %s)\n", where, anchor, sym, hits[1]
    }
    symbol_bad++
  }
}
END {
  hard    = dangling + past_eof + malformed + ambiguous + symbol_bad
  weak_over = (weak > weak_ceiling)
  printf "\ndoc-anchor gate: %d anchor(s) across %d doc(s) (+%d symbol-paired citation(s) in source comments), %d tracked file(s)\n", total, ndocs, srcpairs + 0, ntracked
  printf "  DANGLING   %4d   path matches no file (renamed / moved / deleted)\n", dangling + 0
  printf "  PAST-EOF   %4d   cited line, or range end, is past the last line of its file\n", past_eof + 0
  printf "  MALFORMED  %4d   line < 1, or a range whose end precedes its start\n", malformed + 0
  printf "  AMBIGUOUS  %4d   path matches 2+ files, so the anchor names none of them\n", ambiguous + 0
  printf "  SYMBOL     %4d   a symbol-paired anchor whose symbol is not within 3 lines of it\n", symbol_bad + 0
  printf "  paired OK  %4d   anchors verified against their symbol\n", paired + 0
  printf "  WEAK       %4d   resolve and are in range but carry no symbol (ceiling %d; may only go down)\n", weak + 0, weak_ceiling
  if (hard > 0) {
    printf "\nFAIL: %d doc anchor(s) are wrong. Each is a citation a reader cannot follow or\n", hard
    printf "verify. Write the path far enough to name ONE file and re-point the line at the\n"
    printf "code the sentence describes (`scripts/check-doc-anchors.sh --fix` re-points\n"
    printf "symbol-paired anchors for you).\n"
  }
  if (weak_over) {
    printf "\nFAIL: %d WEAK anchor(s) exceeds the ceiling of %d. A new `file:LINE` citation must name its\n", weak, weak_ceiling
    printf "symbol -- write `Symbol` (`path.rs:N`) -- so the gate can tell when it goes stale.\n"
  }
  if (weak < weak_ceiling) {
    printf "\nNOTE: WEAK is %d, below the ceiling %d: lower WEAK_CEILING in this script to %d so the ratchet holds (not a failure).\n", weak, weak_ceiling, weak
  }
  if (hard > 0 || weak_over) exit 1
  printf "\nOK: every doc anchor names one existing file, lies inside it, and every symbol-paired anchor lands on its symbol.\n"
}
' "$FILELIST" "$SRCLIST" "${DOCS[@]}" $(cat "$SRCLIST")
rc=$?

if [ "$FIX" -eq 1 ]; then
  if [ ! -s "$FIXFILE" ]; then
    echo "--fix: nothing to re-point."
  else
    # Never touch the dated record.
    while IFS=$'\t' read -r file lineno old new; do
      case "$file" in docs/research/*) continue ;; esac
      esc_old="$(printf '%s' "$old" | sed 's/[][\.*^$/|]/\\&/g')"
      esc_new="$(printf '%s' "$new" | sed 's/[\&|]/\\&/g')"
      sed -i "${lineno}s|${esc_old}|${esc_new}|" "$file"
      echo "--fix: $file:$lineno  $old -> $new"
    done < "$FIXFILE"
    echo "--fix: re-run the gate to confirm."
  fi
fi
exit "$rc"
