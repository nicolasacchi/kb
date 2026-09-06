//! Fixture-based tests, built with the real `git` CLI in tempdirs (mirrors
//! `config.rs`'s `git_init` convention). `TMPDIR` discipline: the ambient
//! `/tmp` has a per-user quota on this box, so these rely on the caller
//! having set `TMPDIR` (see the workspace CLAUDE.md build tips) —
//! `tempfile::tempdir()` honours it automatically.

use super::*;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
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

fn git_out(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A plain repo: two commits on `main`, a subdir with a file, a symlink, a
/// lightweight tag on the first commit, and a second branch `feature`
/// pointing at the second commit.
fn plain_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);

    std::fs::write(dir.join("root.txt"), b"root file\n").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub").join("nested.txt"), b"nested file\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("root.txt", dir.join("a-symlink")).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    git(dir, &["tag", "v1"]);

    git(dir, &["branch", "feature"]);
    git(dir, &["checkout", "-q", "feature"]);
    std::fs::write(dir.join("root.txt"), b"root file, v2\n").unwrap();
    git(dir, &["commit", "-q", "-am", "c2"]);
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "-q", "feature"]);

    tmp
}

#[test]
fn open_and_basic_repo_info() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();
    assert!(!repo.is_worktree());
    assert!(!repo.is_shallow());
    assert_eq!(repo.git_dir(), repo.common_dir());

    let head = repo.head_info().unwrap();
    assert!(!head.detached);
    assert!(!head.unborn);
    assert_eq!(head.branch.as_deref(), Some("main"));
    assert!(head.sha.is_some());
}

#[test]
fn open_rejects_non_repo_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let err = GitRepo::open(tmp.path()).expect_err("not a git repo");
    assert!(matches!(err, GitError::Open { .. }), "got: {err:?}");
}

#[test]
fn resolve_handles_every_revspec_form() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();

    let head_sha = git_out(tmp.path(), &["rev-parse", "HEAD"]);
    let parent_sha = git_out(tmp.path(), &["rev-parse", "HEAD^"]);
    let short_sha = git_out(tmp.path(), &["rev-parse", "--short", "HEAD"]);

    assert_eq!(repo.resolve("HEAD").unwrap().to_string(), head_sha);
    assert_eq!(repo.resolve(&head_sha).unwrap().to_string(), head_sha);
    assert_eq!(repo.resolve(&short_sha).unwrap().to_string(), head_sha);
    assert_eq!(repo.resolve("main").unwrap().to_string(), head_sha);
    assert_eq!(repo.resolve("v1").unwrap().to_string(), parent_sha);
    assert_eq!(repo.resolve("HEAD~1").unwrap().to_string(), parent_sha);
    assert_eq!(repo.resolve("HEAD^").unwrap().to_string(), parent_sha);

    let err = repo.resolve("does-not-exist-at-all").unwrap_err();
    assert!(matches!(err, GitError::Resolve { .. }), "got: {err:?}");
}

#[test]
fn list_refs_reports_branch_and_tag_with_correct_target() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();
    let head_sha = git_out(tmp.path(), &["rev-parse", "HEAD"]);
    let v1_sha = git_out(tmp.path(), &["rev-parse", "v1"]);

    let refs = repo.list_refs().unwrap();

    let main_ref = refs
        .iter()
        .find(|r| r.kind == RefKind::Branch && r.name == "main")
        .expect("main branch listed");
    assert_eq!(main_ref.full_name, "refs/heads/main");
    assert_eq!(main_ref.target_sha, head_sha);
    assert!(main_ref.is_head);

    let feature_ref = refs
        .iter()
        .find(|r| r.kind == RefKind::Branch && r.name == "feature")
        .expect("feature branch listed");
    assert!(!feature_ref.is_head);

    let tag_ref = refs
        .iter()
        .find(|r| r.kind == RefKind::Tag && r.name == "v1")
        .expect("v1 tag listed");
    assert_eq!(tag_ref.full_name, "refs/tags/v1");
    assert_eq!(tag_ref.target_sha, v1_sha);
    assert!(!tag_ref.is_head);

    assert!(
        refs.iter().all(|r| r.remote.is_none()),
        "list_refs must stay local+tags only (remotes are list_remote_branches)"
    );
}

