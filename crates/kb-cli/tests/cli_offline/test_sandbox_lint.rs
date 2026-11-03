//! AH1 - the ratchet that keeps tests from resolving kb state out of the
//! developer's real HOME.
//!
//! # This lint's contract, stated plainly
//!
//! It is a SOURCE SCAN with allow-lists, not a proof. It makes the surface it
//! guards impossible to grow silently: a new test that forgets the sandbox
//! fails here with a message saying what to do. It cannot see everything.
//! Known false negatives: out-of-line `mod x;` test files that live under
//! `src/` (the gate is only seen on the `mod` line), items whose body is cut
//! short by column-0 content inside a string literal, and
//! `#[cfg(all(test, ...))]` gates.
//!
//! Three rules, one test each, plus a self-test for the scanner:
//!
//! 1. no `KbPaths` constructor that reads the real XDG/`KB_HOME` dirs in test
//!    code (use `KbPaths::rooted_at`);
//! 2. every hook test script that can reach a real `kb` sources
//!    `lib-sandbox.sh` and calls `kb_test_sandbox` before it does;
//! 3. kb-cli tests spawn bash only through `hook_shell.rs::hook_command` (and
//!    the two pinned `adapter_grok.rs` sites).
//!
//! This file exempts itself from every scan by path and builds its needles
//! with `concat!`, so it never matches itself.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize the repo root")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_self(root: &Path, p: &Path) -> bool {
    rel(root, p) == "crates/kb-cli/tests/cli_offline/test_sandbox_lint.rs"
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

/// True for lines inside a `#[cfg(test)]`-gated item. See the module doc for
/// what it can miss; rustfmt guarantees a gated item's closing brace sits at
/// its attribute's indentation, so it errs toward missing test code, never
/// toward flagging production code.
fn cfg_test_mask(src: &str) -> Vec<bool> {
    let lines: Vec<&str> = src.lines().collect();
    let n = lines.len();
    let mut mask = vec![false; n];
    let mut i = 0;
    while i < n {
        if lines[i].trim() != "#[cfg(test)]" {
            i += 1;
            continue;
        }
        let nind = indent(lines[i]);
        let mut j = i + 1;
        while j < n {
            let t = lines[j].trim();
            if t.is_empty() || t.starts_with("#[") || t.starts_with("//") {
                j += 1;
            } else {
                break;
            }
        }
        if j >= n {
            mask[i..n].fill(true);
            break;
        }
        let first = lines[j].trim_end();
        let end = if first.ends_with(';') || first.ends_with(',') || first.ends_with('}') {
            j
        } else {
            let mut k = j + 1;
            while k < n {
                let t = lines[k].trim();
                if !t.is_empty() {
                    let ind = indent(lines[k]);
                    if ind < nind {
                        break;
                    }
                    if ind == nind
                        && !(t.starts_with(')')
                            || t.starts_with('>')
                            || t.starts_with('{')
                            || t.starts_with("where"))
                    {
                        break;
                    }
                }
                k += 1;
            }
            if k < n && lines[k].trim_start().starts_with('}') {
                k
            } else {
                k.saturating_sub(1).max(j)
            }
        };
        let end = end.min(n - 1);
        mask[i..=end].fill(true);
        i = end + 1;
    }
    mask
}

fn is_rust_comment(l: &str) -> bool {
    l.trim_start().starts_with("//")
}

fn is_bash_comment(l: &str) -> bool {
    l.trim_start().starts_with('#')
}

#[test]
fn cfg_test_scanner_marks_only_gated_items() {
    let src = r##"fn prod_a() {
    let _ = "PROD";
}

#[cfg(test)]
mod out_of_line;

fn prod_b() {
    let _ = "PROD";
}

#[cfg(test)]
fn helper() -> u8 {
    1
}

fn prod_c() {
    let _ = "PROD";
}

#[cfg(test)]
fn helper2(
    a: u8,
) -> u8 {
    let _ = "FLAG";
    a
}

struct S;

impl S {
    #[cfg(test)]
    fn t(&self) {
        let _ = "FLAG";
    }

    fn prod_d(&self) {
        let _ = "PROD";
    }
}

#[cfg(test)]
mod tests {
    fn x() {
        let _ = "FLAG";
    }
}

