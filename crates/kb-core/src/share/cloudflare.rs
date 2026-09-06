//! Cloudflare client — Pages Direct Upload (native REST, no `wrangler`) +
//! Access (self-hosted app + allow-policy + identity providers) + teardown.
//!
//! ## Deploy sequence (doc 24 §02, corrected against workers-sdk)
//! 1. `POST /accounts/{id}/pages/projects` — ensure the project exists.
//! 2. `GET …/pages/projects/{name}/upload-token` — short-lived upload JWT.
//! 3. `POST /pages/assets/check-missing` (bearer = JWT) — which hashes are new.
//! 4. `POST /pages/assets/upload?base64=true` (bearer = JWT) — upload the new ones.
//! 5. `POST /pages/assets/upsert-hashes` (bearer = JWT) — register the full set.
//! 6. `POST …/pages/projects/{name}/deployments` — create the deployment.
//!
//! ## Gate (doc 24 §03)
//! `POST /accounts/{id}/access/apps` (self_hosted, domain = `<project>.pages.dev`)
//! then `POST …/access/apps/{app}/policies` (decision=allow, include=[…]).
//!
//! ## Verification status
//! - The **asset hash** ([`asset_hash`]) is VERIFIED byte-for-byte against
//!   `cloudflare/workers-sdk` `packages/wrangler/src/pages/hash.ts`.
//! - The exact JSON request/response **shapes** of the live endpoints are
//!   the well-trodden-but-undocumented Direct Upload API; they are pinned
//!   by the `#[ignore]`d live integration test (plan R9) against a real
//!   account, not by CI. Each method documents the shape it sends.

use crate::config::CloudflareShareConfig;
use crate::{Error, Result};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

const API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// wrangler-compatible Pages asset hash: **BLAKE3 of (base64(file_bytes) ‖
/// extension-without-dot), hex, first 32 chars**. VERIFIED against
/// `cloudflare/workers-sdk` `packages/wrangler/src/pages/hash.ts`:
///
/// ```js
/// blake3hash(base64Contents + extension).toString("hex").slice(0, 32)
/// ```
///
/// where `base64Contents = Buffer.from(bytes).toString("base64")` (standard
/// alphabet, padded) and `extension = extname(path).substring(1)` (no dot,
/// `""` when absent). Note doc 24's prose omits the base64 step — this
/// function follows the source, not the prose.
pub fn asset_hash(bytes: &[u8], ext: &str) -> String {
    let mut input = base64::engine::general_purpose::STANDARD.encode(bytes);
    input.push_str(ext);
    let digest = blake3::hash(input.as_bytes());
    // First 32 hex chars == hex of the first 16 bytes (lowercase, matching
    // wrangler's `.toString("hex").slice(0,32)`).
    hex::encode(&digest.as_bytes()[..16])
}

/// Extension without the leading dot, matching Node's
/// `extname(path).substring(1)`: `"html"` for `foo.html`, `""` for a file
/// with no extension or a leading-dot dotfile.
pub fn path_extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// Best-effort content type from a file extension. Covers the static-site
/// types a kb artifact bundle uses; everything else is octet-stream.
pub fn content_type_for(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Map one `--gate` token to Access policy `include` rules (doc 24 §03).
/// A comma list under `email:` expands to several rules; all rules across
/// all `--gate` tokens go into the policy's `include` array (OR semantics,
/// deny-by-default). Returns `BadRequest` for an unknown token or a
/// `google`/`github` gate with no registered IdP UUID configured.
///
/// | token                | rule                                               |
/// |----------------------|----------------------------------------------------|
/// | `email:example.com`  | `{email_domain:{domain:"example.com"}}`             |
/// | `email:a@x,b@y`      | `{email:{email:"a@x"}}`, `{email:{email:"b@y"}}`    |
/// | `google`             | `{login_method:{id:<google-idp-uuid>}}`             |
/// | `github`             | `{login_method:{id:<github-idp-uuid>}}`             |
pub fn gate_to_include_rules(
    gate: &str,
    google_idp: Option<&str>,
    github_idp: Option<&str>,
) -> Result<Vec<Value>> {
    let gate = gate.trim();
    if let Some(spec) = gate.strip_prefix("email:") {
        let rules: Vec<Value> = spec
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|part| {
                if part.contains('@') {
                    json!({ "email": { "email": part } })
                } else {
                    json!({ "email_domain": { "domain": part } })
                }
            })
            .collect();
        if rules.is_empty() {
            return Err(Error::BadRequest(format!("empty email gate: {gate:?}")));
        }
        Ok(rules)
    } else if gate == "google" {
        let id = google_idp.ok_or_else(|| {
            Error::BadRequest(
                "gate 'google' needs [share.cloudflare] google_idp (the registered IdP UUID)"
                    .into(),
            )
        })?;
        Ok(vec![json!({ "login_method": { "id": id } })])
    } else if gate == "github" {
        let id = github_idp.ok_or_else(|| {
            Error::BadRequest(
                "gate 'github' needs [share.cloudflare] github_idp (the registered IdP UUID)"
                    .into(),
            )
        })?;
        Ok(vec![json!({ "login_method": { "id": id } })])
    } else {
        Err(Error::BadRequest(format!(
            "unknown --gate {gate:?}: expected email:DOMAIN | email:a@x,b@y | google | github"
        )))
    }
}