/// Plant `refs/remotes/origin/<name>` + optional origin/HEAD without a
/// network fetch (V4.L1 fixture recipe).
fn plant_origin_ref(dir: &Path, name: &str, sha: &str) {
    git(
        dir,
        &["update-ref", &format!("refs/remotes/origin/{name}"), sha],
    );
}

fn plant_origin_head(dir: &Path, branch: &str) {
    git(
        dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{branch}"),
        ],
    );
}

#[test]
fn list_remote_branches_skips_origin_head_and_reports_remote() {
    let tmp = plain_repo();
    let dir = tmp.path();
    let head_sha = git_out(dir, &["rev-parse", "HEAD"]);
    plant_origin_ref(dir, "main", &head_sha);
    plant_origin_ref(dir, "only-on-origin", &head_sha);
    plant_origin_head(dir, "main");

    let repo = GitRepo::open(dir).unwrap();
    let remotes = repo.list_remote_branches().unwrap();
    assert!(
        remotes
            .iter()
            .all(|r| r.remote.as_deref() == Some("origin")),
        "every remote entry carries remote=origin: {remotes:?}"
    );
    let names: Vec<&str> = remotes.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"main"), "got: {names:?}");
    assert!(names.contains(&"only-on-origin"), "got: {names:?}");
    assert!(
        !names.contains(&"HEAD"),
        "origin/HEAD must be excluded: {names:?}"
    );
    assert!(
        remotes
            .iter()
            .all(|r| r.kind == RefKind::Branch && !r.is_head),
        "remote branches are never HEAD"
    );

    // list_refs stays local+tags — origin-only names do not leak in.
    let locals = repo.list_refs().unwrap();
    assert!(locals
        .iter()
        .all(|r| r.name != "only-on-origin" && r.remote.is_none()));
}

#[test]
fn default_branch_reads_origin_head_then_falls_back_to_head_info() {
    let tmp = plain_repo();
    let dir = tmp.path();
    let repo = GitRepo::open(dir).unwrap();
    assert_eq!(
        repo.default_branch().as_deref(),
        Some("main"),
        "no origin/HEAD → HEAD's branch"
    );

    let head_sha = git_out(dir, &["rev-parse", "HEAD"]);
    plant_origin_ref(dir, "main", &head_sha);
    plant_origin_ref(dir, "develop", &head_sha);
    plant_origin_head(dir, "develop");

    let repo = GitRepo::open(dir).unwrap();
    assert_eq!(
        repo.default_branch().as_deref(),
        Some("develop"),
        "origin/HEAD wins over HEAD=main"
    );
}

#[test]
fn default_branch_falls_back_when_origin_head_is_absent() {
    let tmp = plain_repo();
    let dir = tmp.path();
    git(dir, &["checkout", "-q", "feature"]);
    let repo = GitRepo::open(dir).unwrap();
    assert_eq!(
        repo.default_branch().as_deref(),
        Some("feature"),
        "HEAD heuristic when the origin/HEAD symref is missing"
    );
}

/// `git ls-tree <rev>` (root, non-recursive) parsed into `(kind, name)`
/// pairs, for comparison against our own tree listing.
fn ls_tree_root(dir: &Path, rev: &str) -> Vec<(String, String)> {
    let out = git_out(dir, &["ls-tree", rev]);
    out.lines()
        .map(|line| {
            // `<mode> <type> <sha>\t<name>`
            let (meta, name) = line.split_once('\t').expect("tab-separated ls-tree line");
            let kind = meta
                .split_whitespace()
                .nth(1)
                .expect("type field")
                .to_string();
            (kind, name.to_string())
        })
        .collect()
}

