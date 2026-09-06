#!/bin/sh
# kb installer — fetch the latest `kb` + `kb-embedder` release binaries.
#
#   curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install.sh | sh
#
# or from a checkout:  sh scripts/install.sh
#
# Environment overrides:
#   PREFIX       install root; binaries land in $PREFIX/bin.  Default: ~/.local
#   KB_VERSION   pin a release, e.g. "0.24" or "v0.24".        Default: latest
#   KB_BASE_URL  fetch the release archive from this directory URL instead of
#                GitHub Releases (air-gapped mirror / internal artifact store /
#                CI smoke test). Requires KB_VERSION (a bare mirror has no
#                "latest" concept). The archive + its .sha256 must live directly
#                under this URL, named exactly as the GitHub assets are.
#
# SAFETY: every action runs inside main(), which is called ONLY on the final
# line of this file. A truncated download (curl|sh cut mid-stream) therefore
# defines functions but executes nothing — no half-run install.
#
# POSIX sh — no bashisms, no `local`. Verified with `sh -n` + shellcheck -s sh.
set -eu

REPO="nicolasacchi/kb"

# ---- output -----------------------------------------------------------------
say()  { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
err()  { printf 'kb-install: error: %s\n' "$*" >&2; exit 1; }

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

# ---- platform detection -----------------------------------------------------
# Sets TARGET to a rust target triple matching a published release asset.
detect_target() {
  os=$(uname -s 2>/dev/null || echo unknown)
  arch=$(uname -m 2>/dev/null || echo unknown)

  case "$os" in
    Linux)  os_part="unknown-linux-gnu" ;;
    *) err "unsupported OS '$os' — build from source: https://github.com/$REPO" ;;
  esac
  case "$arch" in
    x86_64|amd64)  arch_part="x86_64" ;;
    aarch64|arm64) arch_part="aarch64" ;;
    *) err "unsupported architecture '$arch' — build from source: https://github.com/$REPO" ;;
  esac

  # Only the triples release.yml actually publishes.
  case "${arch_part}-${os_part}" in
    x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu)
      TARGET="${arch_part}-${os_part}" ;;
    *)
      err "no prebuilt binary for ${arch_part}-${os_part} — build from source: https://github.com/$REPO" ;;
  esac
}

# ---- version resolution -----------------------------------------------------
# Sets TAG (e.g. v0.24) and VERSION (e.g. 0.24).
resolve_version() {
  if [ -n "${KB_VERSION:-}" ]; then
    VERSION="${KB_VERSION#v}"
    TAG="v${VERSION}"
    return 0
  fi
  # A bare mirror has no "latest" endpoint — a pinned KB_VERSION is required.
  [ -z "${KB_BASE_URL:-}" ] || err "KB_BASE_URL requires KB_VERSION (e.g. KB_VERSION=0.24)"
  # Latest release tag via the GitHub API, parsed without jq. `sed` exits 0 on
  # empty input, so the pipeline never trips `set -e`; the emptiness check does.
  tag=$(fetch "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
        | grep -m1 '"tag_name"' \
        | sed -e 's/.*"tag_name"[^"]*"//' -e 's/".*//') || tag=""
  [ -n "$tag" ] || err "could not resolve the latest release (is one published yet? try KB_VERSION=…)"
  TAG="$tag"
  VERSION="${TAG#v}"
}

# ---- checksum (best-effort) -------------------------------------------------
sha256_of() {
  if have sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif have shasum;    then shasum -a 256 "$1" | awk '{print $1}'
  else echo ""; fi
}

verify_checksum() {
  # $1 file   $2 url of the .sha256 sidecar
  want=$(fetch "$2" 2>/dev/null | awk '{print $1}') || want=""
  [ -n "$want" ] || { info "checksum sidecar unavailable — skipping verification"; return 0; }
  got=$(sha256_of "$1")
  [ -n "$got" ] || { info "no sha256 tool found — skipping verification"; return 0; }
  [ "$want" = "$got" ] || err "checksum mismatch (want $want, got $got)"
  info "checksum OK"
}

