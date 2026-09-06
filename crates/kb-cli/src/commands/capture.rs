//! `kb capture [FILES... | -] [--kb][--title][--tags][--sanitize][--url]
//! [--text][--name][--output json]` — U3 (v0.25 quick capture). A thin
//! multipart POST to `POST /api/kb/{kb}/capture` (`routes::capture::create`,
//! U2), which stages the upload into the kb's `capture/` folder and stamps
//! provenance (`kb_core::capture`, U1). No local write happens here — the
//! daemon owns the filesystem side, same division as `kb comments upload`.
//!
//! `FILES` may be zero or more local paths, or the single sentinel `-` to
//! read one document from stdin (`--name` gives it a filename stem, default
//! "capture", always written as `.md` — mirrors `kb notes new --stdin` /
//! `kb list import -`). With no `FILES` at all, `--url`/`--text` becomes a
//! url-stub capture (the daemon never fetches the URL — it's a stub the
//! agent layer enriches later). `--from` is NOT a flag here: the daemon
//! derives `from:cli` from the `X-Requested-By: kb-cli` header every
//! kb-cli request already sends (`http::client_with_timeout_and_bearer`).

use anyhow::{bail, Context, Result};
use reqwest::multipart::{Form, Part};
use serde_json::Value;
use std::io::Read;

use crate::http::{
    client_with_timeout_and_bearer, encode_path_segment, resolve_default_kb, send_json,
};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// What `FILES` resolved to — split out from `run` so the `-`-vs-paths
/// decision (and its "don't mix them" guard) is unit-testable without
/// touching stdin or the filesystem.
#[derive(Debug, PartialEq, Eq)]
enum FileSource<'a> {
    /// No `FILES` given — a bare `--url`/`--text` stub capture.
    None,
    /// The single `-` sentinel — read one document from stdin.
    Stdin,
    /// One or more real local paths.
    Paths(&'a [String]),
}

/// Classify the `FILES` positional. `-` is only valid alone: mixing it with
/// real paths (`kb capture a.md -`) has no sane reading (which file does
/// `-` come from — first? last? interleaved?), so it's rejected up front
/// rather than silently picking one.
fn classify_files(files: &[String]) -> Result<FileSource<'_>> {
    if files.is_empty() {
        return Ok(FileSource::None);
    }
    if files.iter().any(|f| f == "-") {
        if files.len() != 1 {
            bail!(
                "`-` (stdin) must be the only FILES argument — pass either \
                 file paths or a single `-`, not both"
            );
        }
        return Ok(FileSource::Stdin);
    }
    Ok(FileSource::Paths(files))
}

/// The stdin capture's filename: `--name` (default "capture") + `.md` —
/// stdin captures are always treated as Markdown (matches `kb notes
/// new --stdin`'s body_md convention; there's no reliable way to sniff a
/// pipe's content type).
fn stdin_filename(name: Option<&str>) -> String {
    format!("{}.md", name.unwrap_or("capture"))
}

/// The non-file multipart text fields for one capture POST, built from the
/// verb's flags — factored out from `run` so the request-body shape is
/// unit-testable without a `reqwest::multipart::Form` (which has no public
/// accessors to assert against). Field names match the server's
/// `drain_capture_form` (`routes/capture.rs`); order is stable for the
/// tests below but not semantically meaningful (multipart fields are a
/// bag, not a sequence).
fn build_text_fields(
    title: Option<&str>,
    tags: Option<&str>,
    sanitize: bool,
    url: Option<&str>,
    text: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut fields = Vec::new();
    if let Some(t) = title {
        fields.push(("title", t.to_string()));
    }
    if let Some(t) = tags {
        fields.push(("tags", t.to_string()));
    }
    if sanitize {
        fields.push(("sanitize", "true".to_string()));
    }
    if let Some(u) = url {
        fields.push(("url", u.to_string()));
    }
    if let Some(t) = text {
        fields.push(("text", t.to_string()));
    }
    fields
}

/// `kb capture [FILES... | -] [--kb][--title][--tags][--sanitize][--url]
/// [--text][--name][--daemon][--output json]`. Entrypoint wired from
/// `main.rs`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    files: &[String],
    kb: Option<&str>,
    title: Option<&str>,
    tags: Option<&str>,
    sanitize: bool,
    url: Option<&str>,
    text: Option<&str>,
    name: Option<&str>,
    daemon: Option<&str>,
    output: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let source = classify_files(files)?;
    if source == FileSource::None && url.is_none() && text.is_none() {
        bail!(
            "kb capture requires at least one FILE, `-` for stdin, or \
             --url/--text"
        );
    }

    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let base = base_url(daemon);
    let post_url = format!(
        "{base}/api/kb/{}/capture",
        encode_path_segment(&resolved_kb)
    );

    let mut form = Form::new();
    for (field, value) in build_text_fields(title, tags, sanitize, url, text) {
        form = form.text(field, value);
    }
    match source {
        FileSource::None => {}
        FileSource::Stdin => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("reading stdin")?;
            let fname = stdin_filename(name);
            form = form.part("files", Part::bytes(buf).file_name(fname));
        }
        FileSource::Paths(paths) => {
            for p in paths {
                let bytes = std::fs::read(p).with_context(|| format!("reading {p}"))?;
                let fname = std::path::Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| p.clone());
                form = form.part("files", Part::bytes(bytes).file_name(fname));
            }
        }
    }

    // 60s timeout — uploads can run longer than the 5-10s JSON verbs
    // (mirrors `comments::upload_multipart`).
    let client = client_with_timeout_and_bearer(60, bearer)?;
    let body = send_json(client.post(&post_url).multipart(form), "capture").await?;

    if output == Some("json") {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    print_human(&body, &base, &resolved_kb);
    Ok(())
}