/// Standard Cloudflare API envelope. The `result` is absent on failure.
#[derive(Deserialize)]
struct CfEnvelope<T> {
    success: bool,
    #[serde(default)]
    errors: Vec<CfError>,
    // `Option<T>` already deserialises a missing key to `None` — no
    // `#[serde(default)]` (which would wrongly require `T: Default`).
    result: Option<T>,
}

#[derive(Deserialize)]
struct CfError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

impl CfError {
    fn joined(errors: &[CfError]) -> String {
        if errors.is_empty() {
            return "no error detail".into();
        }
        errors
            .iter()
            .map(|e| format!("{}: {}", e.code, e.message))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// One asset in the Direct Upload payload. Serialised as
/// `{ key, value, metadata: { contentType }, base64: true }` — the element
/// shape `POST /pages/assets/upload` expects.
#[derive(serde::Serialize)]
pub struct AssetUpload {
    pub key: String,
    pub value: String,
    pub metadata: AssetMetadata,
    pub base64: bool,
}

#[derive(serde::Serialize)]
pub struct AssetMetadata {
    #[serde(rename = "contentType")]
    pub content_type: String,
}

/// Outcome of a Pages deployment.
#[derive(Debug, Clone)]
pub struct Deployment {
    pub id: String,
    pub url: String,
}

#[derive(Deserialize)]
struct DeploymentResult {
    id: String,
    url: String,
}

#[derive(Deserialize)]
struct UploadTokenResult {
    jwt: String,
}

#[derive(Deserialize)]
struct IdProvider {
    id: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Deserialize)]
struct WithId {
    id: String,
}

/// Cloudflare REST client scoped to one account. Holds the account API
/// token; the short-lived Pages upload JWT is passed per call.
pub struct CloudflareClient {
    http: reqwest::Client,
    account_id: String,
    api_token: String,
    api_base: String,
}

impl CloudflareClient {
    /// Build a client from the account id + API token. The token is the
    /// custom token with Pages:Edit + Access: Apps and Policies:Edit +
    /// Access: Organizations, Identity Providers, and Groups:Edit (doc 24 §01).
    pub fn new(account_id: impl Into<String>, api_token: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("kb-share/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            account_id: account_id.into(),
            api_token: api_token.into(),
            api_base: API_BASE.to_string(),
        })
    }

    /// Build from the resolved `[share.cloudflare]` config, reading the API
    /// token from the configured environment variable.
    pub fn from_config(cfg: &CloudflareShareConfig) -> Result<Self> {
        Self::new(cfg.account_id.clone(), cfg.api_token()?)
    }

