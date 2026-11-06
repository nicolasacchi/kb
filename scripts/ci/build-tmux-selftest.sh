#!/usr/bin/env bash
# Self-test for scripts/ci/build-tmux.sh. No network, no sudo.
#
# (a) A bogus tarball via a file:// URL: the script must fail with
#     "refusing to extract" and must NOT have created an extraction
#     directory (a bad tarball is never extracted). KB_TMUX_WORK is set
#     so the test can look.
# (b) A corrupted pin (the TMUX_SHA256= line rewritten to "nothex"):
#     the script must fail with the "pin" message.
# (c) A hostile environment: TMUX_SHA256 exported with the exact sha of
#     the bogus tarball, pointing at the real script. The pin is
#     hard-coded (never environment-overridable), so it must still refuse.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_TMUX="${SCRIPT_DIR}/build-tmux.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

fail() { echo "BUILD-TMUX-SELFTEST FAIL: $*" >&2; exit 1; }

# One bogus tarball, reused by all three cases.
mkdir -p "${WORK}/src"
echo "not tmux" > "${WORK}/src/blob.txt"
tar czf "${WORK}/tmux-3.7c.tar.gz" -C "${WORK}/src" blob.txt
BOGUS_SHA="$(sha256sum "${WORK}/tmux-3.7c.tar.gz" | awk '{print $1}')"

mkdir -p "${WORK}/a-work"
if env KB_TMUX_URL="file://${WORK}/tmux-3.7c.tar.gz" \
     KB_TMUX_OUT="${WORK}/a-out" KB_TMUX_WORK="${WORK}/a-work" \
     bash "${BUILD_TMUX}" >"${WORK}/a-work/stdout.log" 2>"${WORK}/a-work/stderr.log"; then
  fail "case (a): bogus tarball built successfully"
fi
test ! -e "${WORK}/a-work/extract" || fail "case (a): extraction directory exists"
grep -F "refusing to extract" "${WORK}/a-work/stderr.log" >/dev/null \
  || fail "case (a): stderr lacks 'refusing to extract'"

# (b) corrupted pin line: fails closed on the "pin" message.
mkdir -p "${WORK}/b"
cp "${BUILD_TMUX}" "${WORK}/b/build-tmux.sh"
sed -i 's/^TMUX_SHA256=.*/TMUX_SHA256="nothex"/' "${WORK}/b/build-tmux.sh"
mkdir -p "${WORK}/b-work"
if env KB_TMUX_URL="file://${WORK}/tmux-3.7c.tar.gz" \
     KB_TMUX_OUT="${WORK}/b-out" KB_TMUX_WORK="${WORK}/b-work" \
     bash "${WORK}/b/build-tmux.sh" >"${WORK}/b-work/stdout.log" 2>"${WORK}/b-work/stderr.log"; then
  fail "case (b): corrupted pin built successfully"
fi
grep -F "pin" "${WORK}/b-work/stderr.log" >/dev/null \
  || fail "case (b): stderr lacks the pin message"
test ! -e "${WORK}/b-work/extract" || fail "case (b): extraction directory exists"

# (c) exported TMUX_SHA256 cannot move the pin: still refuses.
mkdir -p "${WORK}/c-work"
if env TMUX_SHA256="${BOGUS_SHA}" \
     KB_TMUX_URL="file://${WORK}/tmux-3.7c.tar.gz" \
     KB_TMUX_OUT="${WORK}/c-out" KB_TMUX_WORK="${WORK}/c-work" \
     bash "${BUILD_TMUX}" >"${WORK}/c-work/stdout.log" 2>"${WORK}/c-work/stderr.log"; then
  fail "case (c): environment sha built the bogus tarball"
fi
grep -F "refusing to extract" "${WORK}/c-work/stderr.log" >/dev/null \
  || fail "case (c): stderr lacks 'refusing to extract'"
test ! -e "${WORK}/c-work/extract" || fail "case (c): extraction directory exists"
 

say() { printf '%s\n' "$*"; }
say "build-tmux-selftest: (a) refusal before extraction, (b) corrupted pin, (c) hostile env — all pass"
