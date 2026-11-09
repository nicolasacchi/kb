#!/usr/bin/env bash
# Package the kb-term host-side release archive (kb-term-<ver>-<target>.tar.gz).
#
# This script builds NOTHING. Inputs come from the environment (the release
  # workflow and the CI `term-int` job build the four components first and
  # then call this script — one shared script, so a packaging bug shows up on
  # the PR, not on release day):
#
#   KB_TERM_VERSION    archive version, e.g. `0.44` or `main-<sha12>`
#   KB_TERM_TARGET     target, e.g. `x86_64-unknown-linux-gnu` (x86_64 only)
#   KB_TERMD_BIN       path to the built `kb-termd`
#   KB_TERM_BIN        path to the built `kb-term`
#   KB_TAP_BIN         path to the compiled `kb-tap` (bun single binary)
#   KB_TMUX_BIN        path to the pinned tmux binary (3.7c)
#   OUT_DIR            where the tarball lands (default: `.`)
#   KB_TMUX_SRC_SHA256 optional: sha256 of the pinned tmux source tarball
#   KB_BUN_VERSION     optional: bun version the tap was compiled with
#   KB_GIT_SHA         optional: short git sha recorded in VERSIONS
#
# Inputs that are unset, or that do not name an executable file, are a hard
# error naming the variable. The systemd/tmpfiles templates and the omp
# overlay come from this repository (crates/kb-term/share/) and are copied
# verbatim — never inlined here.
#
# SAFETY: every action runs inside main(), called ONLY on the final line.
set -euo pipefail

say() { printf '%s\n' "$*"; }
err() { printf 'package-kb-term: error: %s\n' "$*" >&2; exit 1; }

require_executable() {
  # $1 = variable name; fails naming the variable when unset or not an
  # executable regular file (an executable directory is not a binary).
  v="${!1:-}"
  if [ -z "$v" ]; then
    err "$1 is unset"
  fi
  if [ ! -f "$v" ] || [ ! -x "$v" ]; then
    err "$1 ($v) is not an executable file"
  fi
}

