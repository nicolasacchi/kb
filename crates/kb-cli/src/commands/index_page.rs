//! `kb index-page` — generate a self-contained HTML index page from a
//! kb's artifacts.
//!
//! Usage:
//!
//!   kb index-page --kb <name> \
//!     [--filter key=value]... \
//!     [--group-by FIELD] \
//!     [--out PATH] \
//!     [--template PATH] \
//!     [--daemon URL] [--limit N] [--title TEXT]
//!
//! Fetches `GET /api/kb/{kb}/docs?limit=N`, filters rows client-side by
//! the supplied `--filter` predicates, groups by `--group-by` field
//! (default: `kb-status`), and renders to a single HTML page. Designed to
//! replace hand-maintained `INDEX.md` ledgers — generated artifact, not
//! authored.
//!
//! Recognised filter / group keys:
//!   kb-category, kb-status, kb-severity   — string match (exact)
//!   tag                                   — membership in tags[]
//!   longread, has-canvas, has-form        — boolean (true/false)
//!
//! Output is itself a kb artifact (carries `<meta name="kb-category"
//! content="index-page">`) so the daemon indexes it like any other.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

const UNGROUPED_LABEL: &str = "(unset)";

#[derive(Debug, Deserialize)]
struct DocRow {
    // Track U — permalinks now use `source_relative`, not `id`; keep the
    // field so the deserialised shape still matches the API response.
    #[allow(dead_code)]
    id: String,
    title: String,
    #[allow(dead_code)]
    path: String,
    /// Track U — source-root-relative path; backs the path-based
    /// permalink `/a/<kb>/<source_relative>`. Empty only against a
    /// daemon older than U (which links would then be broken anyway).
    #[serde(default)]
    source_relative: String,
    #[serde(default)]
    kb_category: Option<String>,
    #[serde(default)]
    kb_status: Option<String>,
    #[serde(default)]
    kb_severity: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    mtime_unix: Option<i64>,
    #[serde(default)]
    longread: Option<bool>,
    #[serde(default)]
    has_canvas: Option<bool>,
    #[serde(default)]
    has_form: Option<bool>,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    kb: &str,
    filters: &[(String, String)],
    group_by: &str,
    out: Option<&Path>,
    template: Option<&Path>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    limit: u32,
    page_title: Option<&str>,
) -> Result<()> {
    let base = crate::http::detect_daemon(daemon, bearer)
        .await
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no kb daemon reachable at {} — start it with `kb daemon` or pass --daemon",
                daemon.unwrap_or("http://127.0.0.1:4000")
            )
        })?;
    let url = format!(
        "{base}/api/kb/{}/docs?limit={limit}",
        crate::http::encode_path_segment(kb)
    );
    let client = crate::http::client_with_timeout(30)?;
    let mut req = client.get(&url);
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("daemon {} returned {}", base, resp.status());
    }
    let rows: Vec<DocRow> = resp
        .json()
        .await
        .context("decode /api/kb/.../docs response")?;

    let matching: Vec<DocRow> = rows
        .into_iter()
        .filter(|r| filters.iter().all(|(k, v)| row_matches(r, k, v)))
        .collect();

    let groups = group_rows(&matching, group_by);
    let rendered = render_html(
        &groups,
        group_by,
        kb,
        page_title.unwrap_or(match group_by {
            "kb-category" => "Artifacts by category",
            "kb-severity" => "Artifacts by severity",
            _ => "Artifacts by status",
        }),
        template,
    )?;

    match out {
        Some(p) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("mkdir -p {}", parent.display()))?;
                }
            }
            std::fs::write(p, &rendered).with_context(|| format!("write {}", p.display()))?;
            eprintln!(
                "kb index-page: wrote {} ({} matching artifacts in {} group(s))",
                p.display(),
                matching.len(),
                groups.len()
            );
        }
        None => {
            print!("{rendered}");
        }
    }
    Ok(())
}

fn row_matches(row: &DocRow, key: &str, value: &str) -> bool {
    match key {
        "kb-category" => row.kb_category.as_deref() == Some(value),
        "kb-status" => row.kb_status.as_deref() == Some(value),
        "kb-severity" => row.kb_severity.as_deref() == Some(value),
        "tag" => row.tags.iter().any(|t| t == value),
        // Boolean filters: only match when the requested value parses
        // as a bool. M-cli: pre-fix `longread=garbage` silently matched
        // None rows because `None == None`; now garbage returns false.
        "longread" => bool_match(value, row.longread),
        "has-canvas" => bool_match(value, row.has_canvas),
        "has-form" => bool_match(value, row.has_form),
        _ => false,
    }
}

