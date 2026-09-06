//! `kb share` — publish an artifact/folder to a gated/public static host,
//! `kb share list`, `kb share revoke <name>`. HTTP-only, like `download` /
//! `comments`: every verb talks to the daemon (`POST /api/kb/{kb}/share`,
//! `GET …/shares`, `DELETE …/shares/{name}`), which runs the engine
//! server-side. The host API tokens live in the *daemon's* environment,
//! not here.
//!
//! `<target>` is a source-relative file or folder path (the daemon joins
//! it to the kb's source root).

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

use crate::http::{client_with_timeout_and_bearer, encode_path_segment, resolve_default_kb};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

/// Deploys can take a while (upload + Pages build); there is no
/// server-side request timeout, so the client allows a generous window.
const DEPLOY_TIMEOUT_SECS: u64 = 600;

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// POST/GET/DELETE helper mirroring `comments::send_json`: surface the
/// daemon's problem+json `detail` on a non-2xx.
async fn send_json(req: reqwest::RequestBuilder, what: &str) -> Result<Value> {
    let resp = req.send().await.with_context(|| what.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or(text);
        anyhow::bail!("{what} failed: HTTP {status} — {detail}");
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// `kb share <target> [flags]` — publish (deploy + gate).
#[allow(clippy::too_many_arguments)]
pub async fn create(
    target: &str,
    kb: Option<&str>,
    host: &str,
    gate: &[String],
    public: bool,
    links: &str,
    update: bool,
    no_scrub: bool,
    with_comments: bool,
    open: bool,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let kb_name = resolve_default_kb(kb, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/share",
        base_url(daemon),
        encode_path_segment(&kb_name)
    );
    let body = json!({
        "target": target,
        "host": host,
        "gate": gate,
        "public": public,
        "links": links,
        "update": update,
        "no_scrub": no_scrub,
        "include_comments": with_comments,
    });
    let client = client_with_timeout_and_bearer(DEPLOY_TIMEOUT_SECS, bearer)?;
    let resp = send_json(client.post(&url).json(&body), "share").await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let share_url = resp["url"].as_str().unwrap_or_default();
    let verb = if resp["updated"].as_bool().unwrap_or(false) {
        "updated"
    } else {
        "shared"
    };
    println!("{verb} {share_url}");
    if let Some(danglers) = resp["danglers"].as_array() {
        if !danglers.is_empty() {
            eprintln!(
                "warning: {} cross-artifact link(s) point outside the share \
                 (use --links absolute to rewrite them, or add them to the share):",
                danglers.len()
            );
            for d in danglers {
                if let Some(id) = d.as_str() {
                    eprintln!("  {id}");
                }
            }
        }
    }
    if open && !share_url.is_empty() {
        if let Err(e) = open_in_browser(share_url) {
            eprintln!("(could not open browser: {e})");
        }
    }
    Ok(())
}

/// `kb share <target> --local <path>` — fetch a self-contained OFFLINE bundle
/// (scrubbed + in-share links relativized) and write/extract it locally. The
/// daemon stages + zips; we write the `.zip` (when `out` ends in `.zip`) or
/// extract it into `out` as a directory. The bytes land on THIS machine (not
/// the daemon host) — the right shape for a remote daemon.
#[allow(clippy::too_many_arguments)]
pub async fn export_local(
    target: &str,
    out: &Path,
    kb: Option<&str>,
    links: &str,
    no_scrub: bool,
    with_comments: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let kb_name = resolve_default_kb(kb, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/share/export",
        base_url(daemon),
        encode_path_segment(&kb_name)
    );
    let body = json!({
        "target": target,
        "links": links,
        "no_scrub": no_scrub,
        "include_comments": with_comments,
    });
    download_share_zip(&url, &body, out, "share export", bearer).await
}

/// `kb share --list <id-or-title> --local <path>` — export a whole reading list
/// as a self-contained offline bundle (ordered entries + generated TOC).
/// Resolves the list like `kb list show` (exact `l_…` id or case-insensitive
/// unique title). Mutually exclusive with a path target.
#[allow(clippy::too_many_arguments)]
pub async fn export_list(
    list: &str,
    out: &Path,
    kb: Option<&str>,
    links: &str,
    no_scrub: bool,
    with_comments: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, row) = crate::commands::list::resolve_list(kb, list, daemon, bearer).await?;
    let list_id = row["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("list response missing id"))?;
    let url = format!(
        "{}/api/kb/{}/lists/{}/share/export",
        base_url(daemon),
        encode_path_segment(&kb_name),
        encode_path_segment(list_id),
    );
    let body = json!({
        "links": links,
        "no_scrub": no_scrub,
        "include_comments": with_comments,
    });
    download_share_zip(&url, &body, out, "list share export", bearer).await
}

/// Shared zip download/unpack for path and list export (same caps + headers).
async fn download_share_zip(
    url: &str,
    body: &Value,
    out: &Path,
    what: &str,
    bearer: Option<&str>,
) -> Result<()> {
    let client = client_with_timeout_and_bearer(DEPLOY_TIMEOUT_SECS, bearer)?;
    let resp = client
        .post(url)
        .json(body)
        .send()
        .await
        .with_context(|| what.to_string())?;
    let status = resp.status();
    // Capture the out-of-band headers before the body is consumed.
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let entry = header("x-kb-share-entry");
    let danglers = header("x-kb-share-danglers").unwrap_or_default();
    let skipped = header("x-kb-share-skipped").unwrap_or_default();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or(text);
        anyhow::bail!("{what} failed: HTTP {} — {detail}", status.as_u16());
    }
    // Bound the compressed body BEFORE it lands in memory: the extract caps
    // below bound the *decompressed* output, but a malicious/compromised daemon
    // (or MITM) could otherwise stream an unbounded compressed body and OOM the
    // CLI before extraction ever runs. Stream-accumulate with a hard ceiling —
    // a legitimate bundle is capped daemon-side at 256 MiB uncompressed, so a
    // compressed body past this ceiling is never legitimate.
    let zip_bytes = {
        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if buf.len() + chunk.len() > EXTRACT_MAX_RESPONSE_BYTES {
                anyhow::bail!(
                    "share bundle response exceeds {} bytes — refusing (possible bomb or malicious daemon)",
                    EXTRACT_MAX_RESPONSE_BYTES
                );
            }
            buf.extend_from_slice(&chunk);
        }
        buf
    };

    let as_zip = out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false);
    if as_zip {
        std::fs::write(out, &zip_bytes).with_context(|| format!("write {}", out.display()))?;
        eprintln!("wrote {} ({} bytes)", out.display(), zip_bytes.len());
    } else {
        let n = extract_zip(&zip_bytes, out)?;
        eprintln!("extracted {n} file(s) into {}", out.display());
    }
    if let Some(entry) = entry.filter(|e| !e.is_empty()) {
        eprintln!("open: {}", out.join(&entry).display());
    }
    let skipped: Vec<&str> = skipped.split(',').filter(|s| !s.is_empty()).collect();
    if !skipped.is_empty() {
        eprintln!(
            "warning: {} list entr{} skipped (tombstoned or unresolvable):",
            skipped.len(),
            if skipped.len() == 1 { "y" } else { "ies" },
        );
        for s in skipped {
            eprintln!("  {s}");
        }
    }
    let danglers: Vec<&str> = danglers.split(',').filter(|s| !s.is_empty()).collect();
    if !danglers.is_empty() {
        eprintln!(
            "warning: {} cross-artifact link(s) point outside the bundle (left as-is):",
            danglers.len()
        );
        for d in danglers {
            eprintln!("  {d}");
        }
    }
    Ok(())
}

