#!/usr/bin/env bash
# check-pinned-by.sh -- resolve "pinned by `name`" citations to a real definition.
#
# A comment or doc that says a property is "pinned by `some_test`" is a claim
# with a name attached, and test names rot exactly like line numbers do:
# crates/kb-code-server/src/lip.rs once said "pinned by
# `lip_registry_status_reports_dead_until_first_real_use`" long after the test
# was renamed, so the sentence pointed at nothing (v0.44 F1 / review I3).
#
# WHAT IS CHECKED
#   * `pinned by `X`` and `X` pins ...-style citations are NOT parsed -- only the
#     explicit, greppable form: the words "pinned by" immediately followed by a
#     backticked token, and any backticked `file.rs::test_name` token.
#   * X is resolved by SHAPE:
#       - a file name (ends .rs/.ts/.tsx/.toml/.sh/.py/.js): a tracked file with
#         that basename must exist (leading ../ and directories are ignored);
#       - `a::b::name` or bare `name` where name is snake_case with an
#         underscore: some tracked .rs file must define `fn name`;
#       - anything else (a module path like `kb_core::slo`, a crate name, a
#         marker like `// invariant:N`) is not a test name and is skipped.
#   A citation that does not resolve is a failure.
#
# SCOPE: docs/*.md, every tracked CLAUDE.md, and the comment/doc lines of
# tracked *.rs/*.ts/*.tsx under crates/, web/src, web-code/src, tests/.
# docs/research/** is a dated record and is never rewritten or gated.
#
# Usage: scripts/check-pinned-by.sh            (exit 1 on any unresolved citation)
#        scripts/check-pinned-by.sh --self-test
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

crate_names="$(ls crates 2>/dev/null | tr '-' '_')"
# The tracked-file list is written to a FILE and matched with `grep -E FILE
# >/dev/null`, never `git ls-files | grep -q`: under pipefail a quiet grep
# kills the writer with SIGPIPE and the pipeline reports "no match" (the
# class scripts/ci/code-changed.sh was written to remove).
tracked="$(mktemp)"
trap 'rm -f "$tracked"' EXIT
git ls-files > "$tracked"
has_file() { grep -E "(^|/)${1//./\\.}\$" "$tracked" >/dev/null; }

# $1 = a citation token; echoes "ok"/"skip"/"missing:<why>"
resolve() {
  local tok="$1" name base
  tok="${tok#../}"
  case "$tok" in
    *.rs::*) name="${tok##*::}" ;;
    *::*) name="${tok##*::}" ;;
    *) name="$tok" ;;
  esac
  case "$tok" in
    *.rs::*)
      base="${tok%%::*}"; base="${base##*/}"
      if ! has_file "$base"; then echo "missing:no tracked file named $base"; return; fi
      # file.rs::name -- the NAME must be a fn defined in a file of that basename
      # (types, consts and nested paths -- anything not a lowercase identifier
      # -- are skipped: only a function name is a test/guard citation).
      if [[ "$name" =~ ^[a-z][a-z0-9_]*$ ]]; then
        local f hit=0
        while IFS= read -r f; do
          if grep -E "fn[[:space:]]+${name}([^A-Za-z0-9_]|\$)" "$f" >/dev/null; then hit=1; break; fi
        done < <(grep -E "(^|/)${base//./\\.}\$" "$tracked")
        if [ "$hit" -eq 1 ]; then echo ok; else echo "missing:no 'fn $name' in any tracked file named $base"; fi
        return
      fi
      ;;
  esac
  case "$name" in
    *.rs|*.ts|*.tsx|*.toml|*.sh|*.py|*.js|*.cjs|*.mjs|*.yml)
      base="${name##*/}"
      if has_file "$base"; then echo ok; else echo "missing:no tracked file named $base"; fi
      return ;;
  esac
  if ! [[ "$name" =~ ^[a-z][a-z0-9]*(_[a-z0-9]+)+$ ]]; then echo skip; return; fi
  if printf '%s\n' "$crate_names" | grep -qx "$name"; then echo skip; return; fi
  if git grep -qE "fn[[:space:]]+${name}([^A-Za-z0-9_]|\$)" -- '*.rs'; then echo ok; else echo "missing:no 'fn $name' in any tracked .rs file"; fi
}

scan() {
  local bad=0 total=0 file line tok res
  # file:line:text for every candidate line
  while IFS=: read -r file line text; do
    while IFS= read -r tok; do
      [ -z "$tok" ] && continue
      tok="${tok#\`}"; tok="${tok%\`}"
      res="$(resolve "$tok")"
      case "$res" in
        ok) total=$((total + 1)) ;;
        skip) ;;
        missing:*) total=$((total + 1)); bad=$((bad + 1)); echo "UNRESOLVED  $file:$line  \`$tok\`  (${res#missing:})" ;;
      esac
    done < <(printf '%s\n' "$text" | grep -oE 'pinned by `[^`]+`|`[A-Za-z0-9_./-]+\.rs::[a-z0-9_:]+`' | sed -E 's/^pinned by //')
  done < <(
    { git grep -nE 'pinned by `|`[A-Za-z0-9_./-]+\.rs::[a-z0-9_:]+`' -- 'docs/*.md' 'CLAUDE.md' '*/CLAUDE.md' ':!docs/research' ;
      git grep -nE '^[[:space:]]*(//|\*|#).*(pinned by `|`[A-Za-z0-9_./-]+\.rs::[a-z0-9_:]+`)' -- 'crates' 'web/src' 'web-code/src' 'tests' ':!docs/research' ; } 2>/dev/null
  )
  echo "pinned-by gate: $total citation(s) checked, $bad unresolved"
  [ "$bad" -eq 0 ]
}

if [ "${1:-}" = "--self-test" ]; then
  fail=0
  t() { [ "$(resolve "$1")" = "$2" ] || { echo "SELFTEST FAIL: resolve($1) = $(resolve "$1"), want $2" >&2; fail=1; }; }
  t "this_test_name_does_not_exist_anywhere" "missing:no 'fn this_test_name_does_not_exist_anywhere' in any tracked .rs file"
  t "kb_core" skip
  t "kb_core::slo" skip
  t "rust-toolchain.toml" ok
  t "nonexistent-file-xyz.ts" "missing:no tracked file named nonexistent-file-xyz.ts"
  t "resolve" skip
  # a real, stable definition in this very tree
  t "parser::tests::task_counts_match_source_scan" ok
  [ "$fail" -eq 0 ] && echo "check-pinned-by self-test ok"
  exit "$fail"
fi
scan
