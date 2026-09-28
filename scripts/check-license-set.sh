#!/usr/bin/env bash
# check-license-set.sh — the CI-reproducible half of the NOTICE drift gate.
#
# WHY THIS EXISTS, AND WHY IT IS NOT A BYTE COMPARISON
# ---------------------------------------------------
# `licenses-check` (justfile) regenerates THIRD-PARTY-LICENSES.md and fails if
# the committed bytes differ. That byte comparison is UNSOUND, and the reason
# is measured, not guessed: on this exact commit, `cargo about generate about.hbs`
# rendered with a WARM ~/.cargo produced file A, and the SAME command against
# the SAME commit with an empty CARGO_HOME (cold) produced file B. A and B
# differ by ~90 lines — the per-crate `used_by` entries REORDER (e.g. `jsonb
# 0.5.6` moves position) and `subtle 2.6.1`'s license body shifts section.
# cargo-about's per-crate license-text gathering and its output ordering are
# not a function of the commit alone, so on a hosted CI runner (always a cold
# cache) the committed file can never match and the gate is permanently red.
#
# So the byte gate is not "flaky" — it is unsatisfiable off the author's
# machine. What IS deterministic is the `## Overview` block: the per-license
# counts (`- MIT License: 611`) are byte-identical between a warm and a cold
# CARGO_HOME, because they come from the resolved graph and not from per-crate
# text gathering or output order. That is the invariant this script checks, and
# it is the one that carries the supply-chain signal: a newly-pulled license, a
# dropped license, or a license whose usage count moved.
#
# ONE MORE THING has to be pinned for this to hold, and it is not obvious: the
# cargo-about VERSION. The counts are stable across cache states but NOT across
# tool versions — rendering this same commit with cargo-about 0.9.0 gives
# MIT 606 / Apache-2.0 151 / ISC 10 / BSD-3-Clause 8, and with 0.9.2 gives
# MIT 611 / Apache-2.0 152 / ISC 25 / BSD-3-Clause 10. ci.yml pins 0.9.2 for
# that reason. If you regenerate this file with a different cargo-about than
# the pin, this script will report drift that is really just a version skew,
# and re-running it with the pinned version is the fix, not a `git checkout`.
#
# WHAT IT WILL CATCH:   the license SET drifting from the dep graph — a new
#   SPDX id / license name appearing, one disappearing, or a count changing,
#   without anyone running `just licenses` and committing the result.
# WHAT IT WILL NOT:    text-body drift inside one license's section, a crate
#   swapping license A for license B with the same headcount, or any other
#   byte-level staleness in the NOTICE. That remains `just licenses-check`'s
#   job — useful for a deliberate local regeneration sweep, just not a
#   portable gate. The two recipes are complementary, not redundant.
#
# THE COMMITTED FILE IS NEVER TOUCHED. The fresh render goes to a mktemp file
# removed by an EXIT trap, so this script is safe to run on a dirty tree: it
# must not be able to destroy the very file it is verifying.
#
# Cost: the fresh render is `cargo about generate`, which is a `cargo metadata`
# resolve plus a handlebars render — it does NOT compile. This lives in the
# supply-chain job, which is budgeted at timeout-minutes: 30 and must never
# grow a build.
#
# Usage: scripts/check-license-set.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

NOTICE="THIRD-PARTY-LICENSES.md"

TMPDIR_LIC="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_LIC"' EXIT

FRESH="$TMPDIR_LIC/THIRD-PARTY-LICENSES.md"
COMMITTED_VIEW="$TMPDIR_LIC/committed.txt"
FRESH_VIEW="$TMPDIR_LIC/fresh.txt"

die() { echo "ERROR: $*" >&2; exit 1; }

# The `## Overview` section: every line between the `## Overview` heading and
# the `---` rule that closes it, with blank lines dropped, runs of internal
# whitespace collapsed, and the result sorted. Sorting is what makes the
# comparison a property of the SET rather than of render order — the whole
# point, given cargo-about reorders its per-license `used_by` lists across
# cache states.
extract_overview() {
    awk '
        /^## Overview[[:space:]]*$/ { in_overview = 1; next }
        in_overview && /^---[[:space:]]*$/ { exit }
        in_overview { print }
    ' "$1" \
    | sed -e 's/[[:space:]]\+/ /g' -e 's/^ //' -e 's/ $//' \
    | grep -v '^$' \
    | LC_ALL=C sort \
    || true   # zero lines: an empty Overview is reported by the caller below, not by a silent set -e exit
}