fn bool_match(filter_value: &str, row_value: Option<bool>) -> bool {
    match (parse_bool(filter_value), row_value) {
        (Some(want), Some(got)) => want == got,
        _ => false,
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn group_rows<'a>(rows: &'a [DocRow], group_by: &str) -> BTreeMap<String, Vec<&'a DocRow>> {
    let mut out: BTreeMap<String, Vec<&DocRow>> = BTreeMap::new();
    for r in rows {
        let bucket = group_key(r, group_by);
        out.entry(bucket).or_default().push(r);
    }
    // Within each bucket, sort by mtime desc, then title.
    for v in out.values_mut() {
        v.sort_by(|a, b| {
            b.mtime_unix
                .cmp(&a.mtime_unix)
                .then_with(|| a.title.cmp(&b.title))
        });
    }
    out
}

fn group_key(row: &DocRow, group_by: &str) -> String {
    let raw = match group_by {
        "kb-category" => row.kb_category.as_deref(),
        "kb-status" => row.kb_status.as_deref(),
        "kb-severity" => row.kb_severity.as_deref(),
        _ => None,
    };
    raw.unwrap_or(UNGROUPED_LABEL).to_string()
}

/// Render the index page. If `template` is supplied, the template is
/// substituted with `{{title}}`, `{{generated_at}}`, and a single
/// `{{groups}}` placeholder containing the rendered group sections.
/// Without a template, emits a minimal self-contained HTML document
/// styled to match the kb gallery aesthetic.
fn render_html(
    groups: &BTreeMap<String, Vec<&DocRow>>,
    group_by: &str,
    kb: &str,
    title: &str,
    template: Option<&Path>,
) -> Result<String> {
    let body = render_groups(groups, kb);
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    if let Some(tmpl_path) = template {
        let raw = std::fs::read_to_string(tmpl_path)
            .with_context(|| format!("read template {}", tmpl_path.display()))?;
        let out = raw
            .replace("{{title}}", title)
            .replace("{{generated_at}}", &now)
            .replace("{{group_by}}", group_by)
            .replace("{{kb}}", kb)
            .replace("{{groups}}", &body);
        Ok(out)
    } else {
        Ok(default_shell(title, &now, group_by, kb, &body))
    }
}

fn render_groups(groups: &BTreeMap<String, Vec<&DocRow>>, kb: &str) -> String {
    let mut out = String::new();
    for (label, rows) in groups {
        let count = rows.len();
        out.push_str(&format!(
            r#"<section class="group" id="g-{label_id}">
  <h2><span class="g-label">{label_html}</span> <span class="g-count">{count}</span></h2>
  <ul class="rows">
"#,
            label_id = slugify(label),
            label_html = escape_html(label)
        ));
        for r in rows {
            let date = r
                .mtime_unix
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            let summary = r
                .summary
                .as_deref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let snip: String = s.chars().take(160).collect();
                    format!("<p class=\"summary\">{}</p>", escape_html(&snip))
                })
                .unwrap_or_default();
            let cat = r
                .kb_category
                .as_deref()
                .map(|c| format!("<span class=\"cat\">{}</span>", escape_html(c)))
                .unwrap_or_default();
            let sev = r
                .kb_severity
                .as_deref()
                .map(|s| {
                    format!(
                        "<span class=\"sev sev-{}\">{}</span>",
                        slugify(s),
                        escape_html(s)
                    )
                })
                .unwrap_or_default();
            let tags = if r.tags.is_empty() {
                String::new()
            } else {
                let chips: Vec<String> = r
                    .tags
                    .iter()
                    .take(6)
                    .map(|t| format!("<span class=\"tag\">{}</span>", escape_html(t)))
                    .collect();
                format!("<div class=\"tags\">{}</div>", chips.join(""))
            };
            // Track U — path-based permalink. Encode each path segment
            // (keeping `/` literal) for the URL, then HTML-escape the
            // whole attribute value.
            let rel_url = r
                .source_relative
                .split('/')
                .map(crate::http::encode_path_segment)
                .collect::<Vec<_>>()
                .join("/");
            out.push_str(&format!(
                r#"    <li class="row">
      <a class="row-link" href="/a/{kb}/{rel}">{title}</a>
      <span class="meta">{cat}{sev}<time>{date}</time></span>
      {summary}
      {tags}
    </li>
"#,
                kb = escape_html(kb),
                rel = escape_html(&rel_url),
                title = escape_html(&r.title),
                cat = cat,
                sev = sev,
                date = escape_html(&date),
                summary = summary,
                tags = tags,
            ));
        }
        out.push_str("  </ul>\n</section>\n");
    }
    out
}

