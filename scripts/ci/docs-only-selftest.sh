#!/usr/bin/env bash
# Self-test for scripts/ci/docs-only.sh. Compile-free; run by `just
# ci-selfcheck` (supply-chain job).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
sut="$here/docs-only.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail=0

expect() { # expect <run=true|run=false> <label>; the names arrive on stdin
  local want="$1" label="$2" got
  cat > "$work/names"
  got="$(GITHUB_OUTPUT= "$sut" --names-file "$work/names")"
  if [ "$got" != "$want" ]; then echo "SELFTEST FAIL: $label: want $want, got $got" >&2; fail=1
  else echo "ok: $label ($got)"; fi
}

# 1. every allowlisted path is docs-only on its own; a near miss is not.
while IFS= read -r p; do
  case "$p" in
    */) printf '%sx.md\n' "$p" | expect run=false "prefix $p" ;;
    *)  printf '%s\n' "$p" | expect run=false "exact $p"
        printf '%s.bak\n' "$p" | expect run=true "near miss ${p}.bak"
        printf 'sub/%s\n' "$p" | expect run=true "nested ${p} is not the root file" ;;
  esac
done < <(sed -n '/^\(PREFIXES\|EXACT\)=(/,/^)/p' "$sut" | grep -o '"[^"]*"' | tr -d '"')

# 2. ONE non-docs path in a docs list flips it (the conservative direction).
printf 'docs/research/a.html\ncrates/kb-core/src/lib.rs\n' | expect run=true "mixed docs + code"
printf 'docs/research/a.html\nscripts/ci/docs-only.sh\n' | expect run=true "docs + the script itself"
printf 'docs/research/a.html\npackaging/release-notes/v1.md\n' | expect run=false "two docs-only paths"
printf 'docs/research/a.html\nREADME.md' | expect run=true "unknown path as the unterminated last line"

# 3. empty list -> run (never silently skip).
: | expect run=true "empty list"

# 4. paths a job or test CONSUMES must never be docs-only.
for p in README.md CLAUDE.md AGENTS.md THIRD-PARTY-LICENSES.md crates/kb-core/CLAUDE.md web-code/CLAUDE.md \
         docs/README.md docs/architecture-invariants.md docs/configuration.md docs/cli.md docs/api-routes.md \
         docs/http-api.md docs/public-mirror.md docs/web-ui.md docs/kb-code.md docs/protocols.md \
         plugins/kb-memory/skills/kb-verify/SKILL.md plugins/kb-code/README.md .github/workflows/ci.yml \
         justfile scripts/ci/docs-only.sh; do
  printf '%s\n' "$p" | expect run=true "consumed path $p"
done
# ...and none of the top-level docs/*.md is allowlisted, whatever is added later.
for f in "$root"/docs/*.md; do
  printf 'docs/%s\n' "$(basename "$f")" | expect run=true "top-level $(basename "$f")"
done

# 5. a 200 KB list with a code path last -> run; all docs-only -> skip.
{ echo "docs/research/a.html"; for i in $(seq 1 4000); do printf 'docs/research/padding-%06d-aaaaaaaaaaaaaaaaaaaaaaaaaaaa.md\n' "$i"; done; echo "crates/kb-core/src/lib.rs"; } | expect run=true "200 KB list, code path last"
{ for i in $(seq 1 4000); do printf 'docs/research/padding-%06d-aaaaaaaaaaaaaaaaaaaaaaaaaaaa.md\n' "$i"; done; } | expect run=false "200 KB list, all docs-only"

# 6. events and bases: only a pull_request with a resolvable base can skip.
mkdir "$work/r"
cd "$work/r"
git init -q .
git config user.email t@t
git config user.name t
mkdir -p docs/research crates
echo a > docs/research/a.html
echo x > crates/x.rs
git add -A
git commit -qm base
base="$(git rev-parse HEAD)"
echo b >> docs/research/a.html
git commit -qam docs
docs_head="$(git rev-parse HEAD)"
echo y >> crates/x.rs
git commit -qam code
code_head="$(git rev-parse HEAD)"
run() { GITHUB_OUTPUT= EVENT_NAME="$1" BASE_SHA="$2" HEAD_SHA="$3" "$sut"; }
chk() { if [ "$2" = "$1" ]; then echo "ok: $3 ($2)"; else echo "SELFTEST FAIL: $3: want $1, got $2" >&2; fail=1; fi; }
chk run=false "$(run pull_request "$base" "$docs_head")" "PR, docs-only range"
chk run=true  "$(run pull_request "$base" "$code_head")" "PR, mixed range"
chk run=true  "$(run push "$base" "$docs_head")" "push never skips"
chk run=true  "$(run workflow_dispatch "$base" "$docs_head")" "dispatch never skips"
chk run=true  "$(run "" "$base" "$docs_head")" "unknown event never skips"
chk run=true  "$(run pull_request "" "$docs_head")" "empty base"
chk run=true  "$(run pull_request deadbeefdeadbeefdeadbeefdeadbeefdeadbeef "$docs_head")" "unresolvable base"
chk run=true  "$(run pull_request "$base" "$base")" "empty diff"

# 7. the skip message names every allowlisted path.
msg="$("$sut" --skip-message)"
while IFS= read -r p; do
  case "$msg" in *"$p"*) ;; *) echo "SELFTEST FAIL: skip message omits $p" >&2; fail=1 ;; esac
done < <(sed -n '/^\(PREFIXES\|EXACT\)=(/,/^)/p' "$sut" | grep -o '"[^"]*"' | tr -d '"')

exit "$fail"
