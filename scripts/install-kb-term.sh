#!/bin/sh
# kb-term installer — fetch the `kb-term-<ver>-x86_64-unknown-linux-gnu` release
# archive and install the host-side kb-term host components: the runner
# (kb-termd), the operator-local client (kb-term), the compiled tap (kb-tap) and
# the pinned tmux 3.7c — the latter OFF the PATH (libexec), so a distro tmux
# upgrade can never mismatch clients and long-lived holder servers.
#
#   curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install-kb-term.sh | sh
#
# or from a checkout:  sh scripts/install-kb-term.sh
#
# Environment overrides:
#   PREFIX       install root (default ~/.local); binaries land in $PREFIX/bin
#                Known limitation: the shipped systemd units, the tmpfiles
#                file and the kb-termd config lookup assume the default
#                ~/.local layout; with a custom PREFIX the operator must
#                adjust the installed units/conf to match (see
#                docs/kb-term-install.md).
#   KB_BASE_URL  fetch the archive from this directory URL instead of GitHub
#                Releases (air-gapped mirror / CI smoke test). Requires
#                KB_VERSION (a bare mirror has no "latest" concept).
#   KB_INSECURE_SKIP_VERIFY=1
#                proceed WITHOUT checksum verification when the .sha256 sidecar
#                or a sha256 tool is missing, and when `gh attestation verify`
#                cannot confirm the build provenance. Default: any of those is
#                a hard error (fail closed, like install.sh).
#   XDG_CONFIG_HOME  systemd user dir parent (default ~/.config)
#   KB_TERM_SKIP_SYSTEMD=1
#                CI / tests only: skip the `systemctl --user daemon-reload`
#
# SAFETY: every action runs inside main(), which is called ONLY on the final
# line of this file. A truncated download (curl|sh cut mid-stream) therefore
# defines functions but executes nothing — no half-run install.
#
# POSIX sh — no bashisms, no `local`.
set -eu

REPO="nicolasacchi/kb"
TERM_TARGET="x86_64-unknown-linux-gnu"

# ---- output -----------------------------------------------------------------
say()  { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
err()  { printf 'install-kb-term: error: %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# ---- network (curl or wget) -------------------------------------------------
# Fetch a URL to stdout.
fetch() {
  if have curl; then
    curl -fsSL "$1"
  elif have wget; then
    wget -qO- "$1"
  else
    err "need curl or wget on PATH"
  fi
}

# Download a URL to a file.
download() {
  # $1 url   $2 dest
  if have curl; then
    curl -fSL --retry 3 -o "$2" "$1"
  elif have wget; then
    wget -q -O "$2" "$1"
  else
    err "need curl or wget on PATH"
  fi
}

# ---- platform ---------------------------------------------------------------
detect_target() {
  os=$(uname -s 2>/dev/null || echo unknown)
  arch=$(uname -m 2>/dev/null || echo unknown)

  [ "$os" = "Linux" ] || err "no kb-term release for OS '$os' — kb-term is published for x86_64 Linux only"
  case "$arch" in
    x86_64|amd64) : ;;
    *) err "no kb-term release for architecture '$arch' — kb-term is published for x86_64 Linux only" ;;
  esac
}

# ---- version resolution -----------------------------------------------------
# Strip a leading `v` only when it matches v[0-9]; `main-<sha12>` passes
# through unchanged. Sets VERSION; TAG is derived when talking to GitHub.
resolve_version() {
  if [ -n "${KB_VERSION:-}" ]; then
    case "${KB_VERSION}" in
      v[0-9]*) VERSION="${KB_VERSION#v}" ;;
      *)       VERSION="${KB_VERSION}" ;;
    esac
    return 0
  fi
  [ -z "${KB_BASE_URL:-}" ] || err "KB_BASE_URL requires KB_VERSION (e.g. KB_VERSION=main-<sha12>)"
  # Latest release tag via the GitHub API, parsed without jq. `sed` exits 0 on
  # empty input, so the pipeline never trips `set -e`; the emptiness check does.
  tag=$(fetch "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
        | grep -m1 '"tag_name"' \
        | sed -e 's/.*"tag_name"[^"]*"//' -e 's/".*//') || tag=""
  [ -n "$tag" ] || err "could not resolve the latest release (is one published yet? try KB_VERSION=…)"
  case "$tag" in
    v[0-9]*) TAG="$tag"; VERSION="${tag#v}" ;;
    *)       TAG="v${tag}"; VERSION="${tag}" ;;
  esac
}