    /// Override the API base (tests / a mock server).
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into();
        self
    }

    fn acct_url(&self, suffix: &str) -> String {
        format!("{}/accounts/{}/{}", self.api_base, self.account_id, suffix)
    }

    /// Send a request, check the HTTP status + CF envelope `success`, and
    /// return the `result`. Maps any failure to `Error::Share` with the
    /// upstream message attached.
    async fn send<T: for<'de> Deserialize<'de>>(
        &self,
        rb: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<T> {
        let resp = rb.send().await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let env: CfEnvelope<T> = serde_json::from_str(&body).map_err(|e| {
            Error::Share(format!(
                "{what}: HTTP {status}: could not parse Cloudflare response ({e}): {}",
                truncate(&body, 300)
            ))
        })?;
        if !status.is_success() || !env.success {
            return Err(Error::Share(format!(
                "{what}: HTTP {status}: {}",
                CfError::joined(&env.errors)
            )));
        }
        env.result
            .ok_or_else(|| Error::Share(format!("{what}: Cloudflare returned no result")))
    }

    /// Like [`send`], but for endpoints whose success response carries no
    /// payload (CF returns `{"success": true, "result": null}` for several
    /// write-only / delete endpoints — `upload`, `upsert-hashes`, `delete`).
    /// `send` would treat the null as a failure; this variant only checks
    /// HTTP status + envelope `success`.
    async fn send_void(&self, rb: reqwest::RequestBuilder, what: &str) -> Result<()> {
        let resp = rb.send().await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let env: CfEnvelope<Value> = serde_json::from_str(&body).map_err(|e| {
            Error::Share(format!(
                "{what}: HTTP {status}: could not parse Cloudflare response ({e}): {}",
                truncate(&body, 300)
            ))
        })?;
        if !status.is_success() || !env.success {
            return Err(Error::Share(format!(
                "{what}: HTTP {status}: {}",
                CfError::joined(&env.errors)
            )));
        }
        Ok(())
    }

    // --- Pages Direct Upload ---------------------------------------------

    /// Ensure the Pages project exists. Idempotent: an "already exists"
    /// error (the project was created by a prior share) is treated as OK.
    pub async fn ensure_project(&self, name: &str) -> Result<()> {
        let resp = self
            .http
            .post(self.acct_url("pages/projects"))
            .bearer_auth(&self.api_token)
            .json(&json!({ "name": name, "production_branch": "main" }))
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // Already-exists is fine (re-share / --update). Anything else fails.
        let body = resp.text().await.unwrap_or_default();
        if body.contains("already exists") || status.as_u16() == 409 {
            return Ok(());
        }
        Err(Error::Share(format!(
            "ensure_project {name:?}: HTTP {status}: {}",
            truncate(&body, 300)
        )))
    }

    /// Fetch the short-lived (~minutes) Pages upload JWT for `project`.
    /// Re-fetch close to upload; a large staging dir can outlive it.
    pub async fn upload_token(&self, project: &str) -> Result<String> {
        let url = self.acct_url(&format!("pages/projects/{project}/upload-token"));
        let r: UploadTokenResult = self
            .send(
                self.http.get(url).bearer_auth(&self.api_token),
                "upload-token",
            )
            .await?;
        Ok(r.jwt)
    }

    /// `POST /pages/assets/check-missing` (bearer = upload JWT). Sends all
    /// hashes, returns the subset Cloudflare still needs.
    pub async fn check_missing(&self, jwt: &str, hashes: &[String]) -> Result<Vec<String>> {
        let url = format!("{}/pages/assets/check-missing", self.api_base);
        self.send(
            self.http
                .post(url)
                .bearer_auth(jwt)
                .json(&json!({ "hashes": hashes })),
            "check-missing",
        )
        .await
    }

    /// `POST /pages/assets/upload?base64=true` (bearer = upload JWT). Body
    /// is a JSON array of [`AssetUpload`]. Caller batches into reasonably
    /// sized chunks.
    pub async fn upload_assets(&self, jwt: &str, batch: &[AssetUpload]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let url = format!("{}/pages/assets/upload", self.api_base);
        self.send_void(
            self.http.post(url).bearer_auth(jwt).json(batch),
            "assets/upload",
        )
        .await
    }

    /// `POST /pages/assets/upsert-hashes` (bearer = upload JWT). Registers
    /// the complete hash set before the deployment is created — the step
    /// doc 24 omitted but wrangler performs.
    pub async fn upsert_hashes(&self, jwt: &str, hashes: &[String]) -> Result<()> {
        let url = format!("{}/pages/assets/upsert-hashes", self.api_base);
        self.send_void(
            self.http
                .post(url)
                .bearer_auth(jwt)
                .json(&json!({ "hashes": hashes })),
            "upsert-hashes",
        )
        .await
    }

    /// `POST …/pages/projects/{name}/deployments` with the asset manifest
    /// (`{ "/path": hash }`) as a multipart `manifest` field. Returns the
    /// deployment id + url.
    pub async fn create_deployment(
        &self,
        project: &str,
        manifest: &BTreeMap<String, String>,
    ) -> Result<Deployment> {
        let url = self.acct_url(&format!("pages/projects/{project}/deployments"));
        let manifest_json = serde_json::to_string(manifest)?;
        let form = reqwest::multipart::Form::new().text("manifest", manifest_json);
        let r: DeploymentResult = self
            .send(
                self.http
                    .post(url)
                    .bearer_auth(&self.api_token)
                    .multipart(form),
                "create-deployment",
            )
            .await?;
        Ok(Deployment {
            id: r.id,
            url: r.url,
        })
    }

    // --- Access ----------------------------------------------------------

    /// Ensure an identity provider of `kind` (e.g. `onetimepin`) exists,
    /// returning its id. Looks for an existing one first; creates it with
    /// an empty config if absent (only meaningful for `onetimepin` — OAuth
    /// IdPs are pre-registered out of band, doc 24 §01).
    pub async fn ensure_identity_provider(&self, kind: &str) -> Result<String> {
        let list: Vec<IdProvider> = self
            .send(
                self.http
                    .get(self.acct_url("access/identity_providers"))
                    .bearer_auth(&self.api_token),
                "list-identity-providers",
            )
            .await?;
        if let Some(idp) = list.into_iter().find(|p| p.kind == kind) {
            return Ok(idp.id);
        }
        let created: WithId = self
            .send(
                self.http
                    .post(self.acct_url("access/identity_providers"))
                    .bearer_auth(&self.api_token)
                    .json(&json!({ "name": kind, "type": kind, "config": {} })),
                "create-identity-provider",
            )
            .await?;
        Ok(created.id)
    }

    /// Create a self-hosted Access app over the exact hostname (the
    /// canonical `<project>.pages.dev`, never just previews — doc 24 §04).
    /// Returns the app id.
    pub async fn create_access_app(&self, name: &str, domain: &str) -> Result<String> {
        let created: WithId = self
            .send(
                self.http
                    .post(self.acct_url("access/apps"))
                    .bearer_auth(&self.api_token)
                    .json(&json!({
                        "name": name,
                        "type": "self_hosted",
                        "domain": domain,
                        "session_duration": "24h",
                    })),
                "create-access-app",
            )
            .await?;
        Ok(created.id)
    }

    /// Create an allow-policy on the app. `include` is the OR-list of rules
    /// from [`gate_to_include_rules`]. Access is deny-by-default, so the
    /// allow-rule IS the gate. Returns the policy id.
    pub async fn create_access_policy(
        &self,
        app_id: &str,
        name: &str,
        include: Vec<Value>,
    ) -> Result<String> {
        let created: WithId = self
            .send(
                self.http
                    .post(self.acct_url(&format!("access/apps/{app_id}/policies")))
                    .bearer_auth(&self.api_token)
                    .json(&json!({
                        "name": name,
                        "decision": "allow",
                        "include": include,
                    })),
                "create-access-policy",
            )
            .await?;
        Ok(created.id)
    }

    // --- Teardown --------------------------------------------------------

    /// Delete the Access app (and its policies). Drop this before the
    /// project so no app is left pointing at a dead hostname.
    pub async fn delete_access_app(&self, app_id: &str) -> Result<()> {
        self.send_void(
            self.http
                .delete(self.acct_url(&format!("access/apps/{app_id}")))
                .bearer_auth(&self.api_token),
            "delete-access-app",
        )
        .await
    }

    /// Delete the Pages project (removes the bytes entirely).
    pub async fn delete_project(&self, name: &str) -> Result<()> {
        self.send_void(
            self.http
                .delete(self.acct_url(&format!("pages/projects/{name}")))
                .bearer_auth(&self.api_token),
            "delete-project",
        )
        .await
    }
}

