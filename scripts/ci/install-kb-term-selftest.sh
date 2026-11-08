#!/usr/bin/env bash
# Drives scripts/install-kb-term.sh against a local file:// mirror
# (KB_BASE_URL) with a fake archive. No network, no real binaries (stub
# scripts plus copies of the host's own system ELFs in case 11). Proves the
# installer fails closed: a missing checksum sidecar, a mismatching checksum,
# a failing `gh attestation verify`, a missing sha tool, a wrong tmux version
# and a missing archive member all abort before anything is installed;
# KB_INSECURE_SKIP_VERIFY=1 is the only override; the pinned tmux lands in
# libexec (never bin/), never on PATH; unit/tmpfiles tokens are rendered.
#
  # This file owns the expected archive manifest. The `term-int` job
  # (ci.yml) and every case here compare the archive listing against the
  # list printed by `--print-manifest`: one source of truth.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
install_sh="$here/../install-kb-term.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"; if [ -n "${running_pid:-}" ]; then kill "$running_pid" 2>/dev/null || true; fi' EXIT

# ---- expected manifest (single source of truth) ------------------------------
# Prints the full tar member names for the archive `kb-term-<VERSION>-<TARGET>`
# (defaults: 9.9.9 / x86_64-unknown-linux-gnu), one per line.
print_manifest() {
  ver="${1:-9.9.9}"
  target="${2:-x86_64-unknown-linux-gnu}"
  root="kb-term-${ver}-${target}"
  printf '%s\n' \
    "${root}/bin/kb-termd" \
    "${root}/bin/kb-term" \
    "${root}/bin/kb-tap" \
    "${root}/libexec/kb-term/tmux" \
    "${root}/libexec/kb-term/tmux.sha256" \
    "${root}/share/kb-term/systemd/kb-termd.service" \
    "${root}/share/kb-term/systemd/kb-tap@.service" \
    "${root}/share/kb-term/systemd/kb-term@.service" \
    "${root}/share/kb-term/tmpfiles.d/kb-term.conf" \
    "${root}/share/kb-term/omp/overlay.yml" \
    "${root}/share/kb-term/kb-term.conf" \
    "${root}/share/kb-term/VERSIONS" \
    "${root}/INSTALL"
}

if [ "${1:-}" = "--print-manifest" ]; then
  print_manifest "${2:-9.9.9}" "${3:-x86_64-unknown-linux-gnu}"
  exit 0
fi

# ---- fake archive builder -----------------------------------------------------
# Builds a fake archive shaped exactly like the real one: stub binaries, a
# fake tmux, service/tmpfiles stubs carrying the renderer tokens, an INSTALL
# note and VERSIONS. Args:
#   $1 = tmux version line ("tmux 3.7c" / "tmux 3.4")
#   $2 = include-kb-tap (1 / 0)
#   $3 = kb-termd source ("stub" | "sleep-elf")
#   $4 = kb-termd-2 source for archive 2 | none
#   $5 = unit extra line ("" / "second")
ver="9.9.9"
target="x86_64-unknown-linux-gnu"
pkg="kb-term-${ver}-${target}"