/// `kb share <target> --page <out>` — fetch the single artifact UNCOMPRESSED,
/// in its native format (a scrubbed, self-contained `.html`, or the raw `.md`
/// SOURCE for a Markdown artifact) and write it to `out`. The daemon stages +
/// scrubs (kb-prompt stripped, outbound redactions applied); the bytes land on
/// THIS machine. Unlike `--local` there's no zip and no asset closure — just
/// the one page. `out` is the file to write.
pub async fn export_page(
    target: &str,
    out: &Path,
    kb: Option<&str>,
    no_scrub: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let kb_name = resolve_default_kb(kb, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/share/export/page",
        base_url(daemon),
        encode_path_segment(&kb_name)
    );
    let body = json!({ "target": target, "no_scrub": no_scrub });
    let client = client_with_timeout_and_bearer(DEPLOY_TIMEOUT_SECS, bearer)?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("share export page")?;
    let status = resp.status();
    let danglers = resp
        .headers()
        .get("x-kb-share-danglers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or(text);
        anyhow::bail!(
            "share export page failed: HTTP {} — {detail}",
            status.as_u16()
        );
    }
    let bytes = resp.bytes().await?.to_vec();
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;
    eprintln!("wrote {} ({} bytes)", out.display(), bytes.len());
    let danglers: Vec<&str> = danglers.split(',').filter(|s| !s.is_empty()).collect();
    if !danglers.is_empty() {
        eprintln!(
            "warning: {} cross-artifact link(s) point outside this page \
             (dead in the standalone file):",
            danglers.len()
        );
        for d in danglers {
            eprintln!("  {d}");
        }
    }
    Ok(())
}

