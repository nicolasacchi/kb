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
packager_sh="$here/../package-kb-term.sh"
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
#   $3 = kb-termd source ("stub" | "bash-elf")
#   $4 = kb-termd-2 source for archive 2 | none
#   $5 = unit extra line ("" / "second")
#   $6 = sidecar mode ("" = good tmux.sha256, "corrupt" = zero hash)
ver="9.9.9"
target="x86_64-unknown-linux-gnu"
pkg="kb-term-${ver}-${target}"

build_fake_archive() {
  # $1 dir   $2 tmux line   $3 include-tap   $4 termd src   $5 unit marker   $6 sidecar
  dir="$1"; tmux_line="$2"; include_tap="$3"; termd_src="$4"; unit_marker="$5"; sidecar_mode="${6:-}"
  rm -rf "$dir"
  mkdir -p "$dir/$pkg/bin" "$dir/$pkg/libexec/kb-term" \
           "$dir/$pkg/share/kb-term/systemd" "$dir/$pkg/share/kb-term/tmpfiles.d" \
           "$dir/$pkg/share/kb-term/omp"
  if [ "$termd_src" = "bash-elf" ]; then
    cp "$(type -P bash)" "$dir/$pkg/bin/kb-termd"
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
  if [ "$sidecar_mode" = "corrupt" ]; then
    printf '0000000000000000000000000000000000000000000000000000000000000000  tmux\n' \
      > "$dir/$pkg/libexec/kb-term/tmux.sha256"
  else
    ( cd "$dir/$pkg/libexec/kb-term" && sha256sum tmux > tmux.sha256 )
  fi
  for u in kb-termd.service kb-tap@.service kb-term@.service; do
    printf "[Unit]\nDescription=kb-term ${u}\nExecStart=@PREFIX@/bin/kb-term --${unit_marker}\n" \
      > "$dir/$pkg/share/kb-term/systemd/$u"
  done
  # The shipped tmpfiles spelling (__USER__ __USER__), as in the real archive.
  printf 'd /run/kb-term 0755 __USER__ __USER__ -\nd /run/kb-term/s 0700 __USER__ __USER__ -\nd /run/kb-term/runner 0700 __USER__ __USER__ -\n' \
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
# Failing curl (case 15) and Darwin uname (case 17).
mkdir -p "$work/curlfail-bin"
cat > "$work/curlfail-bin/curl" <<'CURL'
#!/bin/sh
exit 1
CURL
chmod +x "$work/curlfail-bin/curl"

mkdir -p "$work/os-bin"
cat > "$work/os-bin/uname" <<'OSUN'
#!/bin/sh
case "$*" in
  "-s") echo "Darwin" ;;
  "-m") echo "x86_64" ;;
  *) echo "x86_64" ;;
esac
OSUN
chmod +x "$work/os-bin/uname"

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
say_ok "2 missing sidecar + override proceeds"

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
  # The substituted @PREFIX@ value is the actual install root.
  grep -qF "$work/p4" "$work/home/.config/systemd/user/$u" \
    || fail "unit $u does not carry the rendered prefix $work/p4"
done
! grep -rE '@[A-Z][A-Z0-9_]*@|__[A-Z][A-Z0-9_]*__' "$work/p4" >/dev/null 2>&1 \
  || fail "installer token survived under \$P"
! grep -rE '@[A-Z][A-Z0-9_]*@|__[A-Z][A-Z0-9_]*__' "$work/home/.config/systemd" >/dev/null 2>&1 \
  || fail "installer token survived in the unit dir"
grep -qF "$(id -un)" "$work/p4/share/kb-term/tmpfiles.d/kb-term.conf" \
  || fail "rendered tmpfiles does not contain the user"