build_fake_archive() {
  # $1 dir   $2 tmux line   $3 include-tap   $4 termd src   $5 unit marker
  dir="$1"; tmux_line="$2"; include_tap="$3"; termd_src="$4"; unit_marker="$5"
  rm -rf "$dir"
  mkdir -p "$dir/$pkg/bin" "$dir/$pkg/libexec/kb-term" \
           "$dir/$pkg/share/kb-term/systemd" "$dir/$pkg/share/kb-term/tmpfiles.d" \
           "$dir/$pkg/share/kb-term/omp"
  if [ "$termd_src" = "sleep-elf" ]; then
    cp "$(type -P sleep)" "$dir/$pkg/bin/kb-termd"
  elif [ "$termd_src" = "true-plus" ]; then
    cp "$(type -P true)" "$dir/$pkg/bin/kb-termd"
    printf 'x' >> "$dir/$pkg/bin/kb-termd"
  else
    printf '#!/bin/sh\nexit 0\n' > "$dir/$pkg/bin/kb-termd"
  fi
  chmod +x "$dir/$pkg/bin/kb-termd"
  printf '#!/bin/sh\nexit 0\n' > "$dir/$pkg/bin/kb-term"
  chmod +x "$dir/$pkg/bin/kb-term"
  if [ "$include_tap" = "1" ]; then
    printf '#!/bin/sh\nexit 0\n' > "$dir/$pkg/bin/kb-tap"
    chmod +x "$dir/$pkg/bin/kb-tap"
  fi
  printf '#!/bin/sh\necho "%s"\n' "$tmux_line" > "$dir/$pkg/libexec/kb-term/tmux"
  chmod +x "$dir/$pkg/libexec/kb-term/tmux"
  ( cd "$dir/$pkg/libexec/kb-term" && sha256sum tmux > tmux.sha256 )
  for u in kb-termd.service kb-tap@.service kb-term@.service; do
    printf "[Unit]\nDescription=kb-term ${u}\nExecStart=@PREFIX@/bin/kb-term --${unit_marker}\n" \
      > "$dir/$pkg/share/kb-term/systemd/$u"
  done
  printf 'd /run/kb-term 0755 @USER@ @GROUP@ -\nd /run/kb-term/s 0700 @USER@ @GROUP@ -\nd /run/kb-term/runner 0700 @USER@ @GROUP@ -\n' \
    > "$dir/$pkg/share/kb-term/tmpfiles.d/kb-term.conf"
  printf 'tui:\n  maxInlineImages: 0\n' > "$dir/$pkg/share/kb-term/omp/overlay.yml"
  printf '# fake tmux conf\n' > "$dir/$pkg/share/kb-term/kb-term.conf"
  printf 'version=%s\ntarget=%s\ntmux=3.7c\n' "$ver" "$target" > "$dir/$pkg/share/kb-term/VERSIONS"
  printf 'kb-term %s — %s\n' "$ver" "$target" > "$dir/$pkg/INSTALL"
  tar -czf "$dir/${pkg}.tar.gz" -C "$dir" "$pkg"
  sha256sum "$dir/${pkg}.tar.gz" | awk '{print $1}' > "$dir/${pkg}.tar.gz.expected"
}

mkdir -p "$work/mirror" "$work/src" "$work/home"
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

# fake systemctl: logs argv, one line per call.
mkdir -p "$work/sys-bin"
cat > "$work/sys-bin/systemctl" <<'SYS'
#!/bin/sh
printf '%s\n' "$*" >> "$SYSTEMCTL_LOG"
exit 0
SYS
chmod +x "$work/sys-bin/systemctl"

# fake uname (aarch64) for case 10.
mkdir -p "$work/arch-bin"
cat > "$work/arch-bin/uname" <<'UN'
#!/bin/sh
case "$*" in
  "-s") echo "Linux" ;;
  "-m") echo "aarch64" ;;
  *) echo "aarch64" ;;
esac
UN
chmod +x "$work/arch-bin/uname"

run() {
  # run <prefix> [env...] ; env -i so only the listed variables survive.
  # Uses HOME under $work so nobody's home is touched; XDG_CONFIG_HOME also
  # points under $work so nothing is written outside the test dir.
  prefix="$1"; shift
  env -i HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" \
      PATH="$PATH" PREFIX="$prefix" \
      KB_BASE_URL="file://$work/mirror" KB_VERSION="$ver" "$@" \
      sh "$install_sh" >"$work/out" 2>&1
}
fail() { echo "FAIL: $*" >&2; sed 's/^/  | /' "$work/out" >&2; exit 1; }

say_ok() { printf 'ok: %s\n' "$*"; }

# 1. no sidecar -> hard error, nothing installed
build_fake_archive "$work/src" "tmux 3.7c" 1 stub ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
# (no .sha256 sidecar in the mirror)
if run "$work/p1"; then fail "missing sidecar must abort"; fi
[ ! -e "$work/p1/bin/kb-termd" ] || fail "kb-termd installed despite missing sidecar"
[ -z "$(find "$work/home/.config/systemd" -type f 2>/dev/null)" ] \
  || fail "unit dir not empty despite abort"
grep -q "sidecar unavailable" "$work/out" || fail "no sidecar message"
say_ok "1 missing sidecar aborts"

