//! Inventory-as-code for `kb_core::posture` (v0.44 X5, F1 carry).
//!
//! The `posture` module doc used to carry the list of reviewed `#[default]`
//! enums as PROSE, which nothing checks: a new defaulted enum that gates
//! access could land with a permissive first variant and no one would be
//! asked the question. This test walks every `crates/*/src/**/*.rs`, finds
//! each enum that carries a `#[default]` variant, and requires it to be in
//! the table below -- either `Posture` (an `impl Posture for` exists AND a
//! test calls `assert_default_is_restrictive` for it) or `Reviewed` with the
//! reason it is not an access-control axis. The table is exact in both
//! directions: a stale entry (the enum is gone) fails too, so the inventory
//! cannot rot into a list of things that no longer exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

enum Class {
    /// Must implement `Posture` and be pinned by `assert_default_is_restrictive`.
    Posture,
    /// Not an access-control axis (or pinned elsewhere); the reason is the audit trail.
    Reviewed(&'static str),
}
use Class::{Posture, Reviewed};

const INVENTORY: &[(&str, Class)] = &[
    ("Visibility", Posture),
    (
        "Origin",
        Reviewed("slate: default is Agent; pinned in slate's tests as 'never Human'"),
    ),
    (
        "CredentialPin",
        Reviewed("selector; permissive modes are gated by allow_inherited_credentials=false"),
    ),
    ("ForgeKind", Reviewed("selector")),
    ("BranchSort", Reviewed("selector")),
    (
        "View",
        Reviewed("kb-code history view selector; read-only filter"),
    ),
    ("SortKey", Reviewed("docs_query selector")),
    ("SortDir", Reviewed("docs_query selector")),
    ("GroupKey", Reviewed("docs_query selector")),
    ("Projection", Reviewed("docs_query selector")),
    ("PositionSpec", Reviewed("lists: insertion position")),
    ("Patch", Reviewed("lists: tri-state field patch")),
    ("DecayPolicy", Reviewed("memory decay selector")),
    ("LocBucket", Reviewed("parser: size bucket")),
    (
        "LinksMode",
        Reviewed("share: dangling-link handling selector"),
    ),
    ("Mode", Reviewed("slate: digest rendering mode")),
    ("VersionsMode", Reviewed("vcs: version source selector")),
    ("WatchMode", Reviewed("watcher: notify vs poll")),
    ("AtlasLayoutChoice", Reviewed("atlas layout selector")),
];

fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The source with `//` line comments and `/* */` blocks removed (comment
/// markers inside a string literal are text). A mention of `impl Posture for
/// X` or `assert_default_is_restrictive::<X>()` in a comment or a string must
/// not satisfy the inventory, so the item checks below run on this.
fn code_only(text: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in text.lines() {
        let b: Vec<char> = line.chars().collect();
        let (mut i, mut in_str) = (0, false);
        let mut kept = String::new();
        while i < b.len() {
            let two: String = b[i..(i + 2).min(b.len())].iter().collect();
            if in_block {
                if two == "*/" {
                    in_block = false;
                    i += 2;
                } else {
                    i += 1;
                }
            } else if in_str {
                // A string's contents are blanked: they can never be an item.
                if b[i] == '\\' {
                    i += 2;
                    continue;
                }
                if b[i] == '"' {
                    in_str = false;
                    kept.push('"');
                }
                i += 1;
            } else if two == "//" {
                break;
            } else if two == "/*" {
                in_block = true;
                i += 2;
            } else {
                if b[i] == '"' {
                    in_str = true;
                }
                kept.push(b[i]);
                i += 1;
            }
        }
        out.push_str(&kept);
        out.push('\n');
    }
    out
}

/// Last path segment of a type token, ignoring generics and a trailing `{`.
fn type_name_of(tok: &str) -> &str {
    let tok = tok.trim_end_matches('{');
    let tok = tok.split('<').next().unwrap_or(tok);
    tok.rsplit("::").next().unwrap_or(tok)
}

/// Does `code` contain the ITEM `impl [path::]Posture for [path::]name`?
fn implements_posture(code: &str, name: &str) -> bool {
    code.lines().any(|l| {
        let t: Vec<&str> = l.split_whitespace().collect();
        t.windows(4).any(|w| {
            w[0] == "impl"
                && w[1].rsplit("::").next() == Some("Posture")
                && w[2] == "for"
                && type_name_of(w[3]) == name
        })
    })
}

/// Does `code` contain a CALL `assert_default_is_restrictive::<[path::]name>(`?
fn pins_default(code: &str, name: &str) -> bool {
    const CALL: &str = "assert_default_is_restrictive::<";
    code.match_indices(CALL).any(|(i, _)| {
        let rest = &code[i + CALL.len()..];
        rest.split_once(">(")
            .is_some_and(|(ty, _)| ty.rsplit("::").next() == Some(name))
    })
}

/// `(enum name -> files)` for every enum with a `#[default]` variant, plus
/// the concatenated source of all scanned files (for the impl/test search).
fn scan() -> (BTreeMap<String, Vec<String>>, String) {
    let mut files = Vec::new();
    for c in std::fs::read_dir(workspace_crates()).unwrap().flatten() {
        rs_files(&c.path().join("src"), &mut files);
    }
    files.sort();
    let enum_re = |l: &str| -> Option<String> {
        if l.trim_start().starts_with("//") {
            return None;
        }
        let i = l.find("enum ")?;
        let before = &l[..i];
        if !(before.is_empty() || before.ends_with(' ') || before.ends_with('(')) {
            return None;
        }
        let name: String = l[i + 5..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    };
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut all = String::new();
    for f in &files {
        // posture.rs defines the trait; its own `Open`/`Closed` fixtures are
        // test doubles, not workspace types.
        if f.ends_with("kb-core/src/posture.rs") {
            continue;
        }
        let text = std::fs::read_to_string(f).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.trim() == "#[default]" {
                if let Some(name) = (0..=i).rev().find_map(|j| enum_re(lines[j])) {
                    found.entry(name).or_default().push(
                        f.strip_prefix(workspace_crates())
                            .unwrap_or(f)
                            .display()
                            .to_string(),
                    );
                }
            }
        }
        all.push_str(&code_only(&text));
        all.push('\n');
    }
    (found, all)
}

#[test]
fn every_defaulted_enum_is_in_the_reviewed_inventory() {
    let (found, _) = scan();
    assert!(
        found.len() >= 10,
        "scan found only {} defaulted enums -- the walk is broken: {found:?}",
        found.len()
    );
    let listed: Vec<&str> = INVENTORY.iter().map(|(n, _)| *n).collect();
    for (name, files) in &found {
        assert!(
            listed.contains(&name.as_str()),
            "`#[default]` enum `{name}` ({files:?}) is not in INVENTORY in \
             crates/kb-core/tests/posture_inventory.rs. If it gates access (who may see, \
             write or reach something) implement `kb_core::posture::Posture` for it and \
             call `assert_default_is_restrictive::<{name}>()` in a test, then list it as \
             `Posture`; otherwise list it as `Reviewed(<why it is not security-relevant>)`."
        );
    }
    for name in &listed {
        assert!(
            found.contains_key(*name),
            "INVENTORY lists `{name}` but no `#[default]` enum of that name exists any \
             more -- remove the stale entry"
        );
    }
}

#[test]
fn posture_entries_implement_the_trait_and_pin_their_default() {
    let (_, all) = scan();
    for (name, class) in INVENTORY {
        match class {
            Posture => {
                assert!(
                    implements_posture(&all, name),
                    "`{name}` is listed as Posture but has no `impl Posture for {name}` item"
                );
                assert!(
                    pins_default(&all, name),
                    "`{name}` implements Posture but no test calls \
                     `assert_default_is_restrictive::<{name}>()`"
                );
            }
            Reviewed(why) => assert!(!why.trim().is_empty(), "`{name}`: empty review reason"),
        }
    }
}

/// The item checks parse code: a comment or string that merely MENTIONS the
/// impl / the pinning call must not satisfy the inventory.
#[test]
fn item_checks_ignore_comments_and_strings() {
    let fake = "// impl Posture for Ghost {\n\
                /* assert_default_is_restrictive::<Ghost>() */\n\
                let s = \"impl Posture for Ghost {\";\n\
                let t = \"assert_default_is_restrictive::<Ghost>()\";\n";
    let code = code_only(fake);
    assert!(!implements_posture(&code, "Ghost"), "{code}");
    assert!(!pins_default(&code, "Ghost"), "{code}");

    let real = "impl kb_core::posture::Posture for Real {\n\
                fn restrictive() -> Self { Real::A }\n}\n\
                #[test] fn t() { assert_default_is_restrictive::<Real>(); }\n";
    let code = code_only(real);
    assert!(implements_posture(&code, "Real"));
    assert!(pins_default(&code, "Real"));
    assert!(!implements_posture(&code, "Rea"), "prefix is not a match");
    assert!(!pins_default(&code, "Rea"));
}
