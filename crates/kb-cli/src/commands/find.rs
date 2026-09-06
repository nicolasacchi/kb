//! `kb find <input> [--kb NAME] [--json] [--daemon URL]` — resolve a
//! user-supplied identifier (12-hex id, source-relative path, or
//! unique filename suffix) to an artifact via the daemon's `/lookup`
//! endpoint. Prints the id on success, exits 1 with candidates on
//! ambiguity, exits 2 on no match.
//!
//! Composes in shell pipelines (`kb find atlas.html | xargs kb cat`)
//! and underlies the `--path` flag on `kb comments` subcommands.

use anyhow::Result;

pub async fn run(
    input: &str,
    kb: Option<&str>,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let body = crate::http::get_lookup(daemon, &resolved_kb, input, bearer).await?;

    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match kind {
        "exact" | "unique_suffix" => {
            if json {
                println!("{}", serde_json::to_string(&body)?);
            } else {
                let id = body
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("lookup response missing `id`"))?;
                println!("{id}");
            }
            Ok(())
        }
        "ambiguous" => {
            let candidates = body
                .get("candidates")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            eprintln!(
                "ambiguous: {} matches for {:?} (kb={})",
                candidates.len(),
                input,
                resolved_kb
            );
            for c in candidates {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let rel = c
                    .get("source_relative")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                eprintln!("  {id}  {rel}");
            }
            if body
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                eprintln!("  …(truncated)");
            }
            if json {
                println!("{}", serde_json::to_string(&body)?);
            }
            std::process::exit(1);
        }
        "not_found" => {
            eprintln!("no match: {input:?} (kb={resolved_kb})");
            std::process::exit(2);
        }
        other => {
            anyhow::bail!("lookup returned unknown kind {other:?}: {body}");
        }
    }
}