/// Print `✓ captured <id>  <source_relative>` + the full artifact URL
/// (`{base}/a/{kb}/{source_relative}`, percent-encoded per segment) for
/// every item in the response.
fn print_human(body: &Value, base: &str, default_kb: &str) {
    let items = body
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        println!("kb capture: no items in response");
        return;
    }
    for item in &items {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let rel = item
            .get("source_relative")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let kbn = item
            .get("kb")
            .and_then(|v| v.as_str())
            .unwrap_or(default_kb);
        let encoded_rel = rel
            .split('/')
            .map(encode_path_segment)
            .collect::<Vec<_>>()
            .join("/");
        println!("✓ captured {id}  {rel}");
        println!("  {base}/a/{}/{encoded_rel}", encode_path_segment(kbn));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_files_empty_is_none() {
        assert_eq!(classify_files(&[]).unwrap(), FileSource::None);
    }

    #[test]
    fn classify_files_dash_alone_is_stdin() {
        let files = vec!["-".to_string()];
        assert_eq!(classify_files(&files).unwrap(), FileSource::Stdin);
    }

    #[test]
    fn classify_files_paths_pass_through() {
        let files = vec!["a.md".to_string(), "b.html".to_string()];
        assert_eq!(classify_files(&files).unwrap(), FileSource::Paths(&files));
    }

    #[test]
    fn classify_files_rejects_dash_mixed_with_paths() {
        let files = vec!["a.md".to_string(), "-".to_string()];
        let err = classify_files(&files).unwrap_err();
        assert!(err.to_string().contains("only FILES argument"));
    }

    #[test]
    fn classify_files_rejects_multiple_dashes() {
        let files = vec!["-".to_string(), "-".to_string()];
        assert!(classify_files(&files).is_err());
    }

    #[test]
    fn stdin_filename_defaults_to_capture_md() {
        assert_eq!(stdin_filename(None), "capture.md");
    }

    #[test]
    fn stdin_filename_uses_name_stem() {
        assert_eq!(stdin_filename(Some("meeting-notes")), "meeting-notes.md");
    }

    #[test]
    fn build_text_fields_empty_when_nothing_set() {
        assert!(build_text_fields(None, None, false, None, None).is_empty());
    }

    #[test]
    fn build_text_fields_includes_only_set_fields() {
        let fields = build_text_fields(Some("Title"), None, true, Some("https://x"), None);
        assert_eq!(
            fields,
            vec![
                ("title", "Title".to_string()),
                ("sanitize", "true".to_string()),
                ("url", "https://x".to_string()),
            ]
        );
    }

    #[test]
    fn build_text_fields_sanitize_false_omitted() {
        // sanitize is opt-in (U1 decision 3): a false/absent flag must NOT
        // send `sanitize=false` and override the route's own default (the
        // share-target route defaults html sanitize ON; an explicit
        // `sanitize` field always wins server-side, per `routes/capture.rs`
        // `do_capture`'s doc).
        let fields = build_text_fields(None, None, false, None, None);
        assert!(!fields.iter().any(|(k, _)| *k == "sanitize"));
    }

    #[test]
    fn build_text_fields_all_set() {
        let fields = build_text_fields(
            Some("T"),
            Some("a,b"),
            true,
            Some("https://example.com"),
            Some("shared text"),
        );
        assert_eq!(
            fields,
            vec![
                ("title", "T".to_string()),
                ("tags", "a,b".to_string()),
                ("sanitize", "true".to_string()),
                ("url", "https://example.com".to_string()),
                ("text", "shared text".to_string()),
            ]
        );
    }

    #[test]
    fn print_human_reports_id_and_relative_and_url() {
        let body = serde_json::json!({
            "items": [
                {"kb": "notes", "id": "abc123def456", "source_relative": "capture/foo-1700000000.md", "title": "Foo"}
            ]
        });
        // print_human only writes to stdout; smoke-test it doesn't panic on
        // a well-formed body and an empty one.
        print_human(&body, "http://127.0.0.1:4000", "notes");
        print_human(&serde_json::json!({}), "http://127.0.0.1:4000", "notes");
    }
}
