#!/usr/bin/env bash
# Self-test for scripts/ci/build-tmux.sh. No network, no sudo.
#
# (a) A bogus tarball via a file:// URL: the script must fail with
#     "refusing to extract" and must NOT have created an extraction
#     directory (a bad tarball is never extracted). KB_TMUX_WORK is set
#     so the test can look.
# (b) A corrupted pin (the TMUX_SHA256= line rewritten to "nothex"):
#     the script must fail with the "pin" message.
# (b2) A 64-char non-hex pin ("zz"x32): pins the lowercase-hex guard
#     specifically — with only the length check it would pass.
# (c) A hostile environment: TMUX_SHA256 exported with the exact sha of
#     the bogus tarball, pointing at the real script. The pin is
#     hard-coded (never environment-overridable), so it must still refuse.
# (d) A dynamic cache hit installs the runtime libraries (fake sudo on
#     PATH, CI=true), prints "cached" and exports KB_TMUX_BIN.
# (e) A static cache hit does NOT invoke the dynamic runtime install.
# (f) A cache with a missing or bogus LINKAGE is not trusted: the script
#     warns naming the value and rebuilds (with the bogus tarball it
#     fails at the sha gate) instead of exporting the cached binary.
# (g) The static->dynamic retry: a fake tarball whose configure refuses
#     --enable-static. The build must distclean, wipe the output dir and
#     rebuild dynamically, leaving LINKAGE=dynamic.
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

# (b2) 64-char non-hex pin: only the lowercase-hex guard can reject it.
mkdir -p "${WORK}/b2"
cp "${BUILD_TMUX}" "${WORK}/b2/build-tmux.sh"
printf -v BAD_PIN '%64s' ''
BAD_PIN="${BAD_PIN// /z}"
sed -i "s/^TMUX_SHA256=.*/TMUX_SHA256=\"${BAD_PIN}\"/" "${WORK}/b2/build-tmux.sh"
mkdir -p "${WORK}/b2-work"
if env KB_TMUX_URL="file://${WORK}/tmux-3.7c.tar.gz" \
    KB_TMUX_OUT="${WORK}/b2-out" KB_TMUX_WORK="${WORK}/b2-work" \
    bash "${WORK}/b2/build-tmux.sh" >"${WORK}/b2-work/stdout.log" 2>"${WORK}/b2-work/stderr.log"; then
  fail "case (b2): 64-char non-hex pin built successfully"
fi
grep -F "not lowercase hex" "${WORK}/b2-work/stderr.log" >/dev/null \
  || fail "case (b2): stderr lacks the lowercase-hex pin message"
test ! -e "${WORK}/b2-work/extract" || fail "case (b2): extraction directory exists"

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

# (d) dynamic cache hit: fake sudo records the runtime-library install.
mkdir -p "${WORK}/d-out/bin" "${WORK}/d-stubbin"
printf '#!/bin/sh\necho "tmux 3.7c"\n' > "${WORK}/d-out/bin/tmux"
chmod +x "${WORK}/d-out/bin/tmux"
printf 'dynamic\n' > "${WORK}/d-out/LINKAGE"
printf '#!/bin/sh\necho "sudo $*" >> "%s"\nexit 0\n' "${WORK}/d-sudo.log" > "${WORK}/d-stubbin/sudo"
chmod +x "${WORK}/d-stubbin/sudo"
: > "${WORK}/d-sudo.log"
: > "${WORK}/d-env"
if ! env CI=true PATH="${WORK}/d-stubbin:${PATH}" \
    GITHUB_ENV="${WORK}/d-env" KB_TMUX_OUT="${WORK}/d-out" \
    bash "${BUILD_TMUX}" >"${WORK}/d-stdout.log" 2>"${WORK}/d-stderr.log"; then
  fail "case (d): dynamic cache hit failed"
fi
grep -F "cached" "${WORK}/d-stdout.log" >/dev/null \
  || fail "case (d): stdout lacks 'cached'"