// Decompression-bomb guards for an inbound share bundle. The zip bytes come
// from the daemon's `/share/export` response (`export_local` above) — a remote
// or otherwise attacker-influenced daemon is squarely in the threat model — so
// extraction is bounded on three axes and REFUSES (never silently truncates)
// past any cap. Values mirror the server-side archive BUILD caps in
// `kb-server/src/routes/download.rs` (the inverse operation), so a bundle this
// client accepts is one that side could plausibly have produced; they're
// re-declared here because kb-cli doesn't depend on kb-server.

/// Aggregate uncompressed bytes across all entries before we refuse the
/// archive — 256 MiB, matching `download::DOWNLOAD_MAX_BYTES`.
const EXTRACT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Per-entry uncompressed byte cap — 32 MiB, matching the review-export
/// `EXPORT_MAX_BYTES`. Generous for a single self-contained HTML artifact with
/// base64-embedded assets while still bounding one bomb entry.
const EXTRACT_MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;

/// Entry-count cap — 10_000, matching `download::DOWNLOAD_MAX_FILES`. Checked
/// up front against the central directory before any byte is written.
const EXTRACT_MAX_FILES: usize = 10_000;

/// Ceiling on the raw (compressed) share-bundle response body, enforced while
/// streaming it into memory — a legitimate bundle is capped daemon-side at
/// `EXTRACT_MAX_TOTAL_BYTES` uncompressed, so a compressed body larger than
/// this is never legitimate. Guards against a malicious/compromised daemon
/// OOM-ing the CLI before the decompression caps below ever run.
const EXTRACT_MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// Extract a zip archive (in memory) into `dest`, returning the file count.
///
/// Hardened against a hostile bundle: `enclosed_name` defeats zip-slip (entries
/// escaping `dest` via `..` or an absolute path), and three caps defeat a
/// decompression bomb — see [`extract_zip_bounded`]. Delegates to that inner
/// form with the production caps; tests drive it with small caps directly.
fn extract_zip(bytes: &[u8], dest: &Path) -> Result<usize> {
    extract_zip_bounded(
        bytes,
        dest,
        EXTRACT_MAX_ENTRY_BYTES,
        EXTRACT_MAX_TOTAL_BYTES,
        EXTRACT_MAX_FILES,
    )
}

/// Cap-parameterised core of [`extract_zip`]. Enforces, in order: an
/// entry-count cap (checked against the central directory up front); a
/// per-entry uncompressed-size cap; and an aggregate uncompressed-size cap.
/// The per-entry limit is enforced on the ACTUAL bytes copied — via a `take`
/// adapter capped one byte past the limit — not on the archive's self-declared
/// `size()`, so a lying central directory can't smuggle bytes past the guard
/// (the declared size is only a cheap pre-check). Any breach returns `Err`; the
/// partial extraction is left on disk for the caller to discard.
fn extract_zip_bounded(
    bytes: &[u8],
    dest: &Path,
    max_entry_bytes: u64,
    max_total_bytes: u64,
    max_files: usize,
) -> Result<usize> {
    use std::io::Read;

    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("read zip archive")?;
    if archive.len() > max_files {
        anyhow::bail!(
            "zip bundle has {} entries, exceeding the {max_files}-entry cap \
             (possible decompression bomb)",
            archive.len()
        );
    }
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    let mut written = 0usize;
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            anyhow::bail!("zip entry {:?} has an unsafe path", entry.name());
        };
        let outpath = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&outpath)?;
            continue;
        }
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Cheap pre-check on the DECLARED size — rejects an honestly-large
        // entry before opening the output file. Not trusted alone: the bounded
        // copy below independently enforces the same cap.
        if entry.size() > max_entry_bytes {
            anyhow::bail!(
                "zip entry {:?} declares {} bytes, exceeding the per-entry \
                 {max_entry_bytes}-byte cap (possible decompression bomb)",
                entry.name(),
                entry.size()
            );
        }
        let mut f = std::fs::File::create(&outpath)
            .with_context(|| format!("create {}", outpath.display()))?;
        // Read at most one byte PAST the cap so an over-run is detectable
        // (a spoofed-small `size()`) rather than silently truncated.
        let mut limited = entry.by_ref().take(max_entry_bytes.saturating_add(1));
        let n = std::io::copy(&mut limited, &mut f)
            .with_context(|| format!("extract {}", outpath.display()))?;
        if n > max_entry_bytes {
            anyhow::bail!(
                "zip entry {:?} exceeds the per-entry {max_entry_bytes}-byte cap \
                 (possible decompression bomb)",
                entry.name()
            );
        }
        total = total.saturating_add(n);
        if total > max_total_bytes {
            anyhow::bail!(
                "zip bundle exceeds the aggregate {max_total_bytes}-byte cap \
                 (possible decompression bomb)"
            );
        }
        written += 1;
    }
    Ok(written)
}

