//! Shared helpers for kb-code-server integration tests.
//!
//! Only byte-identical copies live here. Divergent `boot*` / `fixture_repo`
//! / `wait_for_indexed` implementations stay next to the tests that own them.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git -C {} {:?} failed: {}",
        dir.display(),
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
}