grep -F "KB_TMUX_BIN=${WORK}/d-out/bin/tmux" "${WORK}/d-env" >/dev/null \
  || fail "case (d): GITHUB_ENV lacks the KB_TMUX_BIN export"
grep -F "libevent-core-2.1-7t64" "${WORK}/d-sudo.log" >/dev/null \
  || fail "case (d): dynamic cache hit did not install the runtime libraries"
grep -F "libutf8proc3" "${WORK}/d-sudo.log" >/dev/null \
  || fail "case (d): runtime install missed libutf8proc3"
grep -F "libncurses6" "${WORK}/d-sudo.log" >/dev/null \
  || fail "case (d): runtime install missed libncurses6"

# (e) static cache hit: no dynamic runtime install must be attempted.
mkdir -p "${WORK}/e-out/bin" "${WORK}/e-stubbin"
printf '#!/bin/sh\necho "tmux 3.7c"\n' > "${WORK}/e-out/bin/tmux"
chmod +x "${WORK}/e-out/bin/tmux"
printf 'static\n' > "${WORK}/e-out/LINKAGE"
printf '#!/bin/sh\necho "sudo $*" >> "%s"\nexit 0\n' "${WORK}/e-sudo.log" > "${WORK}/e-stubbin/sudo"
chmod +x "${WORK}/e-stubbin/sudo"
: > "${WORK}/e-sudo.log"
: > "${WORK}/e-env"
if ! env CI=true PATH="${WORK}/e-stubbin:${PATH}" \
    GITHUB_ENV="${WORK}/e-env" KB_TMUX_OUT="${WORK}/e-out" \
    bash "${BUILD_TMUX}" >"${WORK}/e-stdout.log" 2>"${WORK}/e-stderr.log"; then
  fail "case (e): static cache hit failed"
fi
grep -F "cached" "${WORK}/e-stdout.log" >/dev/null \
  || fail "case (e): stdout lacks 'cached'"
test ! -s "${WORK}/e-sudo.log" \
  || fail "case (e): static cache hit attempted the dynamic runtime install"

# (f) missing/bogus LINKAGE: warn naming the value and rebuild (fail at
# the sha gate on the bogus tarball), never export the cached binary.
mkdir -p "${WORK}/f-out/bin"
printf '#!/bin/sh\necho "tmux 3.7c"\n' > "${WORK}/f-out/bin/tmux"
chmod +x "${WORK}/f-out/bin/tmux"
: > "${WORK}/f-env"
if env KB_TMUX_URL="file://${WORK}/tmux-3.7c.tar.gz" \
    KB_TMUX_OUT="${WORK}/f-out" KB_TMUX_WORK="${WORK}/f-work" \
    GITHUB_ENV="${WORK}/f-env" \
    bash "${BUILD_TMUX}" >"${WORK}/f-stdout.log" 2>"${WORK}/f-stderr.log"; then
  fail "case (f): missing LINKAGE was treated as a cache hit"
fi
grep -F "unexpected LINKAGE" "${WORK}/f-stdout.log" "${WORK}/f-stderr.log" >/dev/null \
  || fail "case (f): no unexpected-LINKAGE warning"
grep -F "unknown" "${WORK}/f-stdout.log" "${WORK}/f-stderr.log" >/dev/null \
  || fail "case (f): warning does not name the missing value"
test ! -e "${WORK}/f-work/extract" || fail "case (f): extraction directory exists"
if grep -F "KB_TMUX_BIN" "${WORK}/f-env" >/dev/null; then
  fail "case (f): cached binary was exported despite bad LINKAGE"
fi
printf 'bogus\n' > "${WORK}/f-out/LINKAGE"
: > "${WORK}/f-env"
if env KB_TMUX_URL="file://${WORK}/tmux-3.7c.tar.gz" \
    KB_TMUX_OUT="${WORK}/f-out" KB_TMUX_WORK="${WORK}/f-work" \
    GITHUB_ENV="${WORK}/f-env" \
    bash "${BUILD_TMUX}" >"${WORK}/f2-stdout.log" 2>"${WORK}/f2-stderr.log"; then
  fail "case (f): bogus LINKAGE was treated as a cache hit"
