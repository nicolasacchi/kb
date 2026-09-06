//! Build-stamp: bake the git commit + `git describe` version into
//! `KB_GIT_SHA` / `KB_GIT_DESCRIBE`, re-run when HEAD moves.
//!
//! PF-B1 — this is the union of the two build scripts that used to live in
//! kb-server and kb-cli, moved into a leaf crate so the per-commit re-stamp
//! recompiles ~20 lines instead of the kb-server lib and every crate
//! downstream of it (kb-cli, kb-code-server, kb-code-cli, all kb-server
//! test binaries). Consumers read `kb_buildstamp::{BUILD_SHA, VERSION}`;
//! kb-server itself never links this crate — its bins inject the values
//! at runtime via `kb_server::set_build_stamp` (only BIN targets may
//! depend on a per-commit-changing crate, and cargo cannot scope a
//! dependency to one target of a package).
//!
//! The SPA bundle carries the same sha stamp (see `web/vite.config.ts`);
//! the shell compares the two and warns when they diverge. A stale daemon
//! serving a freshly-rebuilt SPA is the drift that white-screens the app
//! when the HTTP contract has changed under it — this makes that loud.
//! Absent git the sha is `unknown` and the comparison silently no-ops.

use std::process::Command;

fn main() {
    // An explicit `KB_BUILD_SHA` override wins over the git probe — the
    // Docker build sets it from a `--build-arg` because `.git` is excluded
    // from the build context. Falls back to git, then to "unknown".
    let stamp = match std::env::var("KB_BUILD_SHA") {
        Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            let sha = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
            let dirty = match git(&["status", "--porcelain"]) {
                Some(s) if !s.is_empty() => "-dirty",
                _ => "",
            };
            format!("{sha}{dirty}")
        }
    };
    println!("cargo:rustc-env=KB_GIT_SHA={stamp}");
    println!("cargo:rerun-if-env-changed=KB_BUILD_SHA");

    // Human-facing version: `git describe` (nearest tag + commits-since
    // + short sha, e.g. `v0.13-42-g9fd65fd`, or exactly `v0.13` on the tag).
    // The workspace Cargo version is pinned at 0.0.0 (real versioning lives
    // in git tags), so CARGO_PKG_VERSION reads 0.0.0 forever; this stamp
    // backs `/api/identity.version` and `kb --version`. `KB_BUILD_VERSION`
    // overrides for builds without `.git` in context (Docker), falling back
    // to `0.0.0-dev`.
    // `--match v[0-9]*` pins describe to the release-tag series — without it
    // the nearest tag by graph distance wins, and a side-series tag
    // (kb-code-v3.4) hijacks the stamp into `kb-code-v3.4-30-g…` (the SPA
    // settings badge then renders `vkb-code-…` and its `^v\d` pin breaks).
    let version = match std::env::var("KB_BUILD_VERSION") {
        Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => git(&[
            "describe", "--tags", "--match", "v[0-9]*", "--always", "--dirty",
        ])
        .unwrap_or_else(|| "0.0.0-dev".into()),
    };
    // Tags are v-prefixed (v0.13) but version strings are conventionally
    // bare — the SPA settings badge renders `v{version}` and would show
    // "vv0.13…" otherwise.
    let version = version.strip_prefix('v').unwrap_or(&version);
    println!("cargo:rustc-env=KB_GIT_DESCRIBE={version}");
    println!("cargo:rerun-if-env-changed=KB_BUILD_VERSION");

    // Re-run when HEAD or the checked-out ref moves so the stamp stays
    // current without a manual `cargo clean`.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        let git_dir = std::path::Path::new(&git_dir);
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join("packed-refs").display()
        );
        if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) {
            if let Some(r) = head.strip_prefix("ref:") {
                println!(
                    "cargo:rerun-if-changed={}",
                    git_dir.join(r.trim()).display()
                );
            }
        }
    }
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