fn default_shell(title: &str, generated_at: &str, group_by: &str, kb: &str, body: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{title}</title>
<meta name="kb-category" content="index-page">
<meta name="kb-tags" content="index, generated">
<meta name="kb-status" content="generated">
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
:root {{
  color-scheme: light dark;
  --bg: #faf8f3;
  --fg: #1a1d23;
  --muted: #6c727f;
  --accent: #2563eb;
  --line: #e5e7eb;
  --chip-bg: #eef0f3;
  --chip-fg: #1a1d23;
  --sev-low: #16a34a;
  --sev-medium: #d97706;
  --sev-high: #dc2626;
  --sev-critical: #7c2d12;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --bg: #0f1115;
    --fg: #e5e7eb;
    --muted: #9aa3b2;
    --line: #1f242c;
    --chip-bg: #1c2128;
    --chip-fg: #e5e7eb;
  }}
}}
body {{ margin: 0; background: var(--bg); color: var(--fg); }}
.wrap {{ max-width: 80ch; margin: 0 auto; padding: 2.5rem 1.5rem 4rem; }}
header {{ border-bottom: 1px solid var(--line); padding-bottom: 1rem; margin-bottom: 2rem; }}
h1 {{ font-size: 1.6rem; margin: 0 0 .25rem; }}
.subtitle {{ color: var(--muted); font-size: .9rem; }}
.group {{ margin-bottom: 2rem; }}
.group h2 {{ font-size: 1.05rem; margin: 0 0 .75rem; display: flex; align-items: baseline; gap: .5rem; }}
.g-label {{ text-transform: capitalize; }}
.g-count {{ color: var(--muted); font-weight: 400; font-size: .8em; }}
ul.rows {{ list-style: none; padding: 0; margin: 0; }}
.row {{ padding: .8rem 0; border-bottom: 1px solid var(--line); }}
.row:last-child {{ border-bottom: none; }}
.row-link {{ color: var(--fg); text-decoration: none; font-weight: 600; }}
.row-link:hover {{ color: var(--accent); }}
.meta {{ display: inline-flex; gap: .4rem; align-items: center; margin-left: .5rem; font-size: .8rem; color: var(--muted); }}
.meta time {{ font-variant-numeric: tabular-nums; }}
.cat, .sev, .tag {{ background: var(--chip-bg); color: var(--chip-fg); padding: 0 .4rem; border-radius: .25rem; font-size: .75rem; font-weight: 500; }}
.sev-low {{ color: var(--sev-low); }}
.sev-medium {{ color: var(--sev-medium); }}
.sev-high {{ color: var(--sev-high); }}
.sev-critical {{ color: var(--sev-critical); }}
.summary {{ color: var(--muted); margin: .3rem 0 .4rem; font-size: .85rem; line-height: 1.45; }}
.tags {{ display: flex; flex-wrap: wrap; gap: .3rem; margin-top: .25rem; }}
.tags .tag {{ font-size: .7rem; opacity: .85; }}
footer {{ margin-top: 3rem; padding-top: 1rem; border-top: 1px solid var(--line); color: var(--muted); font-size: .75rem; }}
</style>
</head>
<body>
<div class="wrap">
<header>
  <h1>{title}</h1>
  <p class="subtitle">kb <code>{kb}</code> — grouped by <code>{group_by}</code> — generated {generated_at}</p>