/// `kb share list`.
pub async fn list(
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let kb_name = resolve_default_kb(kb, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/shares",
        base_url(daemon),
        encode_path_segment(&kb_name)
    );
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let resp = send_json(client.get(&url), "share list").await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let rows = resp.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("no active shares in {kb_name}");
        return Ok(());
    }
    for r in rows {
        println!(
            "{}\t{}\t{}\t{}",
            r["name"].as_str().unwrap_or("?"),
            r["host"].as_str().unwrap_or("?"),
            r["gate"].as_str().unwrap_or("(public)"),
            r["url"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

/// `kb share revoke <name>`.
pub async fn revoke(
    name: &str,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let kb_name = resolve_default_kb(kb, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/shares/{}",
        base_url(daemon),
        encode_path_segment(&kb_name),
        encode_path_segment(name),
    );
    let client = client_with_timeout_and_bearer(120, bearer)?;
    let resp = send_json(client.delete(&url), "share revoke").await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        println!("revoked {name}");
    }
    Ok(())
}

/// Best-effort "open this URL" using the platform handler.
fn open_in_browser(url: &str) -> Result<()> {
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(url)
        .spawn()
        .with_context(|| format!("spawn {program}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    /// Build an in-memory zip. Each `(name, len)` entry is `len` STORED
    /// (uncompressed) zero bytes, so an entry's on-disk `size()` equals `len`
    /// and the aggregate is the sum — no compression ratio games needed.
    fn build_zip(entries: &[(&str, usize)]) -> Vec<u8> {
        let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::<u8>::new()));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, len) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(&vec![0u8; *len]).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn extract_normal_small_zip_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = build_zip(&[("a.html", 16), ("sub/b.html", 32)]);
        // Production caps: a tiny, well-formed bundle sails through.
        let n = extract_zip(&bytes, tmp.path()).unwrap();
        assert_eq!(n, 2);
        assert!(tmp.path().join("a.html").exists());
        assert!(tmp.path().join("sub").join("b.html").exists());
    }

    #[test]
    fn extract_rejects_oversized_entry() {
        let tmp = tempfile::tempdir().unwrap();
        // One 200-byte entry against a 100-byte per-entry cap.
        let bytes = build_zip(&[("big.bin", 200)]);
        let err = extract_zip_bounded(&bytes, tmp.path(), 100, 10_000, 100).unwrap_err();
        assert!(
            err.to_string().contains("per-entry"),
            "expected a per-entry cap error, got: {err}"
        );
    }

    #[test]
    fn extract_rejects_aggregate_overflow() {
        let tmp = tempfile::tempdir().unwrap();
        // Two entries each within the 100-byte per-entry cap but together (160)
        // over the 150-byte aggregate cap.
        let bytes = build_zip(&[("a.bin", 80), ("b.bin", 80)]);
        let err = extract_zip_bounded(&bytes, tmp.path(), 100, 150, 100).unwrap_err();
        assert!(
            err.to_string().contains("aggregate"),
            "expected an aggregate cap error, got: {err}"
        );
    }

    #[test]
    fn extract_rejects_too_many_entries() {
        let tmp = tempfile::tempdir().unwrap();
        // Four entries against a 3-entry cap — refused before any byte is read.
        let bytes = build_zip(&[("a", 1), ("b", 1), ("c", 1), ("d", 1)]);
        let err = extract_zip_bounded(&bytes, tmp.path(), 100, 10_000, 3).unwrap_err();
        assert!(
            err.to_string().contains("entries"),
            "expected an entry-count cap error, got: {err}"
        );
    }
}