# 2. no sidecar + override -> installs
run "$work/p2" KB_INSECURE_SKIP_VERIFY=1 || fail "override should install"
[ -x "$work/p2/bin/kb-termd" ] || fail "kb-termd not installed with override"
say_ok "2 sidecar + override proceeds"

# 3. mismatching checksum aborts even with the override
echo "0000000000000000000000000000000000000000000000000000000000000000  ${pkg}.tar.gz" > "$work/mirror/${pkg}.tar.gz.sha256"
if run "$work/p3" KB_INSECURE_SKIP_VERIFY=1; then fail "checksum mismatch must abort"; fi
[ ! -e "$work/p3/bin/kb-termd" ] || fail "installed despite checksum mismatch"
grep -q "checksum mismatch" "$work/out" || fail "no mismatch message"
say_ok "3 mismatch aborts even with override"

# 4. good sidecar -> full install, layout and tokens asserted
build_fake_archive "$work/src" "tmux 3.7c" 1 stub ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
# manifest equality against the single source of truth
listed="$(tar -tzf "$work/mirror/${pkg}.tar.gz" | grep -v '/$' | LC_ALL=C sort)"
expected="$(print_manifest "$ver" "$target" | LC_ALL=C sort)"
[ "$listed" = "$expected" ] || fail "archive manifest mismatch"
run "$work/p4" || fail "good sidecar should install"
grep -q "checksum OK" "$work/out" || fail "no checksum OK"
[ "$(stat -c '%a' "$work/p4/bin/kb-termd")" = "755" ] || fail "kb-termd mode is not 0755"
[ "$(stat -c '%a' "$work/p4/bin/kb-term")" = "755" ] || fail "kb-term mode is not 0755"
[ "$(stat -c '%a' "$work/p4/bin/kb-tap")" = "755" ] || fail "kb-tap mode is not 0755"
[ -x "$work/p4/libexec/kb-term/tmux" ] || fail "tmux not in libexec"
[ ! -e "$work/p4/bin/tmux" ] || fail "tmux leaked into bin/"
[ "$(stat -c '%a' "$work/p4/libexec/kb-term/tmux")" = "755" ] || fail "tmux mode is not 0755"
[ "$("$work/p4/libexec/kb-term/tmux" -V)" = "tmux 3.7c" ] || fail "installed tmux does not report 3.7c"
for u in kb-termd.service kb-tap@.service kb-term@.service; do
  [ -f "$work/home/.config/systemd/user/$u" ] || fail "unit $u not installed"
done
! grep -rE '@[A-Z][A-Z0-9_]*@|__[A-Z][A-Z0-9_]*__' "$work/p4" >/dev/null 2>&1 \
  || fail "installer token survived under \$P"
! grep -rE '@[A-Z][A-Z0-9_]*@|__[A-Z][A-Z0-9_]*__' "$work/home/.config/systemd" >/dev/null 2>&1 \
  || fail "installer token survived in the unit dir"
grep -q "$(id -un)" "$work/p4/share/kb-term/tmpfiles.d/kb-term.conf" \
  || fail "rendered tmpfiles does not contain the user"
grep -q "sudo install -m 0644" "$work/out" || fail "next-steps sudo line missing"
grep -q "linger" "$work/out" || fail "linger hint missing"
grep -q "NOT put on PATH" "$work/out" || fail "tmux OFF PATH line missing"
grep -q "do not restart kb-term" "$work/out" || fail "do-not-restart warning missing"
say_ok "4 good sidecar installs with correct layout"

# 5. gh attestation fails -> abort with nothing installed; override proceeds
if run "$work/p5" PATH="$work/gh-bin:$PATH" FAKE_GH_VERIFY=1; then fail "failed attestation must abort"; fi
[ ! -e "$work/p5/bin/kb-termd" ] || fail "installed despite failed attestation"
grep -q "provenance" "$work/out" || fail "no provenance message"
run "$work/p6" PATH="$work/gh-bin:$PATH" FAKE_GH_VERIFY=1 KB_INSECURE_SKIP_VERIFY=1 \
  || fail "override should proceed past failed attestation"
[ -x "$work/p6/bin/kb-termd" ] || fail "kb-termd not installed with override"
say_ok "5 failed attestation aborts, override proceeds"

