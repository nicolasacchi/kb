#!/usr/bin/env bash
# Build or reuse a pinned tmux 3.7c for the kb-term integration lanes.
#
# kb-term (the private-tmux-holder terminal feature) is developed against
# tmux 3.7c: control mode, `refresh-client -f pause-after`, `window-size
# manual`, `extended-keys always`, and the recorded width table. GitHub's
# hosted images ship an older tmux (3.4), so CI builds the pinned release
# from source. The tarball sha256 is HARD-CODED below: it is the only
# trust anchor, it cannot be overridden by the environment, and the script
# verifies it BEFORE extracting or executing anything (fail closed).
#
# Output: "$OUT/bin/tmux", with OUT="${KB_TMUX_OUT:-${RUNNER_TEMP:-/tmp}/tmux-3.7c}".
#   - KB_TMUX_URL   download URL (selftest mirror hook; downloads only —
#                   the sha pin still refuses a foreign tarball).
#   - KB_TMUX_WORK  work directory (default a fresh mktemp -d, removed on exit).
#   - KB_TMUX_STATIC=0 disables --enable-static (dynamic build only).
# The last line of this file is `main "$@"`, so a truncated copy defines
# functions but executes nothing.
set -euo pipefail

TMUX_VERSION="3.7c"
TMUX_URL="${KB_TMUX_URL:-https://github.com/tmux/tmux/releases/download/3.7c/tmux-3.7c.tar.gz}"
TMUX_SHA256="7c60cae9a0e25288e2e24750aafc9e8800fc7fd4555e447e1b29ee4201cfb3bf"

OUT="${KB_TMUX_OUT:-${RUNNER_TEMP:-/tmp}/tmux-3.7c}"

say()  { printf '%s\n' "$*"; }
err()  { printf 'build-tmux: error: %s\n' "$*" >&2; exit 1; }

check_pin() {
  # Fail closed on an unset or malformed pin — the sha is the trust anchor.
  if [ -z "${TMUX_SHA256}" ]; then
    err "tmux sha pin is empty (refusing to download without a pin)"
  fi
  case "${TMUX_SHA256}" in
    *[!0-9a-f]*)
      err "tmux sha pin is not lowercase hex (refusing to build)" ;;
  esac
  if [ "${#TMUX_SHA256}" -ne 64 ]; then
    err "tmux sha pin is not 64 hex chars — refusing to build"
  fi
}

install_runtime_libs() {
  # A cached dynamic binary runs on a fresh image that may lack its libs.
  # Package names are those of the current Ubuntu image; fix from the
  # first run's BUILD-INFO `ldd` output if they age out.
  if [ "${CI:-}" = "true" ]; then
    say "installing tmux runtime libraries"
    sudo apt-get update -y
    sudo apt-get install -y libevent-core-2.1-7t64 libutf8proc3 libncurses6
  fi
}

install_build_deps() {
  # Skip when the toolchain is already present (local runs need no sudo).
  if command -v gcc >/dev/null 2>&1 && command -v make >/dev/null 2>&1 \
     && [ -f /usr/include/event.h ] && [ -f /usr/include/ncurses.h ] \
     && [ -f /usr/include/utf8proc.h ] && command -v bison >/dev/null 2>&1 \
     && command -v pkg-config >/dev/null 2>&1; then
    return 0
  fi
  if [ "${CI:-}" = "true" ]; then
    sudo apt-get update -y
    sudo apt-get install -y build-essential libevent-dev libncurses-dev libutf8proc-dev bison pkg-config
  fi
}

export_result() {
  # Print the binary path and hand it to later steps via GITHUB_ENV.
  say "${OUT}/bin/tmux"
  if [ -n "${GITHUB_ENV:-}" ]; then
    printf 'KB_TMUX_BIN=%s/bin/tmux\n' "${OUT}" >> "${GITHUB_ENV}"
  fi
}

