//! `kb pull --from URL --kb NAME --into DIR` — one-shot fetch of every
//! artifact in a remote kb that's missing from a local folder, written
//! as `<id>.html` for the local daemon's watcher to ingest.
//!
//! Built for pulling from a kb behind Authelia (e.g. kb.example.com). When
//! `--oidc-token-url` + `--oidc-client-id` are supplied (and the
//! `KB_OIDC_CLIENT_SECRET` env var is set), pull first exchanges the
//! `client_credentials` grant for an Authelia access token and presents
//! it as the bearer. Authelia's forward-auth validates that token; the
//! kb daemon upstream sees the bearer Traefik injects for it
//! (`kb-inject-bearer`), so no kb-server change is needed.
//!
//! Without the OIDC flags, pull falls back to the local kb bearer token
//! (`~/.config/kb/token`) — the same auth path every other CLI verb
//! uses against an auth-on daemon.

use crate::commands::fleet::{fetch_artifact_bytes, fetch_docs};
use crate::http::{client_with_timeout_and_bearer, encode_path_segment, fetch_oauth_token};
use anyhow::{Context, Result};
use std::path::Path;

/// Env var holding the OIDC client secret. Kept out of argv so it can be
/// supplied via the Proton Pass `pass-run.sh` wrapper without landing in
/// shell history or the process table.
const OIDC_SECRET_ENV: &str = "KB_OIDC_CLIENT_SECRET";

#[allow(clippy::too_many_arguments)]
pub async fn run(
    from: &str,
    kb: &str,
    into: &Path,
    oidc_token_url: Option<&str>,
    oidc_client_id: Option<&str>,
    scope: &str,
    fallback_bearer: Option<&str>,
) -> Result<()> {
    let base = from.trim_end_matches('/');

    // The OIDC token's audience must prefix-match the URLs we'll call,
    // so it's the kb origin itself — i.e. `--from`.
    let bearer =
        resolve_bearer(oidc_token_url, oidc_client_id, scope, base, fallback_bearer).await?;
    let client = client_with_timeout_and_bearer(30, bearer.as_deref())?;

    let docs_url = format!("{base}/api/kb/{}/docs?limit=10000", encode_path_segment(kb));
    let rows = fetch_docs(&client, &docs_url)
        .await
        .with_context(|| format!("list remote docs from {docs_url}"))?;

    std::fs::create_dir_all(into)
        .with_context(|| format!("create --into dir {}", into.display()))?;

    let mut pulled = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;
    for row in &rows {
        // Identity is the content hash, so a same-named local file is a
        // genuine duplicate — skip without re-downloading.
        let dest = into.join(format!("{}.html", row.id));
        if dest.exists() {
            skipped += 1;
            continue;
        }
        let url = format!(
            "{base}/api/kb/{}/artifact/{}",
            encode_path_segment(kb),
            encode_path_segment(&row.id)
        );
        match fetch_artifact_bytes(&client, &url).await {
            Ok(bytes) => {
                std::fs::write(&dest, &bytes)
                    .with_context(|| format!("write {} ({} bytes)", dest.display(), bytes.len()))?;
                pulled += 1;
            }
            Err(e) => {
                eprintln!("  fail {}: {e}", row.id);
                failed += 1;
            }
        }
    }

    println!(
        "pull from {base} kb={kb}: pulled {pulled}, skipped {skipped}, failed {failed} (into {})",
        into.display()
    );
    if failed > 0 {
        anyhow::bail!("{failed} artifact(s) failed to download");
    }
    Ok(())
}

/// OIDC client_credentials when both `--oidc-*` flags are present, else
/// the local kb token. Supplying only one of the pair is a usage error.
async fn resolve_bearer(
    oidc_token_url: Option<&str>,
    oidc_client_id: Option<&str>,
    scope: &str,
    audience: &str,
    fallback_bearer: Option<&str>,
) -> Result<Option<String>> {
    match (oidc_token_url, oidc_client_id) {
        (Some(token_url), Some(client_id)) => {
            let secret = std::env::var(OIDC_SECRET_ENV).map_err(|_| {
                anyhow::anyhow!(
                    "OIDC client configured but {OIDC_SECRET_ENV} is unset — \
                     supply the client secret via the environment"
                )
            })?;
            let token = fetch_oauth_token(token_url, client_id, &secret, scope, audience)
                .await
                .context("fetch OIDC access token")?;
            Ok(Some(token))
        }
        (None, None) => Ok(fallback_bearer.map(str::to_string)),
        _ => anyhow::bail!("--oidc-token-url and --oidc-client-id must be supplied together"),
    }
}