fi
grep -F "bogus" "${WORK}/f2-stdout.log" "${WORK}/f2-stderr.log" >/dev/null \
  || fail "case (f): warning does not name the bogus value"
if grep -F "KB_TMUX_BIN" "${WORK}/f-env" >/dev/null; then
  fail "case (f): cached binary was exported despite bogus LINKAGE"
fi

# (g) static->dynamic retry on a fake tarball. The truncated copy
# (last `main "$@"` line removed) defines functions but runs nothing,
# so build_tarball can be driven directly with install_build_deps
# stubbed to a no-op (no apt even under CI=true).
G_SRC="${WORK}/g-src/tmux-3.7c"
mkdir -p "${G_SRC}"
cat > "${G_SRC}/configure" <<'EOF'
#!/bin/sh
echo "configure $*" >> "${CONFIGURE_LOG}"
prefix=""
for a in "$@"; do
  case "$a" in --prefix=*) prefix="${a#--prefix=}" ;; esac
done
mkdir -p "${prefix}"
static=""
for a in "$@"; do
  case "$a" in --enable-static) static="1" ;; esac
done
if [ -n "${static}" ]; then
  touch "${prefix}/static-marker"
  exit 1
fi
mkdir -p "${prefix}/bin"
printf '#!/bin/sh\necho "tmux 3.7c"\n' > "${prefix}/bin/tmux"
chmod +x "${prefix}/bin/tmux"
exit 0
EOF
chmod +x "${G_SRC}/configure"
printf 'all:\n\t@true\ninstall:\n\t@true\ndistclean:\n\t@touch ./distclean-ran\n' > "${G_SRC}/Makefile"
tar czf "${WORK}/g-tarball.tar.gz" -C "${WORK}/g-src" tmux-3.7c
G_SHA="$(sha256sum "${WORK}/g-tarball.tar.gz" | awk '{print $1}')"
mkdir -p "${WORK}/g"
cp "${BUILD_TMUX}" "${WORK}/g/tool.sh"
sed -i '/^main "\$@"$/d' "${WORK}/g/tool.sh"
sed -i "s/^TMUX_SHA256=.*/TMUX_SHA256=\"${G_SHA}\"/" "${WORK}/g/tool.sh"
export CONFIGURE_LOG="${WORK}/g/configure.log"
: > "${CONFIGURE_LOG}"
if ! (
  export KB_TMUX_OUT="${WORK}/g-out"
  # shellcheck disable=SC1091
  . "${WORK}/g/tool.sh"
  install_build_deps() { :; }
  mkdir -p "${WORK}/g-work"
  build_tarball "${WORK}/g-tarball.tar.gz" "${WORK}/g-work"
) >"${WORK}/g-stdout.log" 2>"${WORK}/g-stderr.log"; then
  fail "case (g): static->dynamic retry did not produce a binary"
fi
test -x "${WORK}/g-out/bin/tmux" || fail "case (g): no binary at the output dir"
test "$(cat "${WORK}/g-out/LINKAGE")" = "dynamic" \
  || fail "case (g): LINKAGE is not dynamic after the retry"
grep -F "linkage=dynamic" "${WORK}/g-out/BUILD-INFO" >/dev/null \
  || fail "case (g): BUILD-INFO lacks linkage=dynamic"
grep -F -- "--- ldd ---" "${WORK}/g-out/BUILD-INFO" >/dev/null \
  || fail "case (g): BUILD-INFO lacks the ldd section for a dynamic build"
test ! -e "${WORK}/g-out/static-marker" \
  || fail "case (g): output dir was not wiped between attempts"
test -f "${WORK}/g-work/extract/tmux-3.7c/distclean-ran" \
  || fail "case (g): make distclean did not run before the retry"
test "$(grep -c "^configure " "${CONFIGURE_LOG}")" = "2" \
  || fail "case (g): configure did not run exactly twice"

say() { printf '%s\n' "$*"; }
say "build-tmux-selftest: all cases pass"