# ---- main -------------------------------------------------------------------
main() {
  say "kb installer"

  detect_target
  resolve_version

  bindir="${PREFIX:-$HOME/.local}/bin"
  # KB_BASE_URL (a mirror / CI smoke server) overrides the GitHub Releases dir;
  # trailing slash is trimmed so "${base}/${archive}" is well-formed either way.
  base="${KB_BASE_URL:-https://github.com/${REPO}/releases/download/${TAG}}"
  base="${base%/}"
  archive="kb-${VERSION}-${TARGET}.tar.gz"
  url="${base}/${archive}"

  info "repo    : ${REPO}"
  info "version : ${TAG}"
  info "target  : ${TARGET}"
  info "into    : ${bindir}"

  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t kb-install) \
    || err "could not create a temp dir"
  trap 'rm -rf "$tmp"' EXIT INT TERM

  step "downloading ${archive}"
  download "$url" "$tmp/$archive" \
    || err "download failed — does the release asset ${archive} exist for ${TAG}?"

  verify_checksum "$tmp/$archive" "${url}.sha256"

  step "extracting"
  tar -xzf "$tmp/$archive" -C "$tmp" || err "extract failed"

  # Layout-agnostic: find the two binaries wherever they landed in the archive.
  kb_bin=$(find "$tmp" -type f -name kb 2>/dev/null | head -n1)
  emb_bin=$(find "$tmp" -type f -name kb-embedder 2>/dev/null | head -n1)
  [ -n "$kb_bin" ]  || err "archive did not contain a 'kb' binary"
  [ -n "$emb_bin" ] || err "archive did not contain a 'kb-embedder' binary"

  step "installing into ${bindir}"
  mkdir -p "$bindir" || err "could not create ${bindir}"
  # kb finds kb-embedder as a SIBLING (invariant #26) — both must land together.
  cp "$kb_bin"  "$bindir/kb"          || err "install kb failed"
  cp "$emb_bin" "$bindir/kb-embedder" || err "install kb-embedder failed"
  chmod 0755 "$bindir/kb" "$bindir/kb-embedder"
  info "${bindir}/kb"
  info "${bindir}/kb-embedder"

  step "verifying"
  if "$bindir/kb" --version >/dev/null 2>&1; then
    info "$("$bindir/kb" --version 2>/dev/null)"
  else
    # Almost always a libc mismatch: the prebuilt bundle needs glibc >= 2.39
    # (the statically-bundled ONNX Runtime in kb-embedder forces it). Point the
    # user at the two escape hatches instead of a bare "it failed".
    say ""
    say "kb was placed in ${bindir}, but it will not run on this system."
    say "The prebuilt binaries require glibc >= 2.39 (Debian 13+, Ubuntu 24.04+,"
    say "Fedora 40+) and do not run on older glibc (Debian 12, Ubuntu 22.04,"
    say "RHEL 9) or musl (Alpine). Use the self-contained container:"
    say "  docker run -p 4000:4000 ghcr.io/${REPO}:${VERSION}"
    say "or build from source: https://github.com/${REPO}#install"
    err "installed binaries are incompatible with this system's libc"
  fi

  # PATH hint if the bin dir isn't reachable.
  case ":${PATH}:" in
    *":${bindir}:"*) : ;;
    *)
      say ""
      say "note: ${bindir} is not on your PATH. Add it, e.g.:"
      say "  export PATH=\"${bindir}:\$PATH\""
      ;;
  esac

  say ""
  say "Done. Next:"
  say "  1. Start a daemon:  kb daemon      (→ http://127.0.0.1:4000/)"
  say "  2. Or, inside Claude Code, bootstrap memory + sessions corpora:  /kb-setup"
  say ""
  say "The web UI is not bundled in the binary archive — build it once with"
  say "'just ci-spa', or run the all-in-one image: ghcr.io/${REPO}:${VERSION}"
  say "Docs: https://github.com/${REPO}#readme"
}

main "$@"
