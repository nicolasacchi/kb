# syntax=docker/dockerfile:1.7
#
# REFERENCE image for kb-lip + gopls (design-lip.md's "can also live in a
# different docker image; the end user must not have to look elsewhere").
# Mirrors Dockerfile.ruby's shape — see that file's own top-of-file comment
# for the full rationale. As with Dockerfile.ruby: NOTHING in CI builds this
# file (see justfile — no `ci-*` recipe references it), so it is not
# covered by the pinned-digest discipline the root Dockerfile documents. An
# operator adopting this for real should apply the same digest-pinning to
# all three FROM lines below before running it unattended.
#
# THREE build stages, not two — the smallest-honest path for Go: rather
# than basing runtime on a full `golang:...` image (which drags in the
# entire Go toolchain just to run ONE already-compiled LSP binary, the same
# tradeoff Dockerfile.rust's own comment explains for rust-analyzer), gopls
# is built in its OWN builder stage and only the resulting binary is copied
# into the slim runtime.
#
# Build (from the repo root, so the kb-lip builder stage can see the whole
# workspace — kb-lip is a Cargo workspace member, so `cargo build -p kb-lip`
# needs the root Cargo.toml/Cargo.lock and every crate's Cargo.toml to
# resolve, even though only kb-lip's own source ends up in the final image):
#
#   docker build -f Dockerfile.go -t kb-lip-go .
#
# Run (mount the target Go repo/module read-only, point workspace_root at
# the in-container mount path, publish the lip/1 port to loopback only —
# see providers/README.md for the "how do I know it's actually protected"
# discussion):
#
#   docker run --rm \
#     -v /path/to/my-go-app:/workspace:ro \
#     -p 127.0.0.1:4851:4851 \
#     kb-lip-go
#
# The container's own providers/gopls.toml (baked in below, workspace_root
# rewritten to /workspace) is the config ENTRYPOINT runs with — override by
# bind-mounting a different config over /app/config.toml.

# --- Builder 1: compile kb-lip ------------------------------------------
FROM rust:1.96-trixie AS builder

WORKDIR /build
COPY . .
RUN cargo build --release -p kb-lip

# --- Builder 2: compile gopls --------------------------------------------
FROM golang:1.25-trixie AS gopls-builder

RUN go install golang.org/x/tools/gopls@latest

# --- Runtime: the compiled kb-lip + the compiled gopls -------------------
FROM debian:trixie-slim AS runtime

COPY --from=builder /build/target/release/kb-lip /usr/local/bin/kb-lip
COPY --from=gopls-builder /go/bin/gopls /usr/local/bin/gopls

WORKDIR /app
COPY providers/gopls.toml /app/config.toml
# The baked-in config's workspace_root placeholder is meaningless outside
# this image (it names a path on the HOST that built the image) — rewrite it
# to the container's expected mount point, /workspace, so the shipped
# default just works against `docker run -v <repo>:/workspace:ro`.
RUN sed -i 's|^workspace_root = .*|workspace_root = "/workspace"|' /app/config.toml

EXPOSE 4851

ENTRYPOINT ["kb-lip", "--config", "/app/config.toml"]
