# syntax=docker/dockerfile:1.7
#
# REFERENCE image for kb-lip + ruby-lsp (design-lip.md's "can also live in a
# different docker image; the end user must not have to look elsewhere").
# This is Track L3's proof that the provider protocol supports a
# per-language sidecar image, not a load-bearing artifact: NOTHING in CI
# builds this file (see justfile — no `ci-*` recipe references it), so it is
# not covered by the pinned-digest discipline the root Dockerfile documents
# (that file's own top-of-file comment explains why prod images pin
# `FROM ... @sha256:...`). An operator adopting this for real should apply
# the same digest-pinning to both FROM lines below before running it
# unattended.
#
# Build (from the repo root, so the builder stage can see the whole
# workspace — kb-lip is a Cargo workspace member, so `cargo build -p kb-lip`
# needs the root Cargo.toml/Cargo.lock and every crate's Cargo.toml to
# resolve, even though only kb-lip's own source ends up in the final image):
#
#   docker build -f Dockerfile.ruby -t kb-lip-ruby .
#
# Run (mount the target Ruby repo read-only, point workspace_root at the
# in-container mount path, publish the lip/1 port to loopback only — see
# providers/README.md for the "how do I know it's actually protected"
# discussion):
#
#   docker run --rm \
#     -v /path/to/my-rails-app:/workspace:ro \
#     -p 127.0.0.1:4841:4841 \
#     kb-lip-ruby
#
# The container's own providers/ruby-lsp.toml (baked in below, workspace_root
# rewritten to /workspace) is the config ENTRYPOINT runs with — override by
# bind-mounting a different config over /app/config.toml.

# --- Builder: compile kb-lip -------------------------------------------
FROM rust:1.96-trixie AS builder

WORKDIR /build
COPY . .
RUN cargo build --release -p kb-lip

# --- Runtime: ruby + ruby-lsp + the compiled kb-lip binary --------------
FROM ruby:3.4-slim-trixie AS runtime

# git: ruby-lsp's "composed bundle" mode (see providers/ruby-lsp.toml's
# prerequisite comments) shells out to `bundle install`, which needs git for
# any git-sourced Gemfile entries. build-essential + libyaml-dev: enough to
# compile the common native-extension gems (nokogiri, psych, etc) a target
# repo's Gemfile.lock is likely to pull in — trimmed relative to a full dev
# image on purpose (this is a reference, not a general-purpose Ruby build
# environment; a repo needing more system libs to bundle install will need
# a thicker image, which is exactly why this is "a" reference image, not
# "the" one).
RUN apt-get update && apt-get install -y --no-install-recommends \
        git \
        build-essential \
        libyaml-dev \
    && rm -rf /var/lib/apt/lists/*

RUN gem install ruby-lsp --no-document

COPY --from=builder /build/target/release/kb-lip /usr/local/bin/kb-lip

WORKDIR /app
COPY providers/ruby-lsp.toml /app/config.toml
# The baked-in config's workspace_root placeholder is meaningless outside
# this image (it names a path on the HOST that built the image) — rewrite it
# to the container's expected mount point, /workspace, so the shipped
# default just works against `docker run -v <repo>:/workspace:ro`.
RUN sed -i 's|^workspace_root = .*|workspace_root = "/workspace"|' /app/config.toml

EXPOSE 4841

ENTRYPOINT ["kb-lip", "--config", "/app/config.toml"]
