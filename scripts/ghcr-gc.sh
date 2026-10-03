#!/usr/bin/env bash
# ghcr garbage collection for ONE container package.
#
# Deletes a version ONLY when it carries at least one tag and EVERY tag on it
# matches ^main-  (the per-commit build-image.yml tags). A version carrying a
# semver tag, `latest`, or ANY non-main-* tag next to a main-* tag is never
# selected; untagged versions (multi-arch child manifests) are never selected
# either, because deleting one corrupts the index that points at it.
#
# The newest KEEP qualifying versions (by created_at) are kept.
#
# Environment:
#   PACKAGE   container package name (kb | kb-code)            [required]
#   OWNER     package owner (user)                             [required unless SELECT_ONLY]
#   KEEP      how many newest main-* versions to keep          [default 10]
#   DRY_RUN   "true" = list only, delete nothing               [default true]
#   VERSIONS_FILE  read the versions JSON array from this file instead of the
#             API (selftest); implies nothing is ever deleted.
set -euo pipefail

KEEP="${KEEP:-10}"
DRY_RUN="${DRY_RUN:-true}"

# select_versions < versions.json  ->  one "<id>\t<tags>" line per deletable version
select_versions() {
  jq -r --argjson keep "$KEEP" '
    [ .[]
      | { id: .id, created: .created_at, tags: (.metadata.container.tags // []) }
      | select((.tags | length) > 0)
      | select(.tags | all(test("^main-")))
    ]
    | sort_by(.created) | reverse
    | .[$keep:]
    | .[]
    | "\(.id)\t\(.tags | join(","))"
  '
}

if [ "${1:-}" = "--select-only" ]; then
  select_versions
  exit 0
fi

: "${PACKAGE:?PACKAGE is required}"

if [ -n "${VERSIONS_FILE:-}" ]; then
  DRY_RUN=true
  versions="$(cat "$VERSIONS_FILE")"
else
  : "${OWNER:?OWNER is required}"
  versions="$(gh api --paginate "/users/${OWNER}/packages/container/${PACKAGE}/versions?per_page=100" \
    | jq -s 'add // []')"
fi

total="$(jq 'length' <<<"$versions")"
protected="$(jq '[ .[] | (.metadata.container.tags // []) | select(length > 0)
                   | select(any(.[]; test("^main-") | not)) ] | length' <<<"$versions")"
doomed="$(select_versions <<<"$versions")"
n="$(printf '%s' "$doomed" | grep -c . || true)"

echo "package=${PACKAGE} versions=${total} protected(non-main tag)=${protected} to_delete=${n} keep=${KEEP} dry_run=${DRY_RUN}"
[ -z "$doomed" ] || printf '%s\n' "$doomed" | while IFS=$'\t' read -r id tags; do
  echo "  candidate ${id} tags=${tags}"
done

if [ "$DRY_RUN" = "true" ]; then
  echo "dry run: nothing deleted"
  exit 0
fi

[ "$n" -gt 0 ] || { echo "nothing to delete"; exit 0; }
printf '%s\n' "$doomed" | while IFS=$'\t' read -r id tags; do
  echo "deleting ${id} (${tags})"
  gh api -X DELETE "/users/${OWNER}/packages/container/${PACKAGE}/versions/${id}"
done