main() {
  require_executable KB_TERMD_BIN
  require_executable KB_TERM_BIN
  require_executable KB_TAP_BIN
  require_executable KB_TMUX_BIN

  [ -n "${KB_TERM_VERSION:-}" ] || err "KB_TERM_VERSION is unset"
  [ -n "${KB_TERM_TARGET:-}" ] || err "KB_TERM_TARGET is unset"
  [ "${KB_TERM_TARGET}" = "x86_64-unknown-linux-gnu" ] \
    || err "kb-term is published for x86_64 Linux only (got target '${KB_TERM_TARGET}')"

  out_dir="${OUT_DIR:-.}"
  mkdir -p "${out_dir}"

  repo_root="$(cd "$(dirname "$0")/.." && pwd)"
  share_src="${repo_root}/crates/kb-term/share"

  for f in \
      "${share_src}/systemd/kb-termd.service" \
      "${share_src}/systemd/kb-tap@.service" \
      "${share_src}/systemd/kb-term@.service" \
      "${share_src}/tmpfiles.d/kb-term.conf" \
      "${share_src}/omp/overlay.yml" \
      "${share_src}/kb-term.conf"; do
    [ -f "$f" ] || err "source template missing: $f"
  done

  # The pinned tmux: a long-lived tmux server must never be paired with a
  # client from an upgraded package, so the pinned 3.7c ships in the archive.
  tmux_version="$("${KB_TMUX_BIN}" -V 2>/dev/null || true)"
  if [ "${tmux_version}" != "tmux 3.7c" ]; then
    err "pinned tmux must report 'tmux 3.7c' (got '${tmux_version}') — refusing to package"
  fi

  name="kb-term-${KB_TERM_VERSION}-${KB_TERM_TARGET}"
  stage="$(mktemp -d)"
  trap 'rm -rf "${stage}"' EXIT
  pkg="${stage}/${name}"
  mkdir -p "${pkg}/bin" "${pkg}/libexec/kb-term" \
           "${pkg}/share/kb-term/systemd" "${pkg}/share/kb-term/tmpfiles.d" \
           "${pkg}/share/kb-term/omp"

  cp "${KB_TERMD_BIN}" "${pkg}/bin/kb-termd"
  cp "${KB_TERM_BIN}"  "${pkg}/bin/kb-term"
  cp "${KB_TAP_BIN}"   "${pkg}/bin/kb-tap"
  cp "${KB_TMUX_BIN}"  "${pkg}/libexec/kb-term/tmux"
  chmod 0755 "${pkg}/bin/kb-termd" "${pkg}/bin/kb-term" "${pkg}/bin/kb-tap" \
             "${pkg}/libexec/kb-term/tmux"
  # Strip the Rust bins and tmux best effort. Never strip `kb-tap`:
  # stripping a `bun --compile` binary corrupts the appended payload.
  strip "${pkg}/bin/kb-termd" "${pkg}/bin/kb-term" \
        "${pkg}/libexec/kb-term/tmux" 2>/dev/null || true

  # Source templates, copied verbatim.
  cp "${share_src}/systemd/kb-termd.service" \
     "${share_src}/systemd/kb-tap@.service" \
     "${share_src}/systemd/kb-term@.service" "${pkg}/share/kb-term/systemd/"
  cp "${share_src}/tmpfiles.d/kb-term.conf" "${pkg}/share/kb-term/tmpfiles.d/kb-term.conf"
  cp "${share_src}/omp/overlay.yml" "${pkg}/share/kb-term/omp/overlay.yml"
  cp "${share_src}/kb-term.conf" "${pkg}/share/kb-term/kb-term.conf"

  # Relocatable sidecar (bare file name, generated from inside the directory).
  ( cd "${pkg}/libexec/kb-term" && sha256sum tmux > tmux.sha256 )

  cat > "${pkg}/share/kb-term/VERSIONS" <<EOF
version=${KB_TERM_VERSION}
target=${KB_TERM_TARGET}
tmux=3.7c
tmux_src_sha256=${KB_TMUX_SRC_SHA256:-}
bun=${KB_BUN_VERSION:-}
git_sha=${KB_GIT_SHA:-}
EOF

  # Runtime libraries the shipped tmux needs (dynamic builds only).
  ldd_list=""
  if out="$(ldd "${KB_TMUX_BIN}" 2>/dev/null)"; then
    case "${out}" in
      *"not a dynamic executable"*|'') : ;;
      *)
        ldd_list="${out}"
        ;;
    esac
  fi

  {
    say "kb-term ${KB_TERM_VERSION} — ${KB_TERM_TARGET}"
    say ""
    say "Contents:"
    say "  bin/kb-termd          the kb-term runner (runner.sock)"
    say "  bin/kb-term           the host-local client (attach|new|ls|doctor)"
    say "  bin/kb-tap            the tap (bun single binary)"
    say "  libexec/kb-term/tmux the pinned tmux 3.7c (never installed on PATH)"
    say "  share/kb-term/        systemd units, tmpfiles.d, omp overlay, kb-term.conf"
    say "  share/kb-term/VERSIONS  build facts for this archive"
    say ""
    say "Install with scripts/install-kb-term.sh from the kb repository:"
    say ""
    say "  curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install-kb-term.sh | sh"
    say ""
    say "It downloads, verifies the checksum and provenance, and installs"
    say "under ~/.local (override with PREFIX). The pinned tmux goes to"
    say "libexec/kb-term/tmux — off your PATH — so a distro tmux upgrade"
    say "can never mismatch the client and the long-lived holder servers."
    say ""
    if [ -n "${ldd_list}" ]; then
      say "Runtime libraries this tmux needs (ldd of the shipped binary):"
      printf '%s\n' "${ldd_list}"
      say ""
    fi
    say "Verify this archive's provenance:"
    say "  gh attestation verify ${name}.tar.gz -R nicolasacchi/kb"
    say ""
    say "Docs: https://github.com/nicolasacchi/kb"
  } > "${pkg}/INSTALL"

  archive="${name}.tar.gz"
  tar -czf "${out_dir}/${archive}" -C "${stage}" "${name}"
  # Bare file name in the sidecar, generated from inside the output dir, so
  # a manual `sha256sum -c <archive>.sha256` works in the download dir even
  # when OUT_DIR is an absolute path.
  ( cd "${out_dir}" && sha256sum "${archive}" > "${archive}.sha256" )
  say "${out_dir}/${archive}"

  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "archive=${archive}" >> "${GITHUB_OUTPUT}"
  fi
}

main "$@"