#[test]
fn tree_at_head_matches_git_ls_tree() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();

    let mut want = ls_tree_root(tmp.path(), "HEAD");
    want.sort();

    let entries = repo.list_tree("HEAD", "").unwrap();
    let mut got: Vec<(String, String)> = entries
        .iter()
        .map(|e| {
            let git_kind = match e.kind {
                EntryKind::Dir => "tree",
                EntryKind::File | EntryKind::Symlink => "blob",
                EntryKind::Submodule => "commit",
            };
            (git_kind.to_string(), e.name.clone())
        })
        .collect();
    got.sort();

    assert_eq!(got, want);

    // Cross-check oids against `git ls-tree` too.
    for line in git_out(tmp.path(), &["ls-tree", "HEAD"]).lines() {
        let (meta, name) = line.split_once('\t').unwrap();
        let mut parts = meta.split_whitespace();
        let _mode = parts.next().unwrap();
        let _kind = parts.next().unwrap();
        let sha = parts.next().unwrap();
        let entry = entries.iter().find(|e| e.name == name).unwrap();
        assert_eq!(entry.oid, sha, "oid mismatch for {name}");
    }
}

#[test]
fn tree_reports_file_sizes_and_symlink_kind() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();
    let entries = repo.list_tree("HEAD", "").unwrap();

    let root_txt = entries.iter().find(|e| e.name == "root.txt").unwrap();
    assert_eq!(root_txt.kind, EntryKind::File);
    assert_eq!(root_txt.size, Some(b"root file, v2\n".len() as u64));

    let sub = entries.iter().find(|e| e.name == "sub").unwrap();
    assert_eq!(sub.kind, EntryKind::Dir);
    assert_eq!(sub.size, None);

    #[cfg(unix)]
    {
        let link = entries.iter().find(|e| e.name == "a-symlink").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
        assert_eq!(link.size, Some(b"root.txt".len() as u64));
    }
}

#[test]
fn tree_at_tag_and_branch_and_short_sha() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();

    // At `v1` (first commit), `sub/nested.txt` already exists but the
    // second commit's changes to root.txt aren't there yet.
    let v1_entries = repo.list_tree("v1", "").unwrap();
    let root_txt = v1_entries.iter().find(|e| e.name == "root.txt").unwrap();
    assert_eq!(root_txt.size, Some(b"root file\n".len() as u64));

    let branch_entries = repo.list_tree("main", "").unwrap();
    assert_eq!(branch_entries, repo.list_tree("HEAD", "").unwrap());

    let short_sha = git_out(tmp.path(), &["rev-parse", "--short", "HEAD"]);
    assert_eq!(repo.list_tree(&short_sha, "").unwrap(), branch_entries);
}

#[test]
fn nested_tree_listing() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();
    let entries = repo.list_tree("HEAD", "sub").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "nested.txt");
    assert_eq!(entries[0].kind, EntryKind::File);

    // Leading/trailing slashes are tolerated.
    assert_eq!(repo.list_tree("HEAD", "/sub/").unwrap(), entries);
}

#[test]
fn tree_missing_path_errors_cleanly() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();
    let err = repo.list_tree("HEAD", "does/not/exist").unwrap_err();
    assert!(matches!(err, GitError::PathNotFound { .. }), "got: {err:?}");
}

#[test]
fn tree_on_a_file_path_errors_not_a_dir() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();
    let err = repo.list_tree("HEAD", "root.txt").unwrap_err();
    assert!(matches!(err, GitError::NotADir { .. }), "got: {err:?}");
}

#[test]
fn cat_returns_bytes_at_two_refs() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();

    let at_head = repo
        .read_blob("HEAD", "root.txt", DEFAULT_BLOB_SIZE_CAP)
        .unwrap();
    assert_eq!(at_head, b"root file, v2\n");

    let at_v1 = repo
        .read_blob("v1", "root.txt", DEFAULT_BLOB_SIZE_CAP)
        .unwrap();
    assert_eq!(at_v1, b"root file\n");

    let nested = repo
        .read_blob("HEAD", "sub/nested.txt", DEFAULT_BLOB_SIZE_CAP)
        .unwrap();
    assert_eq!(nested, b"nested file\n");
}

#[test]
fn cat_missing_path_errors_cleanly() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();
    let err = repo
        .read_blob("HEAD", "no/such/file.txt", DEFAULT_BLOB_SIZE_CAP)
        .unwrap_err();
    assert!(matches!(err, GitError::PathNotFound { .. }), "got: {err:?}");
}