grep -qF "$(id -gn)" "$work/p4/share/kb-term/tmpfiles.d/kb-term.conf" \
  || fail "rendered tmpfiles does not contain the group"
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
# byte copy of the host `bash`, started in the background between installs;
# archive 2 changes kb-termd and the unit. The installer replaces both via
# same-dir temp + mv -f, so it never fails with "text file busy".
# (The brief suggests a copied `sleep`; current GitHub runner images ship
# GNU coreutils as multicalls that name-check the on-disk executable —
# "unknown program" as kb-termd, "Security violation" via a sleep symlink
# or exec -a — so no sleep copy can run there. A copied bash has no name
# check and IS a running ELF: the only property ETXTBSY needs.)
build_fake_archive "$work/src" "tmux 3.7c" 1 bash-elf ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
run "$work/p12" || fail "ELF install 1 should succeed"
"$work/p12/bin/kb-termd" -c 'while :; do sleep 1; done' &
running_pid=$!
# Give the background process a moment to be running (bounded poll: a slow
# runner must not false-fail the start check).
alive=0
for _ in $(seq 1 50); do
  if kill -0 "$running_pid" 2>/dev/null; then alive=1; break; fi
  sleep 0.1
done
[ "$alive" = "1" ] || fail "kb-termd fake did not start"
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

# 13. a leading `v` is stripped (KB_VERSION=v9.9.9 installs 9.9.9)
build_fake_archive "$work/src" "tmux 3.7c" 1 stub ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
run "$work/p14" KB_VERSION="v${ver}" || fail "v-prefixed version should install"
[ -x "$work/p14/bin/kb-termd" ] || fail "v-prefixed install did not place kb-termd"
grep -q "version : ${ver}$" "$work/out" || fail "stripped version not reported"
say_ok "13 leading v is stripped from KB_VERSION"

# 14. KB_BASE_URL without KB_VERSION aborts with the requires-KB_VERSION error
if env -i HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" PATH="$PATH" \
    PREFIX="$work/p15" KB_BASE_URL="file://$work/mirror" \
    sh "$install_sh" >"$work/out" 2>&1; then
  fail "KB_BASE_URL without KB_VERSION must abort"
fi
grep -q "KB_BASE_URL requires KB_VERSION" "$work/out" \
  || fail "no requires-KB_VERSION message"
[ ! -e "$work/p15/bin/kb-termd" ] || fail "installed despite missing KB_VERSION"
say_ok "14 KB_BASE_URL without KB_VERSION aborts"

# 15. a failed non-file download aborts with the exact no-archive message
if env -i HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" \
    PATH="$work/curlfail-bin:$PATH" PREFIX="$work/p16" KB_VERSION="$ver" \
    sh "$install_sh" >"$work/out" 2>&1; then
  fail "failed download must abort"
fi
grep -qF "no kb-term archive in release v${ver}; pin KB_VERSION to a tag built after kb-term shipped, or install a main build (docs/kb-term-install.md)" "$work/out" \
  || fail "no exact no-archive message"
[ ! -e "$work/p16/bin/kb-termd" ] || fail "installed despite failed download"
say_ok "15 failed download names the no-archive error"

# 16. a corrupted tmux sidecar aborts before anything is installed
# (the override must not bypass it: it is a hard error, always)
build_fake_archive "$work/src" "tmux 3.7c" 1 stub "" corrupt
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
if run "$work/p17"; then fail "corrupted tmux sidecar must abort"; fi
[ ! -e "$work/p17/bin/kb-termd" ] || fail "installed despite corrupted sidecar"
grep -q "does not match its sidecar" "$work/out" || fail "no sidecar-mismatch message"
if run "$work/p17b" KB_INSECURE_SKIP_VERIFY=1; then
  fail "corrupted sidecar must abort even with the override"
fi
[ ! -e "$work/p17b/bin/kb-termd" ] \
  || fail "installed despite corrupted sidecar (override)"
say_ok "16 corrupted tmux sidecar aborts (override included)"