</header>
{body}
<footer>Generated by <code>kb index-page</code>. Re-run to refresh.</footer>
</div>
</body>
</html>
"##,
        title = escape_html(title),
        generated_at = escape_html(generated_at),
        group_by = escape_html(group_by),
        kb = escape_html(kb),
        body = body,
    )
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Parse `--filter key=value` specs. Same shape as `kb new --var`.
pub fn parse_filter(spec: &str) -> Result<(String, String)> {
    match spec.split_once('=') {
        Some((k, v)) => {
            let k = k.trim().to_string();
            if k.is_empty() {
                bail!("--filter key must be non-empty: {spec:?}");
            }
            Ok((k, v.to_string()))
        }
        None => bail!("--filter must be key=value, got {spec:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        title: &str,
        cat: Option<&str>,
        status: Option<&str>,
        sev: Option<&str>,
        tags: &[&str],
        mtime: Option<i64>,
    ) -> DocRow {
        DocRow {
            id: title.to_lowercase().replace(' ', "-"),
            title: title.to_string(),
            path: format!("/tmp/{title}.html"),
            source_relative: format!("{}.html", title.to_lowercase().replace(' ', "-")),
            kb_category: cat.map(str::to_string),
            kb_status: status.map(str::to_string),
            kb_severity: sev.map(str::to_string),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            summary: None,
            mtime_unix: mtime,
            longread: None,
            has_canvas: None,
            has_form: None,
        }
    }

    #[test]
    fn row_matches_kb_category() {
        let r = row("a", Some("idea"), None, None, &[], None);
        assert!(row_matches(&r, "kb-category", "idea"));
        assert!(!row_matches(&r, "kb-category", "fix"));
    }

    #[test]
    fn row_matches_tag_membership() {
        let r = row("a", None, None, None, &["caching", "performance"], None);
        assert!(row_matches(&r, "tag", "caching"));
        assert!(!row_matches(&r, "tag", "security"));
    }

    #[test]
    fn row_matches_unknown_filter_key_is_false() {
        let r = row("a", Some("idea"), None, None, &[], None);
        assert!(!row_matches(&r, "title", "a"));
    }

    #[test]
    fn group_rows_buckets_by_status_and_handles_missing() {
        let rows = [
            row("a", Some("idea"), Some("open"), None, &[], Some(100)),
            row("b", Some("idea"), Some("open"), None, &[], Some(50)),
            row("c", Some("idea"), None, None, &[], Some(75)),
        ];
        let refs: Vec<&DocRow> = rows.iter().collect();
        let g = group_rows_helper(&refs, "kb-status");
        // a + b in "open", c in "(unset)"
        assert_eq!(g.get("open").unwrap().len(), 2);
        assert_eq!(g.get(UNGROUPED_LABEL).unwrap().len(), 1);
        // Newest-first within bucket
        assert_eq!(g.get("open").unwrap()[0].title, "a");
        assert_eq!(g.get("open").unwrap()[1].title, "b");
    }

    fn group_rows_helper<'a>(rows: &[&'a DocRow], by: &str) -> BTreeMap<String, Vec<&'a DocRow>> {
        let mut out: BTreeMap<String, Vec<&'a DocRow>> = BTreeMap::new();
        for r in rows {
            out.entry(group_key(r, by)).or_default().push(*r);
        }
        for v in out.values_mut() {
            v.sort_by(|a, b| {
                b.mtime_unix
                    .cmp(&a.mtime_unix)
                    .then_with(|| a.title.cmp(&b.title))
            });
        }
        out
    }

    #[test]
    fn escape_html_handles_dangerous_chars() {
        let s = "<a>&'\"</a>";
        assert_eq!(escape_html(s), "&lt;a&gt;&amp;&#39;&quot;&lt;/a&gt;");
    }

    #[test]
    fn parse_filter_accepts_key_eq_value() {
        let (k, v) = parse_filter("kb-status=open").unwrap();
        assert_eq!(k, "kb-status");
        assert_eq!(v, "open");
    }

    #[test]
    fn parse_filter_rejects_missing_eq() {
        assert!(parse_filter("just-key").is_err());
    }

    #[test]
    fn parse_bool_handles_common_forms() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("YES"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("nope"), None);
    }

    #[test]
    fn slugify_strips_and_lowercases() {
        assert_eq!(slugify("In Progress!"), "in-progress");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn render_groups_emits_path_based_permalink() {
        // Track U — the generated index links to the source-relative
        // path, not the 12-hex id.
        let r = row("Deep Dive", Some("idea"), Some("open"), None, &[], Some(1));
        let mut groups: std::collections::BTreeMap<String, Vec<&DocRow>> =
            std::collections::BTreeMap::new();
        groups.insert("open".to_string(), vec![&r]);
        let html = render_groups(&groups, "platform");
        assert!(
            html.contains(r#"href="/a/platform/deep-dive.html""#),
            "expected path-based permalink, got: {html}"
        );
        assert!(
            !html.contains(r#"href="/a/platform/deep-dive""#),
            "must not emit a bare-id (no .html) permalink: {html}"
        );
    }
}
