//! GitHub Pages client — the public (`--public`) lane. No gate is
//! possible here (doc 23 §03/§04: gating GitHub Pages needs a Worker /
//! oauth2-proxy in front, which is exactly the infra the static-share
//! design avoids), so the engine rejects `--gate` for this host.
//!
//! v1 is **one repo per share** for a clean teardown (`delete_repo`).
//! Publishing is the Git Data API: a single root commit holding the whole
//! staging dir, then a `refs/heads/main` pointing at it, then Pages
//! enabled on `main` — no `auto_init`, so no stray `README.md` in the
//! served site.
//!
//! ## Sequence (`publish_dir`)
//! 1. `POST /user/repos` (`auto_init:false`, public) — create the repo.
//! 2. `POST …/git/blobs` (base64) — one blob per file.
//! 3. `POST …/git/trees` — a fresh tree of all blobs.
//! 4. `POST …/git/commits` — a parent-less root commit of that tree.
//! 5. `POST …/git/refs` — create `refs/heads/main` at the commit.
//! 6. `POST …/pages` — enable Pages on `main` (builds asynchronously).
//!
//! Token: `KB_GH_TOKEN` with repo create/delete + contents + Pages write
//! (classic `repo` scope, or a fine-grained PAT with Administration:write
//! + Contents:write + Pages:write on the target owner).
//!
//! Live request/response shapes are pinned by the deferred `#[ignore]`
//! integration test (plan R9), not CI; the offline-testable helpers
//! ([`pages_url`], tree-entry shape) are unit-tested here.

use crate::config::GithubShareConfig;
use crate::{Error, Result};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};

const API_BASE: &str = "https://api.github.com";

/// A repo created for a share.
#[derive(Debug, Clone)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
}

/// Outcome of `publish_dir`: the repo plus the base Pages URL
/// (`https://<owner>.github.io/<repo>/`). The engine appends the entry
/// file (e.g. `index.html` or `foo.html`) to form the shareable link.
#[derive(Debug, Clone)]
pub struct GithubDeploy {
    pub owner: String,
    pub name: String,
    pub url: String,
}

/// Base GitHub Pages URL for `owner`/`repo`. The `owner` segment is
/// lowercased (github.io hostnames are case-insensitive/lowercase) and
/// the path keeps a trailing slash.
pub fn pages_url(owner: &str, repo: &str) -> String {
    format!("https://{}.github.io/{repo}/", owner.to_ascii_lowercase())
}

/// One Git tree entry for a regular file blob (`mode 100644`).
fn tree_entry(path: &str, blob_sha: &str) -> Value {
    json!({ "path": path, "mode": "100644", "type": "blob", "sha": blob_sha })
}

#[derive(Deserialize)]
struct RepoResult {
    name: String,
    owner: OwnerResult,
}

#[derive(Deserialize)]
struct OwnerResult {
    login: String,
}

#[derive(Deserialize)]
struct ShaResult {
    sha: String,
}

/// GitHub REST client. Holds the token; `api_base` is overridable for
/// tests / a mock server.
pub struct GithubClient {
    http: reqwest::Client,
    token: String,
    api_base: String,
}