# 17. a non-Linux OS (Darwin uname) -> x86_64-Linux-only abort
build_fake_archive "$work/src" "tmux 3.7c" 1 stub ""
cp "$work/src/${pkg}.tar.gz" "$work/mirror/"
cp "$work/src/${pkg}.tar.gz.expected" "$work/mirror/${pkg}.tar.gz.sha256"
if run "$work/p18" PATH="$work/os-bin:$PATH"; then fail "Darwin must abort"; fi
grep -q "x86_64 Linux only" "$work/out" || fail "no x86_64-only message for Darwin"
[ ! -e "$work/p18/bin/kb-termd" ] || fail "installed despite non-Linux OS"
say_ok "17 Darwin aborts"

# 18. a corrupt (non-tar) archive -> extract failed, nothing installed
printf 'not a gzip stream' > "$work/mirror/${pkg}.tar.gz"
sha256sum "$work/mirror/${pkg}.tar.gz" | awk '{print $1}' \
  > "$work/mirror/${pkg}.tar.gz.sha256"
if run "$work/p19"; then fail "corrupt archive must abort"; fi
grep -q "extract failed" "$work/out" || fail "no extract-failed message"
[ ! -e "$work/p19/bin/kb-termd" ] || fail "installed despite corrupt archive"
say_ok "18 corrupt archive fails at extract"

# 19-21. packager gates: the real package-kb-term.sh must refuse bad inputs
# (the CI lane only ever feeds it good inputs, so these are the false
# witnesses for its gates: unset var, non-file input, tmux 3.4).
mkdir -p "$work/pkgin" "$work/pkgout"
printf '#!/bin/sh\nexit 0\n' > "$work/pkgin/termd"
printf '#!/bin/sh\nexit 0\n' > "$work/pkgin/term"
printf '#!/bin/sh\nexit 0\n' > "$work/pkgin/tap"
printf '#!/bin/sh\necho "tmux 3.7c"\n' > "$work/pkgin/tmux"
printf '#!/bin/sh\necho "tmux 3.4"\n' > "$work/pkgin/tmux34"
chmod +x "$work/pkgin/termd" "$work/pkgin/term" "$work/pkgin/tap" \
  "$work/pkgin/tmux" "$work/pkgin/tmux34"
pkg_run() {
  # pkg_run [env assignments...]
  env -i HOME="$work/home" PATH="$PATH" \
    KB_TERMD_BIN="$work/pkgin/termd" KB_TERM_BIN="$work/pkgin/term" \
    KB_TAP_BIN="$work/pkgin/tap" KB_TMUX_BIN="$work/pkgin/tmux" \
    KB_TERM_VERSION=9.9.9 KB_TERM_TARGET=x86_64-unknown-linux-gnu \
    OUT_DIR="$work/pkgout" "$@" bash "$packager_sh" >"$work/out" 2>&1
}
# 19. an unset input var fails, naming it (empty string takes the unset path)
if pkg_run KB_TERMD_BIN=; then fail "unset KB_TERMD_BIN must fail"; fi
grep -q "KB_TERMD_BIN is unset" "$work/out" \
  || fail "no unset-var message naming KB_TERMD_BIN"
say_ok "19 packager rejects an unset input var"
# 20. a directory is not an executable file (it must not reach `cp`)
if pkg_run KB_TERMD_BIN="$work/pkgin"; then
  fail "directory KB_TERMD_BIN must fail"
fi
grep -q "KB_TERMD_BIN ($work/pkgin) is not an executable file" "$work/out" \
  || fail "no not-an-executable-file message for a directory"
say_ok "20 packager rejects a directory as a binary"
# 21. a tmux that is not 3.7c fails closed, naming the pin
if pkg_run KB_TMUX_BIN="$work/pkgin/tmux34"; then
  fail "tmux 3.4 must fail the packager"
fi
grep -q "tmux 3.7c" "$work/out" || fail "packager error does not name 3.7c"
grep -q "refusing to package" "$work/out" \
  || fail "packager error is not fail-closed"
say_ok "21 packager rejects tmux 3.4"

echo "install-kb-term selftest OK"
