//! Share backend — host-agnostic publish/teardown over the Cloudflare and
//! GitHub clients. An enum (not a `dyn` trait): Rust async-fn-in-trait
//! doesn't object-dispatch cleanly, and the two hosts sequence very
//! differently (Cloudflare deploys *and* gates; GitHub only deploys). A
//! `#[cfg(test)]` `Fake` variant lets the engine's staging logic be
//! exercised without network.

use crate::config::CloudflareShareConfig;
use crate::share::cloudflare::{
    asset_hash, content_type_for, gate_to_include_rules, path_extension, AssetMetadata,
    AssetUpload, CloudflareClient,
};
use crate::share::github::GithubClient;
use crate::storage::sqlite::ShareRow;
use crate::Result;
use base64::Engine as _;
use std::collections::BTreeMap;
use std::path::Path;

/// Host-specific identifiers a publish produces, recorded in the registry.
#[derive(Debug, Clone, Default)]
pub struct PublishResult {
    /// Base deployment URL (ends with `/`). The engine appends the entry path.
    pub url: String,
    pub cf_account_id: Option<String>,
    pub pages_project: Option<String>,
    pub cf_deployment_id: Option<String>,
    pub access_app_id: Option<String>,
    pub access_policy_id: Option<String>,
    pub github_repo: Option<String>,
}

/// A configured static host the engine publishes to.
pub enum ShareBackend {
    Cloudflare {
        client: CloudflareClient,
        cfg: CloudflareShareConfig,
    },
    Github {
        client: GithubClient,
    },
    #[cfg(test)]
    Fake(std::sync::Arc<std::sync::Mutex<FakeState>>),
}

impl ShareBackend {
    /// Build the configured backend for `host` from the `[share]` section.
    /// The host's API token is read from the daemon's environment (errors
    /// if the host isn't configured, or the token env var is unset).
    pub fn from_config(
        host: super::ShareHostKind,
        share: &crate::config::ShareSection,
    ) -> crate::Result<Self> {
        match host {
            super::ShareHostKind::CloudflarePages => {
                let cfg = share.cloudflare.as_ref().ok_or_else(|| {
                    crate::Error::BadRequest(
                        "cloudflare-pages is not configured: add [share.cloudflare] to kb.toml"
                            .into(),
                    )
                })?;
                Ok(ShareBackend::Cloudflare {
                    client: CloudflareClient::from_config(cfg)?,
                    cfg: cfg.clone(),
                })
            }
            super::ShareHostKind::GithubPages => {
                let cfg = share.github.as_ref().ok_or_else(|| {
                    crate::Error::BadRequest(
                        "github-pages is not configured: add [share.github] to kb.toml".into(),
                    )
                })?;
                Ok(ShareBackend::Github {
                    client: GithubClient::from_config(cfg)?,
                })
            }
        }
    }

    /// Publish `files` (deployment-relative path → bytes) under `name`.
    /// `gate` is the list of `--gate` tokens (empty = ungated/public);
    /// applied only by the Cloudflare backend. `existing` is the recorded
    /// row from a prior deploy when `--update` is in effect; it lets the
    /// Cloudflare backend reuse the already-created Access app + policy
    /// (CF rejects a second `create_access_app` for the same domain with
    /// HTTP 409). When `existing` is `None` (first deploy), the gate is
    /// applied fresh.
    pub async fn publish(
        &self,
        name: &str,
        files: &[(String, Vec<u8>)],
        gate: &[String],
        existing: Option<&ShareRow>,
    ) -> Result<PublishResult> {
        match self {
            ShareBackend::Cloudflare { client, cfg } => {
                publish_cloudflare(client, cfg, name, files, gate, existing).await
            }
            ShareBackend::Github { client } => {
                let d = client.publish_dir(name, "kb share", files).await?;
                Ok(PublishResult {
                    url: d.url,
                    github_repo: Some(format!("{}/{}", d.owner, d.name)),
                    ..Default::default()
                })
            }
            #[cfg(test)]
            ShareBackend::Fake(state) => {
                state.lock().unwrap().published =
                    Some((name.to_string(), files.to_vec(), gate.to_vec()));
                Ok(PublishResult {
                    url: format!("https://fake.test/{name}/"),
                    pages_project: Some(name.to_string()),
                    ..Default::default()
                })
            }
        }
    }