#[test]
fn cat_on_a_directory_errors_not_a_blob() {
    let tmp = plain_repo();
    let repo = GitRepo::open(tmp.path()).unwrap();
    let err = repo
        .read_blob("HEAD", "sub", DEFAULT_BLOB_SIZE_CAP)
        .unwrap_err();
    assert!(matches!(err, GitError::NotABlob { .. }), "got: {err:?}");
}

#[test]
fn cat_enforces_the_size_cap() {
    let tmp = plain_repo();
    let dir = tmp.path();
    std::fs::write(dir.join("big.bin"), vec![b'x'; 1024]).unwrap();
    git(dir, &["add", "big.bin"]);
    git(dir, &["commit", "-q", "-m", "add big file"]);

    let repo = GitRepo::open(dir).unwrap();
    let err = repo.read_blob("HEAD", "big.bin", 100).unwrap_err();
    match err {
        GitError::TooLarge { size, cap, .. } => {
            assert_eq!(size, 1024);
            assert_eq!(cap, 100);
        }
        other => panic!("expected TooLarge, got: {other:?}"),
    }

    // Under the cap still works.
    let bytes = repo.read_blob("HEAD", "big.bin", 2048).unwrap();
    assert_eq!(bytes.len(), 1024);
}

#[test]
fn detached_head_is_reported() {
    let tmp = plain_repo();
    let dir = tmp.path();
    let head_sha = git_out(dir, &["rev-parse", "HEAD"]);
    git(dir, &["checkout", "-q", &head_sha]);

    let repo = GitRepo::open(dir).unwrap();
    let head = repo.head_info().unwrap();
    assert!(head.detached);
    assert!(!head.unborn);
    assert_eq!(head.branch, None);
    assert_eq!(head.sha.as_deref(), Some(head_sha.as_str()));
}

#[test]
fn unborn_head_on_a_fresh_repo() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    let repo = GitRepo::open(tmp.path()).unwrap();
    let head = repo.head_info().unwrap();
    assert!(!head.detached);
    assert!(head.unborn);
    assert_eq!(head.branch.as_deref(), Some("main"));
    assert_eq!(head.sha, None);
}

#[test]
fn linked_worktree_opens_and_reports_gitdir_commondir_split() {
    let tmp = plain_repo();
    let main_dir = tmp.path();
    let wt_parent = tempfile::tempdir().unwrap();
    let wt_path = wt_parent.path().join("wt");
    git(
        main_dir,
        &[
            "worktree",
            "add",
            "-q",
            wt_path.to_str().unwrap(),
            "feature",
        ],
    );
    // `.git` inside a linked worktree is a FILE, not a directory.
    assert!(wt_path.join(".git").is_file());

    let main_repo = GitRepo::open(main_dir).unwrap();
    assert!(!main_repo.is_worktree());

    let wt_repo = GitRepo::open(&wt_path).unwrap();
    assert!(wt_repo.is_worktree());
    assert_ne!(wt_repo.git_dir(), wt_repo.common_dir());
    assert_eq!(
        std::fs::canonicalize(wt_repo.common_dir()).unwrap(),
        std::fs::canonicalize(main_repo.common_dir()).unwrap(),
        "the linked worktree's common_dir must be the main repo's gitdir"
    );

    // Tree/cat reads work identically through the worktree handle.
    let via_main = main_repo.list_tree("feature", "").unwrap();
    let via_wt = wt_repo.list_tree("feature", "").unwrap();
    assert_eq!(via_main, via_wt);

    let via_main_blob = main_repo
        .read_blob("feature", "root.txt", DEFAULT_BLOB_SIZE_CAP)
        .unwrap();
    let via_wt_blob = wt_repo
        .read_blob("feature", "root.txt", DEFAULT_BLOB_SIZE_CAP)
        .unwrap();
    assert_eq!(via_main_blob, via_wt_blob);
}