[ -f "$NOTICE" ] || die "$NOTICE is missing — run 'just licenses' and commit it."

# A silent cargo failure (e.g. no toolchain) would render an EMPTY file and
# then "pass" as a license-set change with a useless message, so check the
# tool up front and check the render's shape after.
command -v cargo >/dev/null 2>&1 || die "cargo not found on PATH."
cargo about --version >/dev/null 2>&1 || die \
    "cargo-about not found on PATH — cargo install --locked cargo-about --features cli"

echo "==> rendering a fresh NOTICE into a temp file (no compile; graph resolve + handlebars render)"
cargo about generate about.hbs >"$FRESH" || die \
    "'cargo about generate about.hbs' failed — the license set could not be derived."

# Fresh render must be a file we can read an Overview out of, and the committed
# file must actually HAVE an Overview. A template regression (a renamed heading,
# a dropped section) would otherwise compare empty-vs-empty and pass.
grep -q '^## Overview[[:space:]]*$' "$FRESH" \
    || die "the fresh render has no '## Overview' section — about.hbs changed shape?"
grep -q '^## Overview[[:space:]]*$' "$NOTICE" \
    || die "$NOTICE has no '## Overview' section — run 'just licenses' and commit the regenerated file."

extract_overview "$NOTICE" >"$COMMITTED_VIEW"
extract_overview "$FRESH"   >"$FRESH_VIEW"

if [ ! -s "$COMMITTED_VIEW" ]; then
    die "the '## Overview' section of $NOTICE is empty — run 'just licenses' and commit the regenerated file."
fi

if diff -u "$COMMITTED_VIEW" "$FRESH_VIEW" >/dev/null; then
    echo "OK: the license SET matches the committed NOTICE ($(wc -l <"$COMMITTED_VIEW" | tr -d ' ') license entries)."
    echo "    (counts only — the rendered license text is not reproducible across cargo cache states,"
    echo "     so 'just licenses-check' stays the local byte-level sweep, not this gate.)"
    exit 0
fi

{
    echo "ERROR: the license SET has drifted from $NOTICE." >&2
    echo >&2
    echo "These licenses were added, removed, or changed count:" >&2
    # One awk pass over both views. Not `join`: its input must be sorted on the
    # JOIN FIELD under join's own collation, which does not have to agree with
    # the LC_ALL=C sort these views were built with — and when it disagrees,
    # join silently emits garbage pairs instead of failing (it did, here).
    awk '
        # A view line is `- <license name>: <count>`. Split on the LAST ": "
        # (not a plain -F": ", which would truncate a license name containing
        # one) and key on the name, not on the leading "- ".
        function entry(line,   ci) {
            sub(/^-[[:space:]]+/, "", line)
            for (ci = length(line) - 1; ci > 0; ci--)
                if (substr(line, ci, 2) == ": ")
                    return substr(line, 1, ci - 1) "\t" substr(line, ci + 2)
            return line "\t"
        }
        FILENAME == ARGV[1] { split(entry($0), p, "\t"); committed[p[1]] = p[2]; next }
                          { split(entry($0), p, "\t"); fresh[p[1]] = p[2] }
        END {
            for (lic in committed) {
                if (!(lic in fresh))  printf "  REMOVED  %s (was %s)\n", lic, committed[lic]
                else if (fresh[lic] != committed[lic])
                                     printf "  COUNT    %s: %s -> %s\n", lic, committed[lic], fresh[lic]
            }
            for (lic in fresh)
                if (!(lic in committed)) printf "  ADDED    %s (now %s)\n", lic, fresh[lic]
        }
    ' "$COMMITTED_VIEW" "$FRESH_VIEW" | LC_ALL=C sort >&2
    echo >&2
    echo "Unified diff of the normalised '## Overview' (committed vs fresh render):" >&2
    diff -u "$COMMITTED_VIEW" "$FRESH_VIEW" | sed -e '1,2d' -e 's/^/  /' >&2 || true
    echo >&2
    echo "Run 'just licenses' and commit the regenerated $NOTICE." >&2
    echo >&2
    echo "This gate compares the license SET (per-license counts), not the file" >&2
    echo "bytes: 'cargo about generate' embeds each license's full text and a" >&2
    echo "per-license used_by list whose ORDER depends on cargo cache state, so a" >&2
    echo "byte comparison fails on every cold runner no matter what is committed." >&2
    echo "If a count is correct but the rendered text is stale, 'just licenses-check'" >&2
    echo "is the local sweep that shows you the text drift." >&2
} >&2
exit 1
