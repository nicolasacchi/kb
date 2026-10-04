//! Drift gate for the protocol registry (v0.45 N1): `schemas/` + `kb validate`.
//!
//! The schemas are hand-written structural lints of the Rust serde types, so
//! the risk is that a type changes and the schema silently stops describing
//! it. Where kb-core / kb-server can be called from here, the samples are
//! therefore PRODUCED by the real code (`save_atomic`, `md::to_markdown`,
//! `Post::mint`, `Proposal`) and validated through the shipped binary; the
//! kb-code contracts (not linkable from this crate) are pinned by their
//! committed goldens and registries plus hand-written fixtures.
//!
//! Every test here fails without the registry: delete a schema, widen one so
//! an invalid fixture passes, or add a `kb-comments/3` constant without a
//! registry entry, and a named test below goes red.

use assert_cmd::Command;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn schemas_dir() -> PathBuf {
    root().join("schemas")
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn index() -> Vec<Value> {
    let v: Value = serde_json::from_str(&read(&schemas_dir().join("index.json"))).unwrap();
    v["schemas"].as_array().expect("schemas array").clone()
}

fn dashed(id: &str) -> String {
    id.replace('/', "-")
}

fn kb_validate(args: &[&str]) -> Output {
    Command::cargo_bin("kb")
        .unwrap()
        .arg("validate")
        .args(args)
        .output()
        .expect("run kb validate")
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exit code")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn validate_ok(path: &Path, schema: Option<&str>) -> String {
    let p = path.to_str().unwrap();
    let o = match schema {
        Some(s) => kb_validate(&[p, "--schema", s]),
        None => kb_validate(&[p]),
    };
    assert_eq!(
        code(&o),
        0,
        "{} should conform to {schema:?}\nstdout: {}\nstderr: {}",
        path.display(),
        stdout(&o),
        stderr(&o)
    );
    stdout(&o)
}

fn fixtures(id: &str, status: &str) -> Vec<PathBuf> {
    let dir = schemas_dir().join("fixtures").join(dashed(id)).join(status);
    let mut v: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

// --- the registry itself ------------------------------------------------------

#[test]
fn every_registered_schema_is_valid_draft_2020_12() {
    let base = "https://github.com/nicolasacchi/kb/schemas/";
    let meta = "https://json-schema.org/draft/2020-12/schema";
    let mut files = 0;
    for e in index() {
        let id = e["id"].as_str().unwrap();
        let Some(file) = e["file"].as_str() else {
            assert_eq!(
                e["kind"], "text",
                "{id}: only a text grammar has no schema file"
            );
            assert!(
                e["pattern"].is_string(),
                "{id}: a text contract documents its pattern"
            );
            continue;
        };
        files += 1;
        let text = read(&schemas_dir().join(file));
        let schema: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(schema["$schema"], meta, "{id}");
        assert_eq!(schema["$id"], format!("{base}{id}"), "{id}");
        let mut compiler = boon::Compiler::new();
        compiler.set_default_draft(boon::Draft::V2020_12);
        let url = schema["$id"].as_str().unwrap().to_string();
        compiler
            .add_resource(&url, schema)
            .unwrap_or_else(|e| panic!("{id}: {e}"));
        let mut schemas = boon::Schemas::new();
        compiler
            .compile(&url, &mut schemas)
            .unwrap_or_else(|e| panic!("{id}: not a valid draft 2020-12 schema: {e}"));
    }
    assert!(
        files >= 10,
        "the v1 registry ships at least ten schema files, got {files}"
    );
}

#[test]
fn index_json_lists_exactly_the_files_in_schemas_dir() {
    let mut on_disk = BTreeSet::new();
    for entry in std::fs::read_dir(schemas_dir()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if name.ends_with(".schema.json") {
            on_disk.insert(name);
        }
    }
    let mut indexed = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for e in index() {
        let id = e["id"].as_str().unwrap().to_string();
        assert!(ids.insert(id.clone()), "{id} is indexed twice");
        if let Some(f) = e["file"].as_str() {
            assert_eq!(f, format!("{}.schema.json", dashed(&id)), "{id}: file name");
            indexed.insert(f.to_string());
        }
    }
    assert_eq!(on_disk, indexed, "schemas/ and schemas/index.json disagree");
}

#[test]
fn the_binary_embeds_exactly_the_indexed_contracts() {
    let o = kb_validate(&["--list", "--json"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let listed: Value = serde_json::from_str(&stdout(&o)).unwrap();
    let from_binary: BTreeSet<String> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    let from_index: BTreeSet<String> = index()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(from_binary, from_index);
}

#[test]
fn invalid_fixtures_are_rejected() {
    for e in index() {
        let id = e["id"].as_str().unwrap();
        let invalid = fixtures(id, "invalid");
        let names: BTreeSet<String> = invalid
            .iter()
            .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        for needed in ["wrong-schema", "missing-key", "wrong-type"] {
            assert!(
                names.contains(needed),
                "{id}: no invalid fixture `{needed}`"
            );
        }
        for p in &invalid {
            let o = kb_validate(&[p.to_str().unwrap(), "--schema", id]);
            assert_eq!(
                code(&o),
                1,
                "{id}: {} must be rejected (exit 1)\nstdout: {}\nstderr: {}",
                p.display(),
                stdout(&o),
                stderr(&o)
            );
        }
        for p in fixtures(id, "valid") {
            // Valid fixtures also auto-detect: the contract they declare is the
            // one they are filed under.
            let out = validate_ok(&p, None);
            assert!(out.contains(id), "{id}: auto-detected as {out}");
        }
    }
}

// --- samples produced by the real code ---------------------------------------

fn spec(private: bool, tags: Vec<String>) -> kb_core::review::NewComment {
    kb_core::review::NewComment {
        file: "0123456789ab".into(),
        file_label: "Design note".into(),
        anchor: kb_core::review::Anchor::Selection {
            css_path: "main > p:nth-of-type(2)".into(),
            offset: 4,
            snippet: "retry budget".into(),
        },
        author: kb_core::review::Author::You,
        body: "Is the budget per process?".into(),
        choices: vec![],
        attachments: vec![],
        user: Some("operator".into()),
        tags,
        private,
    }
}

// invariant:6 sidecar-schema-stamp
#[test]
fn comments_v1_and_v2_roundtrip_validate() {
    use kb_core::review::{self, Author, Choice, ReviewFile, VerdictState};
    let dir = tempfile::tempdir().unwrap();
    let kb = kb_core::types::KbName::new("notes").unwrap();

    // A public-only sidecar is written as /1 (byte-identical to every earlier
    // release) and validates as /1.
    let mut f = ReviewFile::empty_skeleton(&kb, "0123456789ab", "Design note");
    let cid = f.add_comment(spec(false, vec![])).id.clone();
    f.add_reply(
        &cid,
        Author::Claude,
        "Per process today.".into(),
        vec![Choice {
            label: "Apply".into(),
            reply: "Please apply it.".into(),
            resolve: true,
        }],
        None,
    )
    .unwrap();
    f.set_verdict(VerdictState::Approve, Some("ok".into()), None);
    let p1 = dir.path().join("public.json");
    review::save_atomic(&p1, &f, None).unwrap();
    let out = validate_ok(&p1, None);
    assert!(out.contains("kb-comments/1"), "{out}");

    // The same document plus one private note is stamped /2 at save time and
    // validates as /2 - and ONLY as /2: it must not claim /1, or a pre-notes
    // binary would load it and publish the note on rollback.
    f.add_comment(spec(true, vec!["scratch".into()]));
    let p2 = dir.path().join("with-note.json");
    review::save_atomic(&p2, &f, None).unwrap();
    let out = validate_ok(&p2, None);
    assert!(out.contains("kb-comments/2"), "{out}");
    validate_ok(&p2, Some("kb-comments/2"));
    let wrong = kb_validate(&[p2.to_str().unwrap(), "--schema", "kb-comments/1"]);
    assert_eq!(code(&wrong), 1, "a /2 file must not validate as /1");
    let wrong = kb_validate(&[p1.to_str().unwrap(), "--schema", "kb-comments/2"]);
    assert_eq!(
        code(&wrong),
        1,
        "a public-only /1 file must not validate as /2"
    );

    // A /1 stamp over a private note is the exact rollback hazard the /2
    // stamp closes; the lint refuses it.
    let mut hazard: Value = serde_json::from_str(&read(&p2)).unwrap();
    hazard["schema"] = json!("kb-comments/1");
    let ph = dir.path().join("hazard.json");
    std::fs::write(&ph, serde_json::to_string_pretty(&hazard).unwrap()).unwrap();
    let o = kb_validate(&[ph.to_str().unwrap(), "--schema", "kb-comments/1"]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
}

#[test]
fn list_header_validates() {
    use kb_core::lists::{md, ExportEntry, ListExport, ReadOverride, EXPORT_SCHEMA};
    let doc = ListExport {
        schema: EXPORT_SCHEMA.to_string(),
        kb: Some("platform".into()),
        list_id: Some("l_9f3a21c4d0aa".into()),
        title: "Async Rust, properly".into(),
        description: Some("In reading order.".into()),
        pinned: false,
        archived: false,
        entries: vec![ExportEntry {
            id: Some("le_a1b2c3d4e5f6".into()),
            path: Some("research/async/from-scratch.html".into()),
            read_override: Some(ReadOverride::Read),
            ..Default::default()
        }],
    };
    let dir = tempfile::tempdir().unwrap();
    let md_path = dir.path().join("list.md");
    std::fs::write(&md_path, md::to_markdown(&doc)).unwrap();
    let out = validate_ok(&md_path, None);
    assert!(out.contains("kb-list/1"), "{out}");
    // The JSON export is the same object plus title/entries.
    let json_path = dir.path().join("list.json");
    std::fs::write(&json_path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    validate_ok(&json_path, Some("kb-list/1"));
    // A Markdown list with no header is not a kb-list/1 document.
    let bare = dir.path().join("bare.md");
    std::fs::write(&bare, "# Just a heading\n\n1. [ ] [x](a.html)\n").unwrap();
    let o = kb_validate(&[bare.to_str().unwrap(), "--schema", "kb-list/1"]);
    assert_eq!(code(&o), 1);
}

#[test]
fn proposal_sample_validates() {
    use kb_server::routes::proposals::{Proposal, ProposalSource, SCHEMA};
    let p = Proposal {
        id: "p_0123456789ab".into(),
        schema: SCHEMA.to_string(),
        created_at: 1_767_214_800,
        session_id: Some("4b7e91c2a0".into()),
        title: "Retry budget is per process".into(),
        body: "Measured on the demo service.".into(),
        category: "memory-user".into(),
        tags: vec!["demo".into()],
        global: false,
        linked_kbs: vec!["notes".into()],
        salience: Some(0.5),
        supersedes: None,
        source: ProposalSource::Comment,
        note: Some("kept from a comment".into()),
        memory_created_at: Some(1_767_000_000),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p_0123456789ab.json");
    std::fs::write(&path, serde_json::to_string_pretty(&p).unwrap()).unwrap();
    let out = validate_ok(&path, None);
    assert!(out.contains("kb-proposal/1"), "{out}");
    for f in fixtures("kb-proposal/1", "valid") {
        validate_ok(&f, None);
    }
}

#[test]
fn slate_fixtures_all_validate_per_line() {
    use kb_core::slate::{self, Kind, Origin, Post, PostBody, Prov};
    let dir = root().join("crates/kb-core/tests/slate_fixtures");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        n += 1;
        // The schema must accept exactly what the real reader accepts.
        slate::parse_ledger(&read(&path)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        validate_ok(&path, Some("kb-slate/1"));
    }
    assert_eq!(
        n, 11,
        "the slate fixtures directory changed; update the registry gate"
    );
    validate_ok(&dir.join("plain.jsonl"), None);

    // A line minted by the real engine validates too.
    let post = Post::mint(
        PostBody {
            kind: Kind::Found,
            line: "retry budget is per process".into(),
            body: Some("see the retry loop".into()),
            topic: Some("sprout".into()),
            subject: None,
            refs: vec!["path:src/retry.rs".into()],
            re: None,
            supersedes: None,
            pin: None,
            anyway: false,
            over: None,
            abandoned: None,
            failed: None,
            to: None,
            prov: Prov {
                harness: "claude".into(),
                origin: Origin::Agent,
                ..Default::default()
            },
        },
        7,
        1_767_214_800,
    );
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ledger.jsonl");
    std::fs::write(
        &path,
        format!("{}\n", slate::to_ledger_line(&post).unwrap()),
    )
    .unwrap();
    validate_ok(&path, None);
}

// --- kb-code contracts: committed goldens + hand-written fixtures --------------

#[test]
fn kbc_cmd_and_theme_registries_validate() {
    let cmd = root().join("crates/kb-code-server/commands/registry.json");
    assert!(validate_ok(&cmd, None).contains("kbc-cmd/1"));
    let theme = root().join("crates/kb-code-server/themes/registry.json");
    assert!(validate_ok(&theme, None).contains("kbc-theme/1"));
}

#[test]
fn review_context_golden_validates() {
    let golden = root().join("crates/kb-code-server/grammar/review-context.golden.json");
    let mut bundle: Value = serde_json::from_str(&read(&golden)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // The golden is `assemble()`'s body; the route adds the envelope. Without
    // it the document is not a bundle, and the lint says so.
    let bare = dir.path().join("bare.json");
    std::fs::write(&bare, serde_json::to_string(&bundle).unwrap()).unwrap();
    let o = kb_validate(&[bare.to_str().unwrap(), "--schema", "kbc-review-context/1"]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    bundle["schema"] = json!("kbc-review-context/1");
    bundle["review_id"] = json!(1);
    bundle["repo"] = json!("demo");
    bundle["ps"] = json!(1);
    let full = dir.path().join("full.json");
    std::fs::write(&full, serde_json::to_string(&bundle).unwrap()).unwrap();
    validate_ok(&full, None);
}

#[test]
fn findings_v1_v2_samples_validate() {
    for f in fixtures("kbc-findings/1", "valid") {
        assert!(validate_ok(&f, None).contains("kbc-findings/1"));
    }
    for f in fixtures("kbc-findings/2", "valid") {
        assert!(validate_ok(&f, None).contains("kbc-findings/2"));
    }
    // The two versions are not interchangeable: /2 carries `act`/`blocking`
    // and an optional slug, /1 requires the slug.
    let v2 = fixtures("kbc-findings/2", "valid").remove(0);
    let o = kb_validate(&[v2.to_str().unwrap(), "--schema", "kbc-findings/1"]);
    assert_eq!(code(&o), 1);
}

#[test]
fn github_export_sample_validates() {
    for f in fixtures("kbc-github-export/1", "valid") {
        assert!(validate_ok(&f, None).contains("kbc-github-export/1"));
    }
}

#[test]
fn recall_marker_goldens_match_grammar() {
    let dir = root().join("plugins/kb-memory/hooks/tests/fixtures");
    for name in ["recall-layout-v1.txt", "recall-layout-v2.txt"] {
        let out = validate_ok(&dir.join(name), None);
        assert!(out.contains("kb-recall/1"), "{name}: {out}");
    }
    let tmp = tempfile::tempdir().unwrap();
    let check = |marker: &str| -> i32 {
        let p = tmp.path().join("m.txt");
        std::fs::write(&p, format!("- hit  [kb]\n{marker}\n")).unwrap();
        code(&kb_validate(&[
            p.to_str().unwrap(),
            "--schema",
            "kb-recall/1",
        ]))
    };
    // pos is optional; unknown pairs are tolerated (forward-compatible).
    assert_eq!(check("<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6-->"), 0);
    assert_eq!(check("<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=99-->"), 0);
    assert_eq!(
        check("<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=3 future=x-->"),
        0
    );
    // pos outside 1..=99, a missing id, a short id: malformed.
    assert_eq!(check("<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=100-->"), 1);
    assert_eq!(check("<!--kb-recall/1 kb=kb-->"), 1);
    assert_eq!(check("<!--kb-recall/1 kb=kb id=a1b2c3-->"), 1);
}

// --- no contract constant without a registry entry -----------------------------

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// `(file relative to the repo root, const name, value)` for every
/// `const <..SCHEMA..>: &str = "<value>"` in any crate's `src/`.
fn schema_consts() -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let crates = root().join("crates");
    for c in std::fs::read_dir(&crates).unwrap() {
        let src = c.unwrap().path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for f in files {
            for line in read(&f).lines() {
                let t = line.trim();
                let Some(at) = t.find("const ") else { continue };
                let rest = &t[at + "const ".len()..];
                let Some((name, tail)) = rest.split_once(':') else {
                    continue;
                };
                if !name.contains("SCHEMA") {
                    continue;
                }
                let Some((_, lit)) = tail.split_once("= \"") else {
                    continue;
                };
                let Some((value, _)) = lit.split_once('"') else {
                    continue;
                };
                let rel = f
                    .strip_prefix(root())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, name.trim().to_string(), value.to_string()));
            }
        }
    }
    out
}

#[test]
fn every_schema_const_in_code_has_a_registry_entry() {
    let ids: BTreeSet<String> = index()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    let families: BTreeSet<&str> = ids
        .iter()
        .filter_map(|i| i.rsplit_once('/'))
        .map(|(f, _)| f)
        .collect();
    let consts = schema_consts();
    assert!(
        consts.len() > 50,
        "the const scan is broken: found {}",
        consts.len()
    );

    // 1. A constant in a registered family must be a registered version:
    //    adding `kb-comments/3` without a schema fails here.
    for (file, name, value) in &consts {
        if let Some((family, _)) = value.rsplit_once('/') {
            if families.contains(family) {
                assert!(
                    ids.contains(value),
                    "{file}: `{name}` = {value:?} is a version of a registered contract with no \
                     schemas/ entry; add schemas/{}.schema.json and index it",
                    dashed(value)
                );
            }
        }
    }

    // 2. The scan must actually see the constants the registry is built on
    //    (so a rename cannot make check 1 vacuous).
    let anchored = [
        ("crates/kb-core/src/review.rs", "SCHEMA", "kb-comments/1"),
        ("crates/kb-core/src/review.rs", "SCHEMA_V2", "kb-comments/2"),
        ("crates/kb-core/src/lists.rs", "EXPORT_SCHEMA", "kb-list/1"),
        ("crates/kb-core/src/slate.rs", "SCHEMA", "kb-slate/1"),
        (
            "crates/kb-server/src/routes/proposals.rs",
            "SCHEMA",
            "kb-proposal/1",
        ),
        (
            "crates/kb-code-server/src/review_findings.rs",
            "IMPORT_SCHEMA",
            "kbc-findings/1",
        ),
        (
            "crates/kb-code-server/src/review_context.rs",
            "CONTEXT_SCHEMA",
            "kbc-review-context/1",
        ),
        (
            "crates/kb-code-server/src/review_github_export.rs",
            "EXPORT_SCHEMA",
            "kbc-github-export/1",
        ),
    ];
    for (file, name, id) in anchored {
        assert!(
            consts.iter().any(|(f, n, v)| f == file && n == name && v == id),
            "{file}: `const {name}` = {id:?} not found; if it moved, update the registry and this table"
        );
        assert!(ids.contains(id), "{id} is not registered");
    }

    // 3. Contracts that are not Rust constants: pinned where they live.
    let literal = |file: &str, needle: &str| {
        assert!(
            read(&root().join(file)).contains(needle),
            "{file}: {needle:?} not found"
        );
    };
    literal(
        "crates/kb-code-server/src/review_pseudo.rs",
        "\"kbc-findings/2\"",
    );
    literal(
        "crates/kb-code-server/commands/registry.json",
        "\"kbc-cmd/1\"",
    );
    literal(
        "crates/kb-code-server/themes/registry.json",
        "\"kbc-theme/1\"",
    );
    literal("crates/kb-core/src/sessions/view.rs", "<!--kb-recall/1 ");
    for id in ["kbc-findings/2", "kbc-cmd/1", "kbc-theme/1", "kb-recall/1"] {
        assert!(ids.contains(id), "{id} is not registered");
    }
}

// --- the verb ---------------------------------------------------------------------

#[test]
fn kb_validate_autodetects_and_exits_0() {
    let f = fixtures("kb-comments/2", "valid").remove(0);
    let o = kb_validate(&[f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("kb-comments/2"), "{}", stdout(&o));
    let o = kb_validate(&[f.to_str().unwrap(), "--json"]);
    let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["schema"], "kb-comments/2");
    assert_eq!(v["problems"], json!([]));
}

#[test]
fn kb_validate_bad_file_exits_1_with_pointer() {
    let f = schemas_dir().join("fixtures/kb-comments-1/invalid/wrong-type.json");
    let o = kb_validate(&[f.to_str().unwrap(), "--schema", "kb-comments/1"]);
    assert_eq!(code(&o), 1);
    assert!(
        stderr(&o).contains("/comments/0/status"),
        "the failing JSON pointer must be listed: {}",
        stderr(&o)
    );
    let o = kb_validate(&[f.to_str().unwrap(), "--schema", "kb-comments/1", "--json"]);
    assert_eq!(code(&o), 1);
    let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["ok"], false);
    let pointers: Vec<&str> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pointer"].as_str().unwrap())
        .collect();
    assert!(pointers.contains(&"/comments/0/status"), "{pointers:?}");
    // A JSONL failure names its line.
    let o = kb_validate(&[
        schemas_dir()
            .join("fixtures/kb-slate-1/invalid/wrong-type.jsonl")
            .to_str()
            .unwrap(),
        "--schema",
        "kb-slate/1",
    ]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("line 1"), "{}", stderr(&o));
}

#[test]
fn kb_validate_refuses_external_refs_in_a_user_schema() {
    let dir = tempfile::tempdir().unwrap();
    let schema = dir.path().join("mine.json");
    std::fs::write(&schema, r#"{"$ref":"other.json"}"#).unwrap();
    let doc = dir.path().join("doc.json");
    std::fs::write(&doc, "{}").unwrap();
    let o = kb_validate(&["--schema", schema.to_str().unwrap(), doc.to_str().unwrap()]);
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("same-document"), "{}", stderr(&o));
}

#[test]
fn kb_validate_unknown_schema_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("future.json");
    std::fs::write(&f, "{\"schema\":\"kb-comments/9\"}").unwrap();
    let o = kb_validate(&[f.to_str().unwrap()]);
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("unknown schema"), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("kb-comments/1"),
        "names the registry: {}",
        stderr(&o)
    );
    // Nothing to detect from.
    let g = dir.path().join("prose.txt");
    std::fs::write(&g, "just some prose\n").unwrap();
    assert_eq!(code(&kb_validate(&[g.to_str().unwrap()])), 2);
    // Unreadable file, and no FILE at all.
    assert_eq!(
        code(&kb_validate(&[dir
            .path()
            .join("missing.json")
            .to_str()
            .unwrap()])),
        2
    );
    assert_eq!(code(&kb_validate(&[])), 2);
}

#[test]
fn kb_validate_works_without_daemon() {
    // No config, no daemon, no state dir: a pure file operation.
    let home = tempfile::tempdir().unwrap();
    let f = fixtures("kb-proposal/1", "valid").remove(0);
    let o = Command::cargo_bin("kb")
        .unwrap()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("cfg"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("KB_HOME", home.path().join("kbhome"))
        .env_remove("KB_DAEMON")
        .args([
            "--config",
            "/nonexistent/kb.toml",
            "validate",
            f.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(
        !home.path().join("kbhome").exists() && !home.path().join("state").exists(),
        "validate must not create state"
    );
}

#[test]
fn kb_validate_never_edits_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.json");
    let before = read(&schemas_dir().join("fixtures/kb-comments-1/invalid/missing-key.json"));
    std::fs::write(&f, &before).unwrap();
    let o = kb_validate(&[f.to_str().unwrap(), "--schema", "kb-comments/1"]);
    assert_eq!(code(&o), 1);
    assert_eq!(read(&f), before);
}