cached_so_the_build_is_a_noop() {
  # The cache hit: the right binary is already there, so download nothing.
  if [ -x "${OUT}/bin/tmux" ]; then
    version="$("${OUT}/bin/tmux" -V 2>/dev/null || true)"
    if [ "${version}" = "tmux ${TMUX_VERSION}" ]; then
      say "tmux ${TMUX_VERSION} cached at ${OUT}/bin/tmux"
      linkage="$(cat "${OUT}/LINKAGE" 2>/dev/null || echo unknown)"
      case "${linkage}" in
        static|dynamic) ;;
        *)
          # Fail closed on unknown linkage state: a cache entry that
          # does not say exactly how its binary is linked is not
          # trusted — rebuild from the pinned tarball instead of
          # silently skipping the runtime-library install.
          say "tmux ${TMUX_VERSION} cache has unexpected LINKAGE '${linkage}'; rebuilding"
          return 1 ;;
      esac
      if [ "${linkage}" = "dynamic" ]; then
        install_runtime_libs
      fi
      export_result
      return 0
    fi
  fi
  return 1
}

build_tarball() {
  # $1 = validated tarball path. Verifies the sha, extracts ONLY into a
  # fresh empty directory, then configures/builds/installs.
  tarball="$1"
  work_dir="$2"
  extract_dir="${work_dir}/extract"
  src_dir="${extract_dir}/tmux-${TMUX_VERSION}"

  actual="$(sha256sum "${tarball}" | awk '{print $1}')"
  if [ "${actual}" != "${TMUX_SHA256}" ]; then
    err "sha256 mismatch for tmux ${TMUX_VERSION} tarball (got ${actual}): refusing to extract"
  fi

  mkdir "${extract_dir}"
  tar -xzf "${tarball}" --no-same-owner -C "${extract_dir}"
  [ -d "${src_dir}" ] || err "extracted tarball has no tmux-${TMUX_VERSION} directory"

  install_build_deps

  configure_flags="--prefix=${OUT} --enable-utf8proc"
  if [ "${KB_TMUX_STATIC:-1}" != "0" ]; then
    configure_flags="${configure_flags} --enable-static"
  fi

  build_once() {
    ( cd "${src_dir}" && ./configure ${configure_flags} && make -j"$(nproc)" && make install )
  }

  linkage="dynamic"
  if [ "${KB_TMUX_STATIC:-1}" != "0" ]; then
    if build_once; then
      linkage="static"
    else
      say "static tmux build failed; retrying without --enable-static"
      ( cd "${src_dir}" && make distclean ) || true
      if [ -d "${OUT}" ]; then rm -rf "${OUT}"; fi
      configure_flags="--prefix=${OUT} --enable-utf8proc"
      build_once || err "tmux ${TMUX_VERSION} build failed (static and dynamic)"
      linkage="dynamic"
    fi
  else
    build_once || err "tmux ${TMUX_VERSION} build failed"
    linkage="dynamic"
  fi

  printf '%s\n' "${linkage}" > "${OUT}/LINKAGE"
  printf '::notice::tmux %s built with %s linkage\n' "${TMUX_VERSION}" "${linkage}"

  built="$("${OUT}/bin/tmux" -V)"
  if [ "${built}" != "tmux ${TMUX_VERSION}" ]; then
    err "built tmux reports '${built}', expected 'tmux ${TMUX_VERSION}'"
  fi

  {
    say "version=tmux ${TMUX_VERSION}"
    say "sha256=${TMUX_SHA256}"
    say "configure_flags=${configure_flags}"
    say "linkage=${linkage}"
    say "ImageOS=${ImageOS:-}"
    say "ImageVersion=${ImageVersion:-}"
    if [ "${linkage}" = "dynamic" ]; then
      say "--- ldd ---"
      ldd "${OUT}/bin/tmux" || true
    fi
  } > "${OUT}/BUILD-INFO"

  say "tmux ${TMUX_VERSION} built at ${OUT}/bin/tmux"
  export_result
}

main() {
  check_pin

  if cached_so_the_build_is_a_noop; then
    exit 0
  fi

  work_dir="${KB_TMUX_WORK:-}"
  if [ -z "${work_dir}" ]; then
    work_dir="$(mktemp -d)"
    cleanup_on_exit=1
  else
    mkdir -p "${work_dir}"
    cleanup_on_exit=0
  fi

  if [ "${cleanup_on_exit}" = "1" ]; then
    trap 'rm -rf "${work_dir}"' EXIT
  fi

  tarball="${work_dir}/tmux-${TMUX_VERSION}.tar.gz"
  case "${TMUX_URL}" in
    file://*)
      curl --tlsv1.2 -fsSL --retry 3 -o "${tarball}" "${TMUX_URL}" ;;
    *)
      curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "${tarball}" "${TMUX_URL}" ;;
  esac

  build_tarball "${tarball}" "${work_dir}"
}

main "$@"
