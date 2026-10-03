#!/usr/bin/env bash
# Asserts every documented distribution channel actually resolves, ANONYMOUSLY:
#   * ghcr.io/<owner>/kb and ghcr.io/<owner>/kb-code manifests for :latest and
#     :<version>   (a private package or a pruned tag both answer 401/404)
#   * the GitHub "latest release" lookup scripts/install.sh performs
#
# The Docker channel was dead for weeks (kb:latest 404) while every workflow
# was green; nothing probed the channel from a consumer's side.
#
# Environment:
#   REPO     owner/name, e.g. nicolasacchi/kb            [required]
#   VERSION  version WITHOUT the leading v; default = tag of the latest release
#   GH_TOKEN optional; only used for the releases API call (rate limits)
set -euo pipefail
: "${REPO:?REPO is required}"
owner="${REPO%%/*}"; owner="$(printf '%s' "$owner" | tr '[:upper:]' '[:lower:]')"
fail=0

api_hdr=()
[ -z "${GH_TOKEN:-}" ] || api_hdr=(-H "Authorization: Bearer ${GH_TOKEN}")

rel_json="$(curl -fsSL "${api_hdr[@]}" "https://api.github.com/repos/${REPO}/releases/latest" || true)"
latest_tag="$(printf '%s' "$rel_json" | grep -m1 '"tag_name"' | sed -e 's/.*"tag_name"[^"]*"//' -e 's/".*//' || true)"
if [ -z "$latest_tag" ]; then
  echo "FAIL: install.sh's latest-release lookup returns no tag" >&2; fail=1
else
  echo "ok: latest release is ${latest_tag}"
fi
VERSION="${VERSION:-${latest_tag#v}}"
[ -n "$VERSION" ] || { echo "FAIL: no version to probe" >&2; exit 1; }

for pkg in kb kb-code; do
  # Anonymous pull token (no Authorization header on this request).
  tok="$(curl -fsSL "https://ghcr.io/token?service=ghcr.io&scope=repository:${owner}/${pkg}:pull" \
         | python3 -c 'import json,sys; print(json.load(sys.stdin).get("token",""))' || true)"
  for tag in latest "$VERSION"; do
    code="$(curl -s -o /dev/null -w '%{http_code}' -I \
      -H "Authorization: Bearer ${tok}" \
      -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json' \
      "https://ghcr.io/v2/${owner}/${pkg}/manifests/${tag}")"
    if [ "$code" = "200" ]; then
      echo "ok: ghcr.io/${owner}/${pkg}:${tag}"
    else
      echo "FAIL: ghcr.io/${owner}/${pkg}:${tag} -> HTTP ${code}" >&2; fail=1
    fi
  done
done
exit "$fail"
