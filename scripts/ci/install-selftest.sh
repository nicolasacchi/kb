#!/usr/bin/env bash
# Drives scripts/install.sh against a local file:// mirror (KB_BASE_URL) with a
# fake archive. No network, no real binaries. Proves the installer fails
# closed: a missing checksum sidecar, a mismatching checksum, a failing
# `gh attestation verify` and a missing sha tool all abort before anything is
# installed, KB_INSECURE_SKIP_VERIFY=1 is the only override, and the share/
# tree (web reader + sample corpus) is installed next to bin/.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
install_sh="$here/../install.sh"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT

case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;; aarch64|arm64) arch=aarch64 ;;
  *) echo "unsupported test host" >&2; exit 1 ;;
esac
target="${arch}-unknown-linux-gnu"
ver="9.9.9"
pkg="kb-${ver}-${target}"

mkdir -p "$work/mirror" "$work/src/$pkg/share/kb/web/dist" "$work/src/$pkg/share/kb/sample-corpus"
printf '#!/bin/sh\necho "kb 9.9.9-selftest"\n' > "$work/src/$pkg/kb"
printf '#!/bin/sh\nexit 0\n' > "$work/src/$pkg/kb-embedder"
chmod +x "$work/src/$pkg/kb" "$work/src/$pkg/kb-embedder"
echo '<html></html>' > "$work/src/$pkg/share/kb/web/dist/index.html"
echo '<html></html>' > "$work/src/$pkg/share/kb/sample-corpus/a.html"
tar -czf "$work/mirror/$pkg.tar.gz" -C "$work/src" "$pkg"
good="$(sha256sum "$work/mirror/$pkg.tar.gz" | awk '{print $1}')"

# fake gh: `auth status` ok; `attestation verify` result from FAKE_GH_VERIFY.
mkdir -p "$work/gh-bin"
cat > "$work/gh-bin/gh" <<'GH'
#!/bin/sh
case "$1 $2" in
  "auth status") exit 0 ;;
  "attestation verify") echo "fake gh: verify rc=${FAKE_GH_VERIFY:-0}"; exit "${FAKE_GH_VERIFY:-0}" ;;
esac
exit 0
GH
chmod +x "$work/gh-bin/gh"

run() { # run <prefix> [env...] ; returns installer's exit code, output in $work/out
  prefix="$1"; shift
  env -i HOME="$work/home" PATH="$PATH" PREFIX="$prefix" \
      KB_BASE_URL="file://$work/mirror" KB_VERSION="$ver" "$@" \
      sh "$install_sh" >"$work/out" 2>&1
}
fail() { echo "FAIL: $*" >&2; sed 's/^/  | /' "$work/out" >&2; exit 1; }

# 1. no sidecar -> hard error, nothing installed
rm -f "$work/mirror/$pkg.tar.gz.sha256"
if run "$work/p1"; then fail "missing sidecar must abort"; fi
[ ! -e "$work/p1/bin/kb" ] || fail "binary installed despite missing sidecar"
grep -q "sidecar unavailable" "$work/out" || fail "no sidecar message"

# 2. no sidecar + escape hatch -> installs, shares installed
run "$work/p2" KB_INSECURE_SKIP_VERIFY=1 || fail "KB_INSECURE_SKIP_VERIFY=1 should proceed"
[ -x "$work/p2/bin/kb" ] || fail "kb not installed with override"
[ -f "$work/p2/share/kb/web/dist/index.html" ] || fail "share/kb/web/dist not installed"
[ -f "$work/p2/share/kb/sample-corpus/a.html" ] || fail "sample corpus not installed"

# 3. mismatching checksum aborts even with the escape hatch
echo "0000000000000000000000000000000000000000000000000000000000000000  $pkg.tar.gz" > "$work/mirror/$pkg.tar.gz.sha256"
if run "$work/p3" KB_INSECURE_SKIP_VERIFY=1; then fail "checksum mismatch must abort"; fi
grep -q "checksum mismatch" "$work/out" || fail "no mismatch message"

# 4. good sidecar installs
echo "$good  $pkg.tar.gz" > "$work/mirror/$pkg.tar.gz.sha256"
run "$work/p4" || fail "good checksum should install"
grep -q "checksum OK" "$work/out" || fail "no checksum OK"

# 5. gh present + attestation verify fails -> abort; override proceeds
if run "$work/p5" PATH="$work/gh-bin:$PATH" FAKE_GH_VERIFY=1; then fail "failed attestation must abort"; fi
[ ! -e "$work/p5/bin/kb" ] || fail "binary installed despite failed attestation"
run "$work/p6" PATH="$work/gh-bin:$PATH" FAKE_GH_VERIFY=1 KB_INSECURE_SKIP_VERIFY=1 || fail "override should proceed"
# 6. gh present + attestation verify passes -> installs and says so
run "$work/p7" PATH="$work/gh-bin:$PATH" FAKE_GH_VERIFY=0 || fail "good attestation should install"
grep -q "provenance OK" "$work/out" || fail "no provenance OK"

# 7. no sha256 tool at all -> hard error. A PATH of symlinks to exactly the
# tools install.sh needs, minus sha256sum/shasum.
mkdir -p "$work/min-bin"
for t in sh curl uname mktemp tar find head awk sed grep cp mkdir rm chmod basename cat dirname tr; do
  p="$(command -v "$t" || true)"; [ -z "$p" ] || ln -sf "$p" "$work/min-bin/$t"
done
if run "$work/p8" PATH="$work/min-bin"; then fail "missing sha tool must abort"; fi
grep -q "no sha256 tool" "$work/out" || fail "no sha-tool message"
echo "install selftest OK"