fn prod_e() {
    let _ = "PROD";
}
"##;
    let mask = cfg_test_mask(src);
    let lines: Vec<&str> = src.lines().collect();
    let flagged: BTreeSet<usize> = (0..lines.len())
        .filter(|&i| mask[i] && lines[i].contains("FLAG"))
        .collect();
    let expected: BTreeSet<usize> = (0..lines.len())
        .filter(|&i| lines[i].contains("FLAG"))
        .collect();
    let prod_flagged: Vec<usize> = (0..lines.len())
        .filter(|&i| mask[i] && lines[i].contains("PROD"))
        .collect();
    assert_eq!(
        expected.len(),
        3,
        "the synthetic source holds 3 gated needles"
    );
    assert_eq!(
        lines.iter().filter(|l| l.contains("PROD")).count(),
        5,
        "the synthetic source holds 5 production needles"
    );
    assert_eq!(flagged, expected, "every gated needle line must be flagged");
    assert!(
        prod_flagged.is_empty(),
        "production lines flagged as test code: {prod_flagged:?}"
    );
}

#[test]
fn no_kbpaths_new_in_test_code() {
    let root = repo_root();
    let needle = concat!("KbPaths", "::new(");
    let mut files = Vec::new();
    let crates = root.join("crates");
    for entry in std::fs::read_dir(&crates).expect("read crates/").flatten() {
        rust_files(&entry.path().join("src"), &mut files);
        rust_files(&entry.path().join("tests"), &mut files);
    }
    let mut scanned = 0usize;
    let mut gated_regions = 0usize;
    let mut offenders = Vec::new();
    for f in &files {
        if is_self(&root, f) || rel(&root, f) == "crates/kb-core/src/paths.rs" {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        scanned += 1;
        let r = rel(&root, f);
        let in_tests_dir = r.contains("/tests/");
        let mask = cfg_test_mask(&src);
        gated_regions += src.lines().filter(|l| l.trim() == "#[cfg(test)]").count();
        for (i, l) in src.lines().enumerate() {
            if is_rust_comment(l) || !l.contains(needle) {
                continue;
            }
            if in_tests_dir || mask[i] {
                offenders.push(format!("{r}:{}", i + 1));
            }
        }
    }
    eprintln!("AH1 lint witness: files scanned={scanned}, #[cfg(test)] gates={gated_regions}");
    assert!(scanned >= 100000, "scan looks vacuous: only {scanned} files");
    assert!(
        gated_regions >= 100000,
        "scan looks vacuous: only {gated_regions} #[cfg(test)] gates"
    );
    assert!(
        offenders.is_empty(),
        "KbPaths::new resolves the developer's real XDG/KB_HOME dirs. In test code use \
         `KbPaths::rooted_at(tempdir, name)` in-process, or set `KB_HOME` on the spawned \
         child. Offenders: {offenders:?}"
    );
}

/// Hook test scripts that set up their own sandbox (file, reason). The rule is
/// checked, not trusted: see `real_kb_hook_scripts_source_the_sandbox`.
const SELF_SANDBOXED: &[(&str, &str)] = &[
    (
        "test-capture-omp-segments.sh",
        "exports its own HOME/KB_CAPTURE_SPOOL/KB_CAPTURE_LOCKS under TMPROOT",
    ),
    (
        "test-capture-omp-segments-scale.sh",
        "exports its own HOME/KB_CAPTURE_SPOOL/KB_CAPTURE_LOCKS under TMPROOT",
    ),
    (
        "test-capture-replay-order.sh",
        "exports its own HOME/KB_CAPTURE_SPOOL/KB_CAPTURE_LOCKS under TMPROOT",
    ),
];

const SELF_SANDBOX_MARKERS: &[&str] = &[
    "HOME=\"$TMPROOT/home\"",
    "unset KB_CACHE_DIR KB_STATE_DIR KB_CONFIG_DIR",
    "KB_CAPTURE_SPOOL=",
    "KB_CAPTURE_LOCKS=",
];

#[test]
fn real_kb_hook_scripts_source_the_sandbox() {
    let root = repo_root();
    let dir = root.join("plugins/kb-memory/hooks/tests");
    let mut failures = Vec::new();
    let mut triggered = BTreeSet::new();
    let mut seen_allowed = BTreeSet::new();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("read the hook tests dir")
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
        .collect();
    names.sort();
    for name in &names {
        let src = std::fs::read_to_string(dir.join(name)).expect("read a hook test");
        let code: Vec<&str> = src.lines().filter(|l| !is_bash_comment(l)).collect();
        let triggers = [
            "KB_BIN_DIR",
            "REAL_PATH",
            "--replay-spool",
            "kb sessions capture",
        ];
        let Some(trigger) = code
            .iter()
            .position(|l| triggers.iter().any(|t| l.contains(t)))
        else {
            continue;
        };
        triggered.insert(name.clone());
        let lib = code.iter().position(|l| l.contains("lib-sandbox.sh"));
        let call = code
            .iter()
            .position(|l| l.trim_start().starts_with("kb_test_sandbox "));
        let sourced_first = matches!((lib, call), (Some(a), Some(b)) if a < b && b < trigger);
        match SELF_SANDBOXED.iter().find(|(f, _)| f == name) {
            Some((_, reason)) => {
                seen_allowed.insert(name.clone());
                if lib.is_some() {
                    failures.push(format!(
                        "{name} is allow-listed ({reason}) but also sources lib-sandbox.sh: \
                         drop it from SELF_SANDBOXED"
                    ));
                }
                for m in SELF_SANDBOX_MARKERS {
                    if !code.iter().any(|l| l.contains(m)) {
                        failures.push(format!(
                            "{name} is allow-listed as self-sandboxed but lacks `{m}`"
                        ));
                    }
                }
            }
            None if !sourced_first => failures.push(format!(
                "{name} can reach a real kb but does not source lib-sandbox.sh and call \
                 `kb_test_sandbox \"$TMPROOT\" || exit 1` right after its TMPROOT trap, before \
                 its first KB_BIN_DIR/REAL_PATH/kb-sessions-capture line"
            )),
            None => {}
        }
    }
    for (f, _) in SELF_SANDBOXED {
        if !seen_allowed.contains(*f) {
            failures.push(format!(
                "SELF_SANDBOXED lists {f} but it is missing or no longer reaches a real kb: \
                 remove the entry"
            ));
        }
    }
    eprintln!(
        "AH1 lint witness: triggered scripts={}: {triggered:?}",
        triggered.len()
    );
    assert!(
        triggered.len() >= 100000,
        "scan looks vacuous: only {} triggered scripts",
        triggered.len()
    );
    for must in ["test-capture-omp.sh", "test-lib-sandbox.sh"] {
        assert!(
            triggered.contains(must),
            "{must} must be in the triggered set"
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn kb_cli_bash_spawns_are_pinned() {
    let root = repo_root();
    let needle = concat!("Command::new(", "\"bash\"", ")");
    // adapter_grok.rs: sets its own per-test HOME/KB_CAPTURE_SPOOL; the
    // GROKCLAUDE_FAKE spawn exits before resolving any path.
    let allowed: &[(&str, usize)] = &[
        ("cli_offline/hook_shell.rs", 1),
        ("cli_offline/adapter_grok.rs", 2),
    ];
    let mut files = Vec::new();
    rust_files(&root.join("crates/kb-cli/tests"), &mut files);
    let mut failures = Vec::new();
    let mut counted = BTreeSet::new();
    for f in &files {
        if is_self(&root, f) {
            continue;
        }
        let r = rel(&root, f);
        let r = r
            .strip_prefix("crates/kb-cli/tests/")
            .unwrap_or(&r)
            .to_string();
        let src = std::fs::read_to_string(f).expect("read a kb-cli test");
        let n = src
            .lines()
            .filter(|l| !is_rust_comment(l) && l.contains(needle))
            .count();
        let want = allowed.iter().find(|(a, _)| *a == r).map_or(0, |(_, c)| *c);
        if n != want {
            failures.push(format!(
                "{r}: {n} bash spawn(s), expected {want}. Route a new hook script through \
                 hook_shell.rs::hook_command (throwaway HOME/XDG, KB_*/CLAUDE_* stripped)"
            ));
        }
        counted.insert(r);
    }
    for (a, _) in allowed {
        assert!(
            counted.contains(*a),
            "{a} was not scanned: the pin is vacuous"
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