fn truncate(s: &str, max: usize) -> String {
    crate::strutil::truncate_bytes_ellipsis(s, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- asset hash (the verified core) -----------------------------------

    #[test]
    fn asset_hash_is_32_lowercase_hex() {
        let h = asset_hash(b"<html></html>", "html");
        assert_eq!(h.len(), 32);
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn asset_hash_is_deterministic() {
        assert_eq!(asset_hash(b"abc", "css"), asset_hash(b"abc", "css"));
    }

    #[test]
    fn asset_hash_depends_on_extension() {
        // The extension is folded into the hash — same bytes, different ext
        // must key differently (or Pages serves the wrong content type).
        assert_ne!(asset_hash(b"abc", "html"), asset_hash(b"abc", "css"));
    }

    #[test]
    fn asset_hash_depends_on_content() {
        assert_ne!(asset_hash(b"abc", "html"), asset_hash(b"abd", "html"));
    }

    // invariant:9 asset-hash
    #[test]
    fn asset_hash_base64_encodes_first() {
        // Guards the doc-24 trap: the bytes are base64-encoded BEFORE
        // hashing. Hashing raw-bytes‖ext would produce a different value;
        // assert we are NOT doing that.
        let raw = hex::encode(&blake3::hash(b"abchtml").as_bytes()[..16]);
        assert_ne!(
            asset_hash(b"abc", "html"),
            raw,
            "asset_hash must base64-encode the bytes before hashing"
        );
    }

    #[test]
    fn path_extension_matches_node_extname_substring() {
        assert_eq!(path_extension(Path::new("index.html")), "html");
        assert_eq!(path_extension(Path::new("a/b/style.css")), "css");
        assert_eq!(path_extension(Path::new("archive.tar.gz")), "gz");
        assert_eq!(path_extension(Path::new("LICENSE")), "");
        assert_eq!(path_extension(Path::new(".gitignore")), "");
        assert_eq!(path_extension(Path::new("FOO.HTML")), "HTML"); // case preserved
    }

    // --- gate → include rules (doc 24 §03) --------------------------------

    #[test]
    fn gate_email_domain_rule() {
        let rules = gate_to_include_rules("email:example.com", None, None).unwrap();
        assert_eq!(
            rules,
            vec![json!({"email_domain": {"domain": "example.com"}})]
        );
    }

    #[test]
    fn gate_email_list_expands_to_per_address_rules() {
        let rules = gate_to_include_rules("email:a@x.com,b@y.com", None, None).unwrap();
        assert_eq!(
            rules,
            vec![
                json!({"email": {"email": "a@x.com"}}),
                json!({"email": {"email": "b@y.com"}}),
            ]
        );
    }

    #[test]
    fn gate_email_mixed_domain_and_address() {
        let rules =
            gate_to_include_rules("email:example.com,vip@elsewhere.com", None, None).unwrap();
        assert_eq!(
            rules,
            vec![
                json!({"email_domain": {"domain": "example.com"}}),
                json!({"email": {"email": "vip@elsewhere.com"}}),
            ]
        );
    }

    #[test]
    fn gate_google_needs_idp_uuid() {
        assert!(gate_to_include_rules("google", None, None).is_err());
        let rules = gate_to_include_rules("google", Some("g-uuid"), None).unwrap();
        assert_eq!(rules, vec![json!({"login_method": {"id": "g-uuid"}})]);
    }

    #[test]
    fn gate_github_needs_idp_uuid() {
        assert!(gate_to_include_rules("github", None, None).is_err());
        let rules = gate_to_include_rules("github", None, Some("gh-uuid")).unwrap();
        assert_eq!(rules, vec![json!({"login_method": {"id": "gh-uuid"}})]);
    }

    #[test]
    fn gate_unknown_token_errors() {
        assert!(gate_to_include_rules("everyone", None, None).is_err());
        assert!(gate_to_include_rules("email:", None, None).is_err());
    }

    #[test]
    fn content_type_covers_artifact_bundle() {
        assert_eq!(content_type_for("html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("CSS"), "text/css; charset=utf-8");
        assert_eq!(content_type_for("png"), "image/png");
        assert_eq!(content_type_for("weird"), "application/octet-stream");
    }
}