# ---- integrity (fail closed) ------------------------------------------------
sha256_of() {
  if have sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif have shasum;    then shasum -a 256 "$1" | awk '{print $1}'
  else echo ""; fi
}

# A verification we cannot perform is a failure, not a pass — unless the
# operator says so explicitly. $1 = what could not be verified.
cannot_verify() {
  if [ "${KB_INSECURE_SKIP_VERIFY:-}" = "1" ]; then
    info "WARNING: $1 — continuing because KB_INSECURE_SKIP_VERIFY=1"
    return 0
  fi
  err "$1 — refusing to install an unverified archive (set KB_INSECURE_SKIP_VERIFY=1 to override at your own risk)"
}

verify_checksum() {
  # $1 file   $2 url of the .sha256 sidecar
  want=$(fetch "$2" 2>/dev/null | awk '{print $1}') || want=""
  if [ -z "$want" ]; then
    cannot_verify "checksum sidecar unavailable ($2)"
    return 0
  fi
  got=$(sha256_of "$1")
  if [ -z "$got" ]; then
    cannot_verify "no sha256 tool found (need sha256sum or shasum)"
    return 0
  fi
  [ "$want" = "$got" ] || err "checksum mismatch (want ${want}, got ${got})"
  info "checksum OK"
}

# The checksum sits beside the artifact on the same release, so it proves
# integrity, not authenticity. When `gh` is present and signed in, also verify
# the SLSA build provenance the release workflow attests for every tarball.
verify_attestation() {
  # $1 file
  if ! have gh; then
    info "gh not found — provenance not checked (checksum only); install gh to verify"
    return 0
  fi
  if ! gh auth status >/dev/null 2>&1; then
    info "gh is not signed in — provenance not checked (checksum only)"
    return 0
  fi
  if out=$(gh attestation verify "$1" -R "$REPO" 2>&1); then
    info "provenance OK (gh attestation verify -R ${REPO})"
  else
    info "$out"
    cannot_verify "gh attestation verify could not confirm the build provenance of $(basename "$1")"
  fi
}

# ---- atomic install ---------------------------------------------------------
# $1 src   $2 dest   $3 mode
# Replace each file by copy-to-temp-in-the-same-dir then `mv -f` (atomic
# rename; copying over a running ELF fails with "text file busy").
install_one() {
  src="$1"; dest="$2"; mode="$3"
  dest_dir=$(dirname "$dest")
  mkdir -p "$dest_dir"
  tmpfile="${dest}.tmp.$$"
  rm -f "$tmpfile"
  cp "$src" "$tmpfile" || err "copy to ${dest} failed"
  chmod "$mode" "$tmpfile" || err "chmod ${dest} failed"
  mv -f "$tmpfile" "$dest" || err "move ${dest} failed"
}

# sed-substitute the installer tokens. The shipped templates carry `__USER__`
# in the tmpfiles file; `@PREFIX@`/`@USER@`/`@GROUP@` are supported too so a
# future template using those spellings renders without a code change.
sed_escape() {
  # Escape a replacement for sed s||| in the pattern of render.
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/|/\\|/g' -e 's/&/\\&/g'
}

render() {
  # $1 src   $2 dest
  src="$1"; dest="$2"
  prefix_abs=$(CDPATH= cd "${PREFIX}" 2>/dev/null && pwd) \
    || err "PREFIX ${PREFIX} is not a directory"
  p_esc=$(sed_escape "$prefix_abs")
  u_esc=$(sed_escape "$(id -un)")
  g_esc=$(sed_escape "$(id -gn)")
  tmpfile="${dest}.tmp.$$"
  dest_dir=$(dirname "$dest")
  mkdir -p "$dest_dir"
  sed -e "s|@PREFIX@|${p_esc}|g" \
      -e "s|@USER@|${u_esc}|g" \
      -e "s|@GROUP@|${g_esc}|g" \
      -e "s|__USER__|${u_esc}|g" \
      "$src" > "$tmpfile" || err "render ${dest} failed"
  chmod 0644 "$tmpfile"
  # Fail BEFORE the rendered file reaches its destination: a surviving
  # token aborts the install without leaving a half-rendered file behind.
  # Scoped to this one rendered file, so an unrelated pre-existing file in
  # a shared directory can never abort an install.
  check_no_tokens "$tmpfile"
  mv -f "$tmpfile" "$dest" || err "move ${dest} failed"
}

# Fail if any installer token survives rendering in an installed file.
check_no_tokens() {
  if grep -rE '@[A-Z][A-Z0-9_]*@|__[A-Z][A-Z0-9_]*__' "$1" >/dev/null 2>&1; then
    err "installer token survived rendering in $1 — refusing to install"
  fi
}