    /// Tear down a share, undoing exactly what its registry row records.
    /// Cloudflare: delete the Access app (and its policy) first, then the
    /// project. GitHub: delete the repo. The shared IdP is never deleted.
    pub async fn teardown(&self, row: &ShareRow) -> Result<()> {
        match self {
            ShareBackend::Cloudflare { client, .. } => {
                if let Some(app) = &row.access_app_id {
                    client.delete_access_app(app).await?;
                }
                if let Some(project) = &row.pages_project {
                    client.delete_project(project).await?;
                }
                Ok(())
            }
            ShareBackend::Github { client } => {
                if let Some(repo) = &row.github_repo {
                    if let Some((owner, name)) = repo.split_once('/') {
                        client.delete_repo(owner, name).await?;
                    }
                }
                Ok(())
            }
            #[cfg(test)]
            ShareBackend::Fake(state) => {
                state.lock().unwrap().revoked.push(row.name.clone());
                Ok(())
            }
        }
    }
}

async fn publish_cloudflare(
    client: &CloudflareClient,
    cfg: &CloudflareShareConfig,
    name: &str,
    files: &[(String, Vec<u8>)],
    gate: &[String],
    existing: Option<&ShareRow>,
) -> Result<PublishResult> {
    client.ensure_project(name).await?;

    // Build the path→hash manifest + a hash→(bytes,ext) dedup map.
    let mut manifest: BTreeMap<String, String> = BTreeMap::new();
    let mut by_hash: BTreeMap<String, (Vec<u8>, String)> = BTreeMap::new();
    for (path, bytes) in files {
        let ext = path_extension(Path::new(path));
        let hash = asset_hash(bytes, &ext);
        manifest.insert(format!("/{path}"), hash.clone());
        by_hash.entry(hash).or_insert_with(|| (bytes.clone(), ext));
    }

    let jwt = client.upload_token(name).await?;
    let all_hashes: Vec<String> = by_hash.keys().cloned().collect();
    let missing = client.check_missing(&jwt, &all_hashes).await?;
    let payloads: Vec<AssetUpload> = missing
        .iter()
        .filter_map(|h| {
            by_hash.get(h).map(|(bytes, ext)| AssetUpload {
                key: h.clone(),
                value: base64::engine::general_purpose::STANDARD.encode(bytes),
                metadata: AssetMetadata {
                    content_type: content_type_for(ext).to_string(),
                },
                base64: true,
            })
        })
        .collect();
    for chunk in payloads.chunks(50) {
        client.upload_assets(&jwt, chunk).await?;
    }
    client.upsert_hashes(&jwt, &all_hashes).await?;
    let deployment = client.create_deployment(name, &manifest).await?;

    let (access_app_id, access_policy_id) = if gate.is_empty() {
        (None, None)
    } else if let Some(row) = existing.filter(|r| r.access_app_id.is_some()) {
        // --update: an Access app already covers this domain. CF rejects
        // a second create_access_app on the same hostname with HTTP 409,
        // so reuse the recorded id (and its policy) verbatim. Changing
        // the gate on an existing share is not supported in v1 — revoke
        // first if you need different rules.
        (row.access_app_id.clone(), row.access_policy_id.clone())
    } else {
        let mut include = Vec::new();
        for token in gate {
            include.extend(gate_to_include_rules(
                token,
                cfg.google_idp.as_deref(),
                cfg.github_idp.as_deref(),
            )?);
        }
        // Email gates ride Cloudflare's One-Time PIN; ensure it's on
        // (best-effort — it's usually enabled by default).
        if gate.iter().any(|g| g.starts_with("email:")) {
            let _ = client.ensure_identity_provider("onetimepin").await;
        }
        let domain = format!("{name}.pages.dev");
        let app = client.create_access_app(name, &domain).await?;
        let policy = client.create_access_policy(&app, name, include).await?;
        (Some(app), Some(policy))
    };

    Ok(PublishResult {
        url: format!("https://{name}.pages.dev/"),
        cf_account_id: Some(cfg.account_id.clone()),
        pages_project: Some(name.to_string()),
        cf_deployment_id: Some(deployment.id),
        access_app_id,
        access_policy_id,
        github_repo: None,
    })
}

/// `(share name, deployment files, gate tokens)` recorded by a fake publish.
#[cfg(test)]
pub type FakePublish = (String, Vec<(String, Vec<u8>)>, Vec<String>);

/// Records what the [`ShareBackend::Fake`] backend was asked to do.
#[cfg(test)]
#[derive(Default)]
pub struct FakeState {
    /// The last `publish` call's args.
    pub published: Option<FakePublish>,
    /// Names passed to `teardown`.
    pub revoked: Vec<String>,
}