#[test]
fn submodule_entry_reports_kind_and_pinned_sha_without_descending() {
    let sub_src = plain_repo();
    let sub_head_sha = git_out(sub_src.path(), &["rev-parse", "HEAD"]);

    let parent = tempfile::tempdir().unwrap();
    let parent_dir = parent.path();
    git(parent_dir, &["init", "-q", "-b", "main"]);
    git(parent_dir, &["config", "user.email", "test@example.com"]);
    git(parent_dir, &["config", "user.name", "Test"]);
    std::fs::write(parent_dir.join("top.txt"), b"top level\n").unwrap();
    git(parent_dir, &["add", "top.txt"]);
    git(parent_dir, &["commit", "-q", "-m", "c1"]);

    git(
        parent_dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub_src.path().to_str().unwrap(),
            "vendor/sub",
        ],
    );
    git(parent_dir, &["commit", "-q", "-m", "add submodule"]);

    let repo = GitRepo::open(parent_dir).unwrap();
    let vendor_entries = repo.list_tree("HEAD", "vendor").unwrap();
    let sub_entry = vendor_entries.iter().find(|e| e.name == "sub").unwrap();
    assert_eq!(sub_entry.kind, EntryKind::Submodule);
    assert_eq!(sub_entry.oid, sub_head_sha);
    assert_eq!(sub_entry.size, None);

    // Never descended into: reading it as a directory fails cleanly rather
    // than opening the submodule's own repository.
    let err = repo.list_tree("HEAD", "vendor/sub").unwrap_err();
    assert!(matches!(err, GitError::NotADir { .. }), "got: {err:?}");
}

#[test]
fn shallow_clone_flag_and_boundary_resolution() {
    let src = plain_repo();
    // A third commit so there's ancestry to run off the end of.
    std::fs::write(src.path().join("root.txt"), b"root file, v3\n").unwrap();
    git(src.path(), &["commit", "-q", "-am", "c3"]);

    let dst = tempfile::tempdir().unwrap();
    // `--depth` is silently ignored on a same-filesystem local clone (git
    // defaults to a `--local` hardlink-based copy, which has no concept of
    // shallow history) unless the `file://` URL scheme forces the real
    // upload-pack transport — hence both the explicit scheme AND
    // `protocol.file.allow=always` (transport-level clones from `file://`
    // are disabled by default since CVE-2022-39253).
    let out = Command::new("git")
        .args([
            "clone",
            "-q",
            "--depth=1",
            "-c",
            "protocol.file.allow=always",
            &format!("file://{}", src.path().display()),
            dst.path().to_str().unwrap(),
        ])
        .output()
        .expect("git clone runs");
    assert!(
        out.status.success(),
        "shallow clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let repo = GitRepo::open(dst.path()).unwrap();
    assert!(repo.is_shallow());

    // HEAD itself still resolves fine.
    assert!(repo.resolve("HEAD").is_ok());

    // Walking past the shallow boundary must error, not panic.
    let err = repo.resolve("HEAD~5").unwrap_err();
    match err {
        GitError::Resolve { message, .. } => {
            assert!(
                message.to_lowercase().contains("shallow"),
                "expected a shallow-boundary hint, got: {message}"
            );
        }
        other => panic!("expected Resolve, got: {other:?}"),
    }
}

#[test]
fn blob_oid_matches_the_tree_walk_oid_and_is_none_for_a_missing_path() {
    let repo_dir = plain_repo();
    let repo = GitRepo::open(repo_dir.path()).unwrap();

    let entries = repo.list_tree("HEAD", "").unwrap();
    let root_entry = entries.iter().find(|e| e.name == "root.txt").unwrap();

    let oid = repo.blob_oid("HEAD", "root.txt").unwrap();
    assert_eq!(oid, Some(root_entry.oid.clone()));

    // A nested path resolves the same way (not just root-level entries).
    let nested_oid = repo.blob_oid("HEAD", "sub/nested.txt").unwrap();
    assert!(nested_oid.is_some());

    // A path that doesn't exist at this revision is `None`, not an error.
    assert_eq!(repo.blob_oid("HEAD", "does-not-exist.txt").unwrap(), None);

    // A blob that changes between two commits reports two different oids —
    // `v1` (tagging c1, "root file\n") vs `HEAD` (post-merge, "root file,
    // v2\n" from the `feature` branch's c2).
    let head_oid = repo.blob_oid("HEAD", "root.txt").unwrap().unwrap();
    let v1_oid = repo.blob_oid("v1", "root.txt").unwrap().unwrap();
    assert_ne!(
        head_oid, v1_oid,
        "root.txt's content differs between v1 (c1) and HEAD (post-merge c2)"
    );
}