impl GithubClient {
    pub fn new(token: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("kb-share/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            token: token.into(),
            api_base: API_BASE.to_string(),
        })
    }

    /// Build from `[share.github]`, reading the token from the configured
    /// environment variable.
    pub fn from_config(cfg: &GithubShareConfig) -> Result<Self> {
        Self::new(cfg.token()?)
    }

    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into();
        self
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// Send + decode a JSON body, mapping any non-2xx to `Error::Share`
    /// with GitHub's `message` field attached.
    async fn send<T: for<'de> Deserialize<'de>>(
        &self,
        rb: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<T> {
        let resp = self.authed(rb).send().await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| truncate(&body, 300));
            return Err(Error::Share(format!("{what}: HTTP {status}: {msg}")));
        }
        serde_json::from_str(&body).map_err(|e| {
            Error::Share(format!(
                "{what}: could not parse GitHub response ({e}): {}",
                truncate(&body, 300)
            ))
        })
    }

    /// Send expecting no body of interest (still surfaces failures).
    async fn send_ok(&self, rb: reqwest::RequestBuilder, what: &str) -> Result<()> {
        let resp = self.authed(rb).send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| truncate(&body, 300));
        Err(Error::Share(format!("{what}: HTTP {status}: {msg}")))
    }

    /// Create a public repo under the authenticated user (`auto_init`
    /// off). Returns the actual owner+name from the response.
    pub async fn create_repo(&self, name: &str, description: &str) -> Result<GithubRepo> {
        let r: RepoResult = self
            .send(
                self.http
                    .post(format!("{}/user/repos", self.api_base))
                    .json(&json!({
                        "name": name,
                        "description": description,
                        "private": false,
                        "auto_init": false,
                        "has_issues": false,
                        "has_wiki": false,
                    })),
                "create-repo",
            )
            .await?;
        Ok(GithubRepo {
            owner: r.owner.login,
            name: r.name,
        })
    }

    async fn create_blob(&self, owner: &str, repo: &str, bytes: &[u8]) -> Result<String> {
        let content = base64::engine::general_purpose::STANDARD.encode(bytes);
        let r: ShaResult = self
            .send(
                self.http
                    .post(format!("{}/repos/{owner}/{repo}/git/blobs", self.api_base))
                    .json(&json!({ "content": content, "encoding": "base64" })),
                "create-blob",
            )
            .await?;
        Ok(r.sha)
    }

    async fn create_tree(&self, owner: &str, repo: &str, entries: Vec<Value>) -> Result<String> {
        let r: ShaResult = self
            .send(
                self.http
                    .post(format!("{}/repos/{owner}/{repo}/git/trees", self.api_base))
                    .json(&json!({ "tree": entries })),
                "create-tree",
            )
            .await?;
        Ok(r.sha)
    }

    async fn create_commit(
        &self,
        owner: &str,
        repo: &str,
        message: &str,
        tree_sha: &str,
    ) -> Result<String> {
        let r: ShaResult = self
            .send(
                self.http
                    .post(format!(
                        "{}/repos/{owner}/{repo}/git/commits",
                        self.api_base
                    ))
                    .json(&json!({ "message": message, "tree": tree_sha, "parents": [] })),
                "create-commit",
            )
            .await?;
        Ok(r.sha)
    }

    async fn create_main_ref(&self, owner: &str, repo: &str, commit_sha: &str) -> Result<()> {
        self.send_ok(
            self.http
                .post(format!("{}/repos/{owner}/{repo}/git/refs", self.api_base))
                .json(&json!({ "ref": "refs/heads/main", "sha": commit_sha })),
            "create-ref",
        )
        .await
    }

    async fn enable_pages(&self, owner: &str, repo: &str) -> Result<()> {
        self.send_ok(
            self.http
                .post(format!("{}/repos/{owner}/{repo}/pages", self.api_base))
                .json(&json!({ "source": { "branch": "main", "path": "/" } })),
            "enable-pages",
        )
        .await
    }

    /// Delete the repo (revoke). Returns Ok even on 404 (already gone).
    pub async fn delete_repo(&self, owner: &str, repo: &str) -> Result<()> {
        let resp = self
            .authed(
                self.http
                    .delete(format!("{}/repos/{owner}/{repo}", self.api_base)),
            )
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() || status.as_u16() == 404 {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(Error::Share(format!(
            "delete-repo {owner}/{repo}: HTTP {status}: {}",
            truncate(&body, 300)
        )))
    }

    /// Create the repo and publish `files` (repo-relative path → bytes) as
    /// a single root commit on `main`, then enable Pages. Returns the repo
    /// + base Pages URL.
    pub async fn publish_dir(
        &self,
        repo_name: &str,
        description: &str,
        files: &[(String, Vec<u8>)],
    ) -> Result<GithubDeploy> {
        let repo = self.create_repo(repo_name, description).await?;
        let mut entries = Vec::with_capacity(files.len());
        for (path, bytes) in files {
            let sha = self.create_blob(&repo.owner, &repo.name, bytes).await?;
            entries.push(tree_entry(path, &sha));
        }
        let tree = self.create_tree(&repo.owner, &repo.name, entries).await?;
        let commit = self
            .create_commit(&repo.owner, &repo.name, "kb share", &tree)
            .await?;
        self.create_main_ref(&repo.owner, &repo.name, &commit)
            .await?;
        self.enable_pages(&repo.owner, &repo.name).await?;
        let url = pages_url(&repo.owner, &repo.name);
        Ok(GithubDeploy {
            owner: repo.owner,
            name: repo.name,
            url,
        })
    }
}

fn truncate(s: &str, max: usize) -> String {
    crate::strutil::truncate_bytes_ellipsis(s, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_url_lowercases_owner_and_keeps_trailing_slash() {
        assert_eq!(
            pages_url("OctoCat", "kb-share-foo-abc"),
            "https://octocat.github.io/kb-share-foo-abc/"
        );
    }

    #[test]
    fn tree_entry_is_a_regular_file_blob() {
        let e = tree_entry("index.html", "deadbeef");
        assert_eq!(e["path"], "index.html");
        assert_eq!(e["mode"], "100644");
        assert_eq!(e["type"], "blob");
        assert_eq!(e["sha"], "deadbeef");
    }
}