# 6. gh attestation passes -> "provenance OK"
run "$work/p7" PATH="$work/gh-bin:$PATH" FAKE_GH_VERIFY=0 || fail "good attestation should install"
grep -q "provenance OK" "$work/out" || fail "no provenance OK"
say_ok "6 passing attestation says provenance OK"

# 7. no sha256 tool at all -> hard error
mkdir -p "$work/min-bin"
for t in sh curl uname mktemp tar find head awk sed grep cp mkdir rm chmod basename cat dirname tr stat dirname mv id pwd; do
  p="$(command -v "$t" 2>/dev/null || true)"; [ -z "$p" ] || ln -sf "$p" "$work/min-bin/$t"
done
# (the tool list deliberately excludes sha256sum and shasum)
if run "$work/p8" PATH="$work/min-bin"; then fail "missing sha tool must abort"; fi
grep -q "no sha256 tool" "$work/out" || fail "no sha-tool message"
say_ok "7 missing sha tool aborts"

# 8. fake tmux prints tmux 3.4 -> abort BEFORE anything is installed
build_fake_archive "$work/src" "tmux 3.4" 1 stub ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
if run "$work/p9"; then fail "wrong tmux version must abort"; fi
[ ! -e "$work/p9/bin/kb-termd" ] || fail "installed despite wrong tmux version"
grep -q "3.7c" "$work/out" || fail "version error does not name 3.7c"
say_ok "8 wrong tmux version aborts before installing"

# 9. archive missing bin/kb-tap -> abort, nothing installed
build_fake_archive "$work/src" "tmux 3.7c" 0 stub ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
if run "$work/p10"; then fail "missing member must abort"; fi
[ ! -e "$work/p10/bin/kb-termd" ] || fail "installed despite missing member"
grep -q "missing required member" "$work/out" || fail "no missing-member message"
say_ok "9 missing member aborts"

# 10. uname shim printing aarch64 -> x86_64-only abort
build_fake_archive "$work/src" "tmux 3.7c" 1 stub ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
if run "$work/p11" PATH="$work/arch-bin:$PATH"; then fail "aarch64 must abort"; fi
grep -q "x86_64 Linux only" "$work/out" || fail "no x86_64-only message"
say_ok "10 aarch64 aborts"

# 11. idempotent re-run over a RUNNING ELF (ETXTBSY). The kb-termd stub is a
# byte copy of the host `sleep`, started in the background between installs;
# archive 2 changes kb-termd and the unit. The installer replaces both via
# same-dir temp + mv -f, so it never fails with "text file busy".
build_fake_archive "$work/src" "tmux 3.7c" 1 sleep-elf ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
run "$work/p12" || fail "ELF install 1 should succeed"
"$work/p12/bin/kb-termd" 30 &
running_pid=$!
# give the background process a moment to be running
sleep 1
if ! kill -0 "$running_pid" 2>/dev/null; then fail "kb-termd fake did not start"; fi
# archive 2: different kb-termd ELF and a changed unit stub
build_fake_archive "$work/src" "tmux 3.7c" 1 true-plus "second"
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
run "$work/p12" || fail "reinstall over a running ELF must not fail"
cmp "$work/p12/bin/kb-termd" "$work/src/$pkg/bin/kb-termd" \
  || fail "new kb-termd bytes are not in place after reinstall"
grep -q -- "--second" "$work/home/.config/systemd/user/kb-termd.service" \
  || fail "stale unit was not replaced"
kill "$running_pid" 2>/dev/null || true
wait "$running_pid" 2>/dev/null || true
say_ok "11 reinstall over a running ELF succeeds with new bytes"

# 12. the installer never enables/starts/stops/restarts a unit
build_fake_archive "$work/src" "tmux 3.7c" 1 stub ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
: > "$work/systemctl.log"
run "$work/p13" PATH="$work/sys-bin:$PATH" SYSTEMCTL_LOG="$work/systemctl.log" \
  || fail "install with systemctl shim should succeed"
[ "$(cat "$work/systemctl.log")" = "--user daemon-reload" ] \
  || fail "systemctl argv log is not only --user daemon-reload: $(cat "$work/systemctl.log")"
say_ok "12 installer only runs systemctl --user daemon-reload"

echo "install-kb-term selftest OK"