# ---- main -------------------------------------------------------------------
main() {
  say "kb-term installer"

  detect_target
  resolve_version

  # Export PREFIX so render() (which reads the global) always sees the
  # resolved install root; both default to ${HOME}/.local.
  PREFIX="${PREFIX:-${HOME}/.local}"
  prefix="$PREFIX"
  bindir="${prefix}/bin"
  xdg_config="${XDG_CONFIG_HOME:-${HOME}/.config}"
  unit_dir="${xdg_config}/systemd/user"

  if [ -n "${KB_BASE_URL:-}" ]; then
    # KB_BASE_URL (a mirror / CI smoke server) overrides the GitHub Releases dir.
    base="${KB_BASE_URL%/}"
  else
    # GitHub release URL: the asset ships on the kb tag (tag `v${VERSION}`).
    base="https://github.com/${REPO}/releases/download/v${VERSION}"
  fi
  archive="kb-term-${VERSION}-${TERM_TARGET}.tar.gz"
  url="${base}/${archive}"

  info "repo    : ${REPO}"
  info "version : ${VERSION}"
  info "target  : ${TERM_TARGET}"
  info "into    : ${prefix}"

  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t kb-term-install) \
    || err "could not create a temp dir"
  # Staged extraction + pre-install validation happens entirely in $tmp, so a
  # rejected archive installs nothing.
  trap 'rm -rf "$tmp"' EXIT INT TERM

  step "downloading ${archive}"
  if ! download "$url" "$tmp/$archive" 2>/dev/null; then
    if [ -n "${KB_BASE_URL:-}" ]; then
      err "download failed — the archive ${archive} is not under ${base}/"
    else
      err "no kb-term archive in release v${VERSION}; pin KB_VERSION to a tag built after kb-term shipped, or install a main build (docs/kb-term-install.md)"
    fi
  fi

  verify_checksum "$tmp/$archive" "${url}.sha256"
  verify_attestation "$tmp/$archive"

  step "extracting and validating"
  tar -xzf "$tmp/$archive" -C "$tmp" || err "extract failed"
  root="${tmp}/kb-term-${VERSION}-${TERM_TARGET}"
  [ -d "$root" ] || err "archive did not contain the expected top-level directory"

  # Require every manifest path before touching the destination: a missing
  # member aborts with nothing installed.
  for member in \
      bin/kb-termd bin/kb-term bin/kb-tap \
      libexec/kb-term/tmux libexec/kb-term/tmux.sha256 \
      share/kb-term/VERSIONS INSTALL \
      share/kb-term/kb-term.conf share/kb-term/omp/overlay.yml \
      share/kb-term/systemd/kb-termd.service \
      share/kb-term/systemd/kb-tap@.service \
      share/kb-term/systemd/kb-term@.service \
      share/kb-term/tmpfiles.d/kb-term.conf; do
    [ -f "${root}/${member}" ] || err "archive is missing required member ${member} — refusing to install"
  done

  # Validate the pinned tmux before touching the destination.
  staged_tmux="${root}/libexec/kb-term/tmux"
  staged_version="$("$staged_tmux" -V 2>/dev/null || echo "")"
  if [ -z "$staged_version" ]; then
    say ""
    say "The bundled tmux binary will not run on this system. Likely causes:"
    say "  - missing shared libraries (libevent, libncurses, libutf8proc)"
    say "  - the system glibc is below 2.39 (the prebuilt bundle requires"
    say "    glibc >= 2.39 — Debian 13+, Ubuntu 24.04+, Fedora 40+)"
    err "the bundled tmux does not run — refusing to install"
  fi
  [ "$staged_version" = "tmux 3.7c" ] \
    || err "bundled tmux reports '${staged_version}', expected 'tmux 3.7c' — refusing to install"
  info "bundled tmux is tmux 3.7c"

  # The in-archive sidecar binds the bundled tmux binary (always verified).
  sidecar_want=$(awk '{print $1}' "${root}/libexec/kb-term/tmux.sha256")
  sidecar_got=$(sha256_of "$staged_tmux")
  [ -n "$sidecar_got" ] || err "no sha256 tool found (need sha256sum or shasum)"
  [ "$sidecar_want" = "$sidecar_got" ] \
    || err "bundled tmux does not match its sidecar (want ${sidecar_want}, got ${sidecar_got})"
  info "bundled tmux matches its sidecar"

  step "installing into ${prefix}"
  mkdir -p "${prefix}"

  # Render the tmpfiles file and the units into the extraction staging dir
  # first (render() fails on a surviving token before the file moves), then
  # install the staged files. A token failure therefore aborts before any
  # file is written to the destination instead of leaving a partial install.
  # The `%h` systemd specifiers stay literal (they resolve to $HOME when
  # the unit runs, not at install time).
  rendered="${tmp}/rendered"
  mkdir -p "${rendered}/tmpfiles.d" "${rendered}/systemd"
  render "${root}/share/kb-term/tmpfiles.d/kb-term.conf" \
         "${rendered}/tmpfiles.d/kb-term.conf"
  for u in kb-termd.service kb-tap@.service kb-term@.service; do
    render "${root}/share/kb-term/systemd/${u}" \
           "${rendered}/systemd/${u}"
  done

  for b in kb-termd kb-term kb-tap; do
    install_one "${root}/bin/${b}" "${bindir}/${b}" 0755
    info "${bindir}/${b}"
  done

  # The pinned tmux goes OFF the PATH, next to no other binary.
  install_one "${staged_tmux}" "${prefix}/libexec/kb-term/tmux" 0755
  install_one "${root}/libexec/kb-term/tmux.sha256" \
              "${prefix}/libexec/kb-term/tmux.sha256" 0644
  info "${prefix}/libexec/kb-term/tmux (tmux 3.7c — intentionally NOT put on PATH)"

  # Share tree: overlay, tmux conf, VERSIONS, INSTALL and the rendered
  # tmpfiles template + units (rendered in staging, so no token survives in
  # any installed copy).
  install_one "${root}/share/kb-term/kb-term.conf" \
              "${prefix}/share/kb-term/kb-term.conf" 0644
  install_one "${root}/share/kb-term/omp/overlay.yml" \
              "${prefix}/share/kb-term/omp/overlay.yml" 0644
  install_one "${root}/share/kb-term/VERSIONS" \
              "${prefix}/share/kb-term/VERSIONS" 0644
  install_one "${root}/INSTALL" \
              "${prefix}/share/kb-term/INSTALL" 0644
  install_one "${rendered}/tmpfiles.d/kb-term.conf" \
              "${prefix}/share/kb-term/tmpfiles.d/kb-term.conf" 0644

  # Units go to both the share tree and the live systemd user dir.
  for u in kb-termd.service kb-tap@.service kb-term@.service; do
    install_one "${rendered}/systemd/${u}" \
                "${prefix}/share/kb-term/systemd/${u}" 0644
    install_one "${rendered}/systemd/${u}" "${unit_dir}/${u}" 0644
    info "${unit_dir}/${u}"
  done
  info "${prefix}/share/kb-term (units, tmpfiles.d, omp overlay, kb-term.conf)"

  step "reloading systemd"
  if [ "${KB_TERM_SKIP_SYSTEMD:-}" = "1" ]; then
    info "KB_TERM_SKIP_SYSTEMD=1 — skipping systemctl --user daemon-reload"
  elif have systemctl; then
    if systemctl --user daemon-reload >/dev/null 2>&1; then
      info "systemctl --user daemon-reload ok"
    else
      info "systemctl --user daemon-reload failed — run it manually to pick up the units"
    fi
  else
    info "systemctl not found — units are in place for the next daemon-reload"
  fi

  say ""
  say "do not restart kb-term@* units to pick up an upgrade; that ends live sessions. Restart kb-termd only and let new sessions use the new tmux."
  say ""
  say "Done. The operator (you) runs these host steps, in order:"
  say "  1. sudo install -m 0644 ${prefix}/share/kb-term/tmpfiles.d/kb-term.conf /etc/tmpfiles.d/kb-term.conf && sudo systemd-tmpfiles --create /etc/tmpfiles.d/kb-term.conf"
  say "  2. systemctl --user enable --now kb-termd.service"
  say "  3. loginctl enable-linger \"\$(id -un)\"   # without linger the user manager and its sessions end at logout"
  say "  4. kb-term doctor"
  say "  5. add a runner dir bind mount to the kb container:"
  say "       - /run/kb-term/runner:/run/kb-term/runner"
  say "     then set [terminal] enabled = true in the host copy of kb.toml"
  say ""

  # PATH hint if the bin dir isn't reachable.
  case ":${PATH}:" in
    *":${bindir}:"*) : ;;
    *)
      say "note: ${bindir} is not on your PATH. Add it, e.g.:"
      say "  export PATH=\"${bindir}:\$PATH\""
      say ""
      ;;
  esac

  say "Docs: https://github.com/${REPO}/blob/main/docs/kb-term-install.md"
}

main "$@"
