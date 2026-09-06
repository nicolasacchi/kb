//! `kb share` — static export + OAuth-gated publish.
//!
//! The engine resolves a target (a single artifact or a folder), copies
//! its files from the source dir, scrubs the HTML ([`crate::scrub::scrub_export`]),
//! resolves cross-artifact links, and pushes to a static host: Cloudflare
//! Pages + Access (gated) or GitHub Pages (public). It runs inside the
//! daemon and is invoked by both the CLI (`POST /api/kb/{kb}/share`) and
//! the SPA over this one engine.
//!
//! ## Why explicit args (no kb-server state)
//! kb-core has no "read artifact bytes" method (the serve-path uses
//! `std::fs::read`) and no folder-walk (that lives in a kb-server route
//! over `KbContext.source_path`). So the engine takes a [`ShareCtx`] of
//! explicit inputs the daemon route assembles from its `KbContext`; the
//! copy is a **filesystem walk from `source_path`**, not index-driven
//! (the index has no CSS/JS/image assets — an index-only copy would ship
//! unstyled pages).

pub mod assets;
pub mod cloudflare;
pub mod github;
pub mod host;

use crate::attachments::{is_inline_image, rewrite_attachment_refs, sanitize_filename};
use crate::indexer::is_markdown;
use crate::review::{Anchor, Attachment, Author, CommentStatus, ReviewFile};
use crate::scrub::{scrub_export, OutboundCache};
use crate::storage::sqlite::ShareRow;
use crate::storage::StorageHandle;
use crate::{Error, Result};
pub use host::ShareBackend;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Which static host a share is published to. Serialised to the `host`
/// column of the `shares` registry (V0004), whose CHECK constraint
/// matches these exact strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareHostKind {
    CloudflarePages,
    GithubPages,
}

impl ShareHostKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ShareHostKind::CloudflarePages => "cloudflare-pages",
            ShareHostKind::GithubPages => "github-pages",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cloudflare-pages" => Some(ShareHostKind::CloudflarePages),
            "github-pages" => Some(ShareHostKind::GithubPages),
            _ => None,
        }
    }
}

/// How to handle a link from a shared artifact to another artifact NOT in
/// the share. `pull-in` (transitive closure) is deferred — `warn` is the
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinksMode {
    /// List the danglers; leave the HTML untouched.
    #[default]
    Warn,
    /// Rewrite `/a/<kb>/<path>` permalinks to `<live_origin>/a/…` so the
    /// reader bounces back to the live instance. Requires `live_origin`.
    Absolute,
}

/// Per-invocation share options (the user-facing flags).
#[derive(Debug, Clone)]
pub struct ShareOpts {
    /// Source-relative target — a file (single artifact) or a folder.
    pub target: String,
    pub host: ShareHostKind,
    /// `--gate` tokens (repeatable). Empty = ungated. Cloudflare only.
    pub gate: Vec<String>,
    pub public: bool,
    pub links: LinksMode,
    /// Re-deploy to the SAME recorded deployment for this target.
    pub update: bool,
    /// Opt OUT of the export scrub (loud — leaks the generation prompt).
    pub no_scrub: bool,
    /// Y-track — publish each shared artifact's comment thread + its
    /// attachments into the static site (opt-in; default false). PUBLISHES
    /// otherwise-private review state — the SPA/CLI warn the operator. The
    /// `review_dir`/`attachments_root` below supply the daemon-state paths
    /// (they live on the owned `ShareOpts`, not the borrowed `ShareCtx`, so
    /// the engine's many test/ctx call sites stay untouched).
    pub include_comments: bool,
    /// `<state>/<kb>/.review/` — read each artifact's review file. `None`
    /// (or `include_comments=false`) disables comment publishing.
    pub review_dir: Option<PathBuf>,
    /// `<state>/<kb>/.attachments/` — copy attachment blobs from here.
    pub attachments_root: Option<PathBuf>,
}

/// Explicit inputs the daemon route assembles from its `KbContext`.
pub struct ShareCtx<'a> {
    pub handle: &'a StorageHandle,
    /// The kb's source root on disk (the copy walks from here).
    pub source_path: &'a Path,
    pub kb_name: &'a str,
    /// `artifact_host_suffix` — threaded into cross-artifact link parsing
    /// (subdomain-URL hosts; invariant 7/10).
    pub suffix: &'a str,
    /// Live kb origin for `--links absolute` permalink rewrites.
    pub live_origin: Option<&'a str>,
    /// The kb's compiled redaction rules (the export scrub always strips
    /// the kb-prompt regardless; these add the kb's PII redactions).
    pub outbound: Option<&'a OutboundCache>,
}

/// The staged share, ready to push: deployment-relative files (scrubbed),
/// the entry path to open, and the dangling out-of-share artifact links.
#[derive(Debug, Clone)]
pub struct StagedShare {
    pub files: Vec<(String, Vec<u8>)>,
    pub entry_path: String,
    pub danglers: Vec<String>,
}

/// Options for the ordered file-set staging path (list share / explicit paths).
/// Same scrub/links/comments knobs as [`ShareOpts`], without a path-walk target.
#[derive(Debug, Clone)]
pub struct ShareFileSetOpts {
    pub links: LinksMode,
    pub no_scrub: bool,
    pub include_comments: bool,
    pub review_dir: Option<PathBuf>,
    pub attachments_root: Option<PathBuf>,
}

/// One ordered TOC row for a generated list-share [`index.html`].
///
/// Section anchors deep-link as `{deploy}#{section_id}`. Chapter and Selection
/// anchors leave `section_id` unset and link only to the artifact page — those
/// anchor kinds have no stable offline fragment we can mint without the live
/// DOM (Chapter is a heading-text path; Selection is a css_path+offset).
#[derive(Debug, Clone)]
pub struct ShareIndexEntry {
    pub title: String,
    /// Source-relative path of the artifact (as stored on the list entry).
    /// The index href uses [`deploy_rel`] of this path (`.md` → `.html`).
    pub source_relative: String,
    pub note: Option<String>,
    /// When set (from `Anchor::Section`), the index href ends in `#{id}`.
    pub section_id: Option<String>,
}

/// Metadata for the generated list-share table-of-contents page.
#[derive(Debug, Clone)]
pub struct ShareIndexPage {
    pub title: String,
    pub description: Option<String>,
    /// Display order — drives the ordered links on `index.html` (not file names).
    pub entries: Vec<ShareIndexEntry>,
}

/// What a completed share looks like to the caller.
#[derive(Debug, Clone)]
pub struct ShareOutcome {
    pub name: String,
    pub url: String,
    pub host: String,
    pub gate: Option<String>,
    pub danglers: Vec<String>,
    pub files: usize,
    /// True when this was an `--update` to an existing deployment.
    pub updated: bool,
}

/// A single artifact staged for an uncompressed, native-format download
/// (`kb share --page` / `POST …/share/export/page`). HTML artifacts come
/// back as a scrubbed standalone `.html`; Markdown artifacts as their
/// scrubbed RAW `.md` source (never rendered — the "native format"
/// decision). A lone page has no in-share siblings to relativize against,
/// so cross-artifact links are left as authored and merely reported in
/// `danglers` (HTML only).
#[derive(Debug, Clone)]
pub struct StagedPage {
    /// Suggested download filename — the source basename, native extension.
    pub filename: String,
    /// `text/html; charset=utf-8` or `text/markdown; charset=utf-8`.
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
    /// Cross-artifact links pointing outside this page (dead in the
    /// standalone file). Empty for Markdown (raw source isn't link-rewritten).
    pub danglers: Vec<String>,
}

/// Trust boundary for a share target: the request-supplied path MUST stay
/// inside the kb's source root, returning the validated absolute path.
/// Without this guard a crafted `../../…` target — or an absolute path,
/// which `join` resolves wholesale — would let `kb share` read arbitrary
/// host files (SSH keys, env, /etc/*) into a deployment. Mirrors the
/// artifact serve path's `resolved.starts_with(source_root)` check
/// (routes/artifact.rs). Two layers: reject a literal `..` component up
/// front for a clear error, then a canonical containment check that also
/// defeats a symlinked target. (The relative asset closure is separately
/// guarded in `resolve_rel_asset`.) Shared by `stage_files` and
/// `stage_single_page`.
fn resolve_target_within_root(source_path: &Path, target: &str) -> Result<PathBuf> {
    if target.split(['/', '\\']).any(|c| c == "..") {
        return Err(Error::BadRequest(format!(
            "share target escapes the corpus: {target}"
        )));
    }
    let target_abs = source_path.join(target);
    let root_canon = source_path.canonicalize().map_err(Error::Io)?;
    let target_canon = target_abs
        .canonicalize()
        .map_err(|_| Error::NotFound(format!("share target not found: {target}")))?;
    if !target_canon.starts_with(&root_canon) {
        return Err(Error::BadRequest(format!(
            "share target escapes the corpus: {target}"
        )));
    }
    Ok(target_abs)
}

/// Stage ONE artifact for an uncompressed, native-format download — the
/// single-page counterpart to [`stage_files`] (which renders + zips a
/// folder/page-plus-assets bundle). HTML → scrubbed standalone `.html`;
/// Markdown → scrubbed RAW `.md` source. No zip, no asset closure, no
/// markdown→HTML render. The same trust boundary + export scrub as the
/// bundle (`scrub_export` always strips the kb-prompt; outbound redactions
/// apply) so a single-page download is just as safe to hand off.
pub fn stage_single_page(ctx: &ShareCtx<'_>, opts: &ShareOpts) -> Result<StagedPage> {
    let target_abs = resolve_target_within_root(ctx.source_path, &opts.target)?;
    let meta = std::fs::metadata(&target_abs)
        .map_err(|_| Error::NotFound(format!("share target not found: {}", opts.target)))?;
    if meta.is_dir() {
        return Err(Error::BadRequest(format!(
            "single-page export needs a file target, not a folder: {}",
            opts.target
        )));
    }
    let basename = target_abs
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| Error::BadRequest("share target has no file name".into()))?;
    let raw = std::fs::read(&target_abs)?;

    // Markdown: hand back the RAW source (never rendered), scrubbed only by
    // the kb's regex redactions — see `scrub_export_markdown`.
    if is_markdown(&target_abs) {
        let src = String::from_utf8_lossy(&raw).into_owned();
        let bytes = if opts.no_scrub {
            src.into_bytes()
        } else {
            crate::scrub::scrub_export_markdown(&src, ctx.outbound).into_bytes()
        };
        return Ok(StagedPage {
            filename: basename,
            content_type: "text/markdown; charset=utf-8",
            bytes,
            danglers: Vec::new(),
        });
    }

    if !is_html(&basename) {
        return Err(Error::BadRequest(format!(
            "single-page export supports HTML or Markdown artifacts, not: {}",
            opts.target
        )));
    }

    // HTML: scrubbed standalone page (same export scrub as the bundle).
    let html = String::from_utf8_lossy(&raw).into_owned();
    let scrubbed = if opts.no_scrub {
        html
    } else {
        scrub_export(&html, ctx.outbound)
    };
    // Report cross-artifact links as danglers — a lone page has nothing
    // in-share to relativize to, so every such link is dead offline. Empty
    // lookup maps ⇒ `relativize_links` rewrites nothing and records each
    // out-of-page link in `found`; we keep the scrubbed bytes as-is (the
    // rewritten output is identical here) and only surface the warnings.
    let from_deploy = deploy_rel(&basename);
    let deploy_set: HashSet<String> = std::iter::once(from_deploy.clone()).collect();
    let (_unchanged, found) = relativize_links(
        &scrubbed,
        &from_deploy,
        ctx.suffix,
        &HashMap::new(),
        &HashMap::new(),
        &deploy_set,
    );
    let danglers: Vec<String> = found
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(StagedPage {
        filename: basename,
        content_type: "text/html; charset=utf-8",
        bytes: scrubbed.into_bytes(),
        danglers,
    })
}

/// Resolve → copy → scrub → resolve-links. The network-free core of the
/// engine; unit-tested with a real source dir + storage handle.
///
/// Path-walk selection: `opts.target` is a single file (plus relative-asset
/// closure) or a folder (full walk). For an ordered explicit file set with a
/// generated TOC, use [`stage_file_set`].
pub async fn stage_files(ctx: &ShareCtx<'_>, opts: &ShareOpts) -> Result<StagedShare> {
    let target_abs = resolve_target_within_root(ctx.source_path, &opts.target)?;

    let meta = std::fs::metadata(&target_abs)
        .map_err(|_| Error::NotFound(format!("share target not found: {}", opts.target)))?;

    // Collect (deployment-relative path, absolute source path) pairs. The
    // if/else is an expression yielding the entry path to open.
    let mut file_list: Vec<(String, PathBuf)> = Vec::new();

    let entry_path: String = if meta.is_dir() {
        for entry in WalkDir::new(&target_abs).follow_links(false) {
            let entry = entry.map_err(|e| Error::Io(e.into()))?;
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&target_abs)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                // Markdown ships as a rendered `.html` page; rename the deploy
                // path now so entry-pick, link resolution, and scrub all treat
                // it as HTML (the source on disk stays `.md`).
                file_list.push((deploy_rel(&rel), entry.path().to_path_buf()));
            }
        }
        file_list.sort_by(|a, b| a.0.cmp(&b.0));
        if file_list.iter().any(|(r, _)| r == "index.html") {
            "index.html".to_string()
        } else {
            file_list
                .iter()
                .find(|(r, _)| is_html(r))
                .map(|(r, _)| r.clone())
                .unwrap_or_default()
        }
    } else {
        // Single artifact: deployment root = the file's parent dir; include
        // the file itself + its relative-asset closure (CSS/JS/images).
        let parent = target_abs
            .parent()
            .ok_or_else(|| Error::BadRequest("share target has no parent directory".into()))?
            .to_path_buf();
        let raw_entry = target_abs
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| Error::BadRequest("share target has no file name".into()))?;
        // Markdown ships as rendered `.html`; the source on disk stays `.md`.
        let entry = deploy_rel(&raw_entry);
        file_list.push((entry.clone(), target_abs.clone()));
        // Gather the relative-asset closure (CSS/JS/images) from the entry's
        // HTML — rendering markdown first so `![img](./x.png)` references are
        // visible to the asset scan.
        let entry_html: Option<String> = if is_markdown(&target_abs) {
            std::fs::read_to_string(&target_abs)
                .ok()
                .map(|src| crate::markdown::render_page(&src))
        } else if is_html(&entry) {
            std::fs::read_to_string(&target_abs).ok()
        } else {
            None
        };
        if let Some(html) = entry_html {
            for relref in assets::relative_assets(&html) {
                if let Some((rel, abs)) = resolve_rel_asset(&parent, &relref) {
                    if abs.is_file() && !file_list.iter().any(|(r, _)| *r == rel) {
                        file_list.push((rel, abs));
                    }
                }
            }
        }
        entry
    };

    if file_list.is_empty() {
        return Err(Error::BadRequest(format!(
            "share target is empty: {}",
            opts.target
        )));
    }

    stage_from_file_list(
        ctx,
        opts.links,
        opts.no_scrub,
        opts.include_comments,
        opts.review_dir.as_deref(),
        opts.attachments_root.as_deref(),
        file_list,
        entry_path,
    )
    .await
}

/// Stage an ordered explicit file set (source-relative paths) with a generated
/// list TOC as the bundle entry page.
///
/// Deploy paths keep the **source tree shape** (`research/a.html` stays
/// `research/a.html`); order is expressed only by the generated `index.html`,
/// not by renaming files. Each path reuses the same pipeline as
/// [`stage_files`]: per-file relative-asset closure (resolved against that
/// artifact's parent, deploy path relative to the kb root), markdown→HTML,
/// `scrub_export` (kb-prompt always stripped unless `no_scrub`), in-share link
/// relativization, out-of-set danglers, and deploy-path collision as an
/// explicit error.
///
/// `paths` may contain duplicates (e.g. two list entries pointing at the same
/// artifact with different section anchors); each unique deploy path is staged
/// once. Empty `paths` is a 400 (never an empty zip).
pub async fn stage_file_set(
    ctx: &ShareCtx<'_>,
    opts: &ShareFileSetOpts,
    paths: &[String],
    index: &ShareIndexPage,
) -> Result<StagedShare> {
    if paths.is_empty() {
        return Err(Error::BadRequest(
            "share file-set is empty — no resolvable artifacts to export".into(),
        ));
    }
    let file_list = collect_ordered_file_set(ctx.source_path, paths)?;
    if file_list.is_empty() {
        return Err(Error::BadRequest(
            "share file-set is empty — no resolvable artifacts to export".into(),
        ));
    }
    // Generated TOC always owns the bundle-root `index.html`. Refuse when a
    // real artifact would also claim that deploy path.
    if file_list.iter().any(|(r, _)| r == "index.html") {
        return Err(Error::BadRequest(
            "deploy path collision: list-share index.html collides with an \
             artifact in the set (rename or drop the root index.html entry)"
                .into(),
        ));
    }

    let mut staged = stage_from_file_list(
        ctx,
        opts.links,
        opts.no_scrub,
        opts.include_comments,
        opts.review_dir.as_deref(),
        opts.attachments_root.as_deref(),
        file_list,
        "index.html".to_string(),
    )
    .await?;

    // TOC is generated HTML — no kb-prompt, no scripts, fully self-contained.
    let index_bytes = render_share_index(index).into_bytes();
    staged.files.push(("index.html".to_string(), index_bytes));
    staged.entry_path = "index.html".to_string();
    Ok(staged)
}

/// Build the (deploy-rel, abs) list for an ordered source-relative file set.
/// Keeps source-tree deploy paths; expands each file's relative-asset closure
/// against that file's parent, with asset deploy paths also relative to the
/// kb source root. Dedupes by deploy path; two distinct sources mapping to the
/// same deploy path error explicitly.
fn collect_ordered_file_set(
    source_path: &Path,
    paths: &[String],
) -> Result<Vec<(String, PathBuf)>> {
    let mut file_list: Vec<(String, PathBuf)> = Vec::new();
    // deploy → absolute source, for collision detection on re-insert.
    let mut deploy_to_abs: HashMap<String, PathBuf> = HashMap::new();

    let push_unique = |file_list: &mut Vec<(String, PathBuf)>,
                       deploy_to_abs: &mut HashMap<String, PathBuf>,
                       deploy: String,
                       abs: PathBuf|
     -> Result<()> {
        if let Some(existing) = deploy_to_abs.get(&deploy) {
            if existing != &abs {
                return Err(Error::BadRequest(format!(
                    "deploy path collision: two sources map to `{deploy}` \
                     (a `.md` and its rendered `.html` sibling?) — rename one"
                )));
            }
            return Ok(()); // same file again — fine (duplicate list entries)
        }
        deploy_to_abs.insert(deploy.clone(), abs.clone());
        file_list.push((deploy, abs));
        Ok(())
    };

    for path in paths {
        let src_rel = path.replace('\\', "/");
        let src_rel = src_rel.trim_start_matches('/').to_string();
        if src_rel.is_empty() {
            return Err(Error::BadRequest(
                "share file-set path must not be empty".into(),
            ));
        }
        let target_abs = resolve_target_within_root(source_path, &src_rel)?;
        let meta = std::fs::metadata(&target_abs)
            .map_err(|_| Error::NotFound(format!("share target not found: {src_rel}")))?;
        if meta.is_dir() {
            return Err(Error::BadRequest(format!(
                "share file-set entries must be files, not folders: {src_rel}"
            )));
        }
        let deploy = deploy_rel(&src_rel);
        push_unique(
            &mut file_list,
            &mut deploy_to_abs,
            deploy.clone(),
            target_abs.clone(),
        )?;

        // Relative-asset closure against this artifact's own parent; deploy
        // paths stay under the source tree (not re-rooted to the parent).
        let parent = target_abs
            .parent()
            .ok_or_else(|| Error::BadRequest("share target has no parent directory".into()))?
            .to_path_buf();
        let entry_html: Option<String> = if is_markdown(&target_abs) {
            std::fs::read_to_string(&target_abs)
                .ok()
                .map(|src| crate::markdown::render_page(&src))
        } else if is_html(&deploy) {
            std::fs::read_to_string(&target_abs).ok()
        } else {
            None
        };
        if let Some(html) = entry_html {
            for relref in assets::relative_assets(&html) {
                if let Some((_, abs)) = resolve_rel_asset(&parent, &relref) {
                    if !abs.is_file() {
                        continue;
                    }
                    let Some(asset_deploy) = source_rel_deploy(source_path, &abs) else {
                        continue;
                    };
                    push_unique(&mut file_list, &mut deploy_to_abs, asset_deploy, abs)?;
                }
            }
        }
    }
    Ok(file_list)
}

/// Deployment-relative path of `abs` under `source_path`, or `None` when it
/// escapes the corpus (symlink / resolve failure).
fn source_rel_deploy(source_path: &Path, abs: &Path) -> Option<String> {
    let root = source_path.canonicalize().ok()?;
    let abs_c = abs.canonicalize().ok()?;
    if !abs_c.starts_with(&root) {
        return None;
    }
    let rel = abs_c
        .strip_prefix(&root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    if rel.is_empty() {
        return None;
    }
    Some(rel)
}

/// Shared scrub → link-relativize → optional comment-inject pipeline used by
/// both path-walk ([`stage_files`]) and ordered file-set ([`stage_file_set`]).
#[allow(clippy::too_many_arguments)] // mirrors ShareOpts / ShareFileSetOpts fields + file_list
async fn stage_from_file_list(
    ctx: &ShareCtx<'_>,
    links: LinksMode,
    no_scrub: bool,
    include_comments: bool,
    review_dir: Option<&Path>,
    attachments_root: Option<&Path>,
    file_list: Vec<(String, PathBuf)>,
    entry_path: String,
) -> Result<StagedShare> {
    // Deploy-path collision: a markdown source and a real HTML sibling can both
    // claim the same deploy rel (`report.md` + `report.html` → `report.html`).
    // The dir walk has no dedup, so both would stage to the same path and the
    // host would silently last-writer-win (non-deterministic across machines)
    // or 422. Refuse with a clear error rather than publish ambiguous bytes.
    {
        let mut seen: HashSet<&str> = HashSet::with_capacity(file_list.len());
        for (rel, _) in &file_list {
            if !seen.insert(rel.as_str()) {
                return Err(Error::BadRequest(format!(
                    "deploy path collision: two sources map to `{rel}` \
                     (a `.md` and its rendered `.html` sibling?) — rename one"
                )));
            }
        }
    }

    // Read → (render markdown) → (rewrite `.md` sibling links) → scrub.
    let mut staged: Vec<(String, Vec<u8>)> = Vec::with_capacity(file_list.len());
    for (rel, abs) in &file_list {
        let raw = std::fs::read(abs)?;
        let is_md = is_markdown(abs);
        // Non-HTML, non-markdown assets (CSS/JS/images) ship untouched.
        if !is_md && !is_html(rel) {
            staged.push((rel.clone(), raw));
            continue;
        }
        // Markdown renders to a full HTML page (its rel was already renamed
        // `.md`→`.html`); plain HTML is taken as-is.
        let mut html = if is_md {
            crate::markdown::render_page(&String::from_utf8_lossy(&raw))
        } else {
            String::from_utf8_lossy(&raw).into_owned()
        };
        // Rewrite relative `.md` sibling links → `.html`. Always for rendered
        // markdown; for plain HTML only when it actually references a markdown
        // path, so a normal HTML share stays byte-identical (no extra parse).
        if is_md || html.contains(".md") || html.contains(".markdown") {
            html = rewrite_md_links(&html);
        }
        let bytes = if no_scrub {
            html.into_bytes()
        } else {
            scrub_export(&html, ctx.outbound).into_bytes()
        };
        staged.push((rel.clone(), bytes));
    }

    // Cross-artifact links → self-contained export. Build deploy-path lookups
    // keyed two ways so an in-share link in EITHER authored shape resolves to
    // the co-deployed file:
    //   - by artifact id      → subdomain URLs (http://<id>.artifacts.<suffix>/…)
    //   - by kb-relative path → SPA permalinks (/a/<kb>/<source-relative-path>)
    // Both are derived from the file list the SAME way the indexer derives an
    // id (`ArtifactId::from_path(doc_rel_path)`), so they match the live
    // ids/paths with no storage round-trip — and resolve even for files not yet
    // indexed. `deploy_set` lets a subdomain sub-page link (`…/02-cause.html`)
    // confirm its target file is actually in the bundle before rewriting.
    let mut id_to_deploy: HashMap<String, String> = HashMap::new();
    let mut path_to_deploy: HashMap<String, String> = HashMap::new();
    let deploy_set: HashSet<String> = file_list.iter().map(|(rel, _)| rel.clone()).collect();
    for (rel, abs) in &file_list {
        if !is_html(rel) {
            continue;
        }
        let kb_rel = crate::paths::doc_rel_path(&abs.to_string_lossy(), ctx.source_path);
        let id = if kb_rel.is_empty() {
            crate::ids::ArtifactId::from_path(&abs.to_string_lossy())
        } else {
            crate::ids::ArtifactId::from_path(&kb_rel)
        };
        id_to_deploy.insert(id.as_str().to_string(), rel.clone());
        if !kb_rel.is_empty() {
            path_to_deploy.insert(kb_rel.clone(), rel.clone());
            // A permalink may name the rendered `.html` even when the source on
            // disk is `.md`; key both forms so either resolves.
            let html_key = deploy_rel(&kb_rel);
            if html_key != kb_rel {
                path_to_deploy.insert(html_key, rel.clone());
            }
        }
    }

    // `--links absolute` bounces OUT-OF-SHARE permalinks to the live instance;
    // fail fast (before the rewrite loop) when it's requested without an origin.
    let absolute_origin: Option<&str> = if matches!(links, LinksMode::Absolute) {
        Some(ctx.live_origin.ok_or_else(|| {
            Error::BadRequest("--links absolute needs [share] live_origin set in kb.toml".into())
        })?)
    } else {
        None
    };

    // One resolve-and-dispatch pass per staged HTML file: in-share links →
    // relative deploy paths (self-contained); out-of-share links recorded as
    // danglers and — under `absolute` — bounced to `<origin>/a/…` by the
    // following `prefix_permalinks` (which now only ever sees danglers, since
    // in-share permalinks were already relativized).
    let mut danglers: BTreeSet<String> = BTreeSet::new();
    for (rel, bytes) in staged.iter_mut() {
        if !is_html(rel) {
            continue;
        }
        let html = String::from_utf8_lossy(bytes).into_owned();
        let (rewritten, found) = relativize_links(
            &html,
            rel,
            ctx.suffix,
            &id_to_deploy,
            &path_to_deploy,
            &deploy_set,
        );
        danglers.extend(found);
        *bytes = match absolute_origin {
            Some(origin) => prefix_permalinks(&rewritten, origin).into_bytes(),
            None => rewritten.into_bytes(),
        };
    }

    // Y-track — opt-in: publish each shared artifact's comment thread + its
    // attachments into the static site. PUBLISHES otherwise-private review
    // state (the SPA/CLI warn the operator before setting this).
    if include_comments {
        if let Some(review_dir) = review_dir {
            inject_comments(&mut staged, &file_list, ctx, review_dir, attachments_root).await;
        }
    }

    Ok(StagedShare {
        files: staged,
        entry_path,
        danglers: danglers.into_iter().collect(),
    })
}

/// Render a self-contained list-share table of contents (`index.html`).
/// Inline minimal CSS (same system-ui palette as comment-publish chrome);
/// no external resources, no scripts, no `kb-prompt` template.
fn render_share_index(page: &ShareIndexPage) -> String {
    let mut out = String::with_capacity(2048 + page.entries.len() * 160);
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>");
    out.push_str(&esc(&page.title));
    out.push_str("</title>\n");
    out.push_str(SHARE_INDEX_CSS);
    out.push_str("</head>\n<body>\n");
    out.push_str("<header class=\"kb-share-idx__head\">\n");
    out.push_str("<h1>");
    out.push_str(&esc(&page.title));
    out.push_str("</h1>\n");
    if let Some(desc) = page
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push_str("<p class=\"kb-share-idx__desc\">");
        out.push_str(&esc(desc));
        out.push_str("</p>\n");
    }
    out.push_str("</header>\n");
    out.push_str("<ol class=\"kb-share-idx__list\">\n");
    for entry in &page.entries {
        let deploy = deploy_rel(&entry.source_relative);
        let href = match entry
            .section_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(sec) => format!("{deploy}#{sec}"),
            None => deploy,
        };
        out.push_str("<li class=\"kb-share-idx__item\">\n");
        out.push_str("<a href=\"");
        out.push_str(&esc(&href));
        out.push_str("\">");
        out.push_str(&esc(&entry.title));
        out.push_str("</a>\n");
        if let Some(note) = entry
            .note
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str("<p class=\"kb-share-idx__note\">");
            out.push_str(&esc(note));
            out.push_str("</p>\n");
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ol>\n</body>\n</html>\n");
    out
}

/// Minimal inline CSS for the list-share TOC — system-ui, no external assets.
/// Palette mirrors the comment-publish chrome (`KB_COMMENTS_CSS`) and the
/// live share demo so offline bundles feel consistent.
const SHARE_INDEX_CSS: &str = "<style>\
:root{font-family:ui-sans-serif,system-ui,-apple-system,sans-serif;line-height:1.55;color:#1c1c1c;background:#fafaf7}\
body{max-width:40rem;margin:2.5rem auto;padding:0 1.5rem}\
h1{font-size:1.5rem;margin:0 0 .35rem;letter-spacing:-.01em}\
.kb-share-idx__desc{color:#555;margin:0 0 1.5rem}\
.kb-share-idx__list{padding-left:1.25rem;margin:0}\
.kb-share-idx__item{margin:.55rem 0}\
.kb-share-idx__item a{color:#0b57d0;text-decoration:none}\
.kb-share-idx__item a:hover{text-decoration:underline}\
.kb-share-idx__note{margin:.2rem 0 0;color:#666;font-size:.9rem}\
</style>\n";

// --- Y-track: publish comment threads + attachments into the static site ---

/// For each shared HTML page that maps to an indexed artifact with a review
/// file, append a read-only rendered comment thread + copy its attachment
/// blobs (per-page-relative `attachments/<aid>-<name>`). Best-effort — a
/// missing review / unresolved id / unreadable blob is skipped, never fatal.
async fn inject_comments(
    staged: &mut Vec<(String, Vec<u8>)>,
    file_list: &[(String, PathBuf)],
    ctx: &ShareCtx<'_>,
    review_dir: &Path,
    attachments_root: Option<&Path>,
) {
    for (rel, abs) in file_list {
        if !is_html(rel) {
            continue;
        }
        let Some(id) = resolve_id(ctx.handle, abs).await else {
            continue;
        };
        let review = match crate::review::load(&review_dir.join(format!("{id}.json"))) {
            Ok(Some(r)) if !r.comments.is_empty() => r,
            _ => continue,
        };
        // Per-page-relative attachment paths: a page at `sub/x.html` refs
        // `attachments/<aid>-<name>` (resolved against itself) → deployed at
        // `sub/attachments/<aid>-<name>`.
        let page_dir = match rel.rsplit_once('/') {
            Some((d, _)) => d.to_string(),
            None => String::new(),
        };
        let mut ref_map: BTreeMap<String, String> = BTreeMap::new();
        let mut to_copy: Vec<(String, String)> = Vec::new(); // (aid, deploy_path)
        for c in &review.comments {
            collect_atts(&c.attachments, &page_dir, &mut ref_map, &mut to_copy);
            for r in &c.replies {
                collect_atts(&r.attachments, &page_dir, &mut ref_map, &mut to_copy);
            }
        }
        // Copy blobs (dedup by deploy path).
        if let Some(att_root) = attachments_root {
            for (aid, deploy) in &to_copy {
                if staged.iter().any(|(p, _)| p == deploy) {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(att_root.join(&id).join(aid)) {
                    staged.push((deploy.clone(), bytes));
                }
            }
        }
        // Render + inject the thread before </body>.
        let section = render_comments_section(&review, &ref_map);
        if let Some(entry) = staged.iter_mut().find(|(p, _)| p == rel) {
            let html = String::from_utf8_lossy(&entry.1).into_owned();
            entry.1 = inject_before_body_end(&html, &section).into_bytes();
        }
    }
}

fn collect_atts(
    atts: &[Attachment],
    page_dir: &str,
    ref_map: &mut BTreeMap<String, String>,
    to_copy: &mut Vec<(String, String)>,
) {
    for a in atts {
        let rref = format!("attachments/{}-{}", a.id, sanitize_filename(&a.filename));
        let deploy = if page_dir.is_empty() {
            rref.clone()
        } else {
            format!("{page_dir}/{rref}")
        };
        ref_map.insert(a.id.clone(), rref);
        to_copy.push((a.id.clone(), deploy));
    }
}

/// Render a review file's comments as a self-contained, inline-styled,
/// read-only HTML section. Bodies render via `render_comment_fragment`
/// (untrusted → raw HTML stripped) AFTER `attachment:<aid>` refs are
/// rewritten to the per-page relative paths in `ref_map`.
fn render_comments_section(file: &ReviewFile, ref_map: &BTreeMap<String, String>) -> String {
    let mut out = String::from("\n<section class=\"kb-comments\" id=\"kb-comments\">\n");
    out.push_str(KB_COMMENTS_CSS);
    out.push_str("<h2>Comments</h2>\n");
    for c in &file.comments {
        out.push_str(&render_one(
            &c.body,
            &c.author,
            c.status == CommentStatus::Resolved,
            Some(&c.anchor),
            &c.attachments,
            ref_map,
        ));
        for r in &c.replies {
            out.push_str("<div class=\"kb-c-reply\">");
            out.push_str(&render_one(
                &r.body,
                &r.author,
                false,
                None,
                &r.attachments,
                ref_map,
            ));
            out.push_str("</div>");
        }
    }
    out.push_str("</section>\n");
    out
}

fn render_one(
    body: &str,
    author: &Author,
    resolved: bool,
    anchor: Option<&Anchor>,
    attachments: &[Attachment],
    ref_map: &BTreeMap<String, String>,
) -> String {
    let author = match author {
        Author::You => "you",
        Author::Claude => "claude",
    };
    let body_html =
        crate::markdown::render_comment_fragment(&rewrite_attachment_refs(body, ref_map));
    let mut s = String::from("<article class=\"kb-c");
    if resolved {
        s.push_str(" kb-c--resolved");
    }
    s.push_str("\">\n<div class=\"kb-c-meta\"><span class=\"kb-c-author\">");
    s.push_str(author);
    s.push_str("</span>");
    if let Some(a) = anchor {
        s.push_str(" <span class=\"kb-c-anchor\">");
        s.push_str(&esc(&share_anchor_label(a)));
        s.push_str("</span>");
    }
    if resolved {
        s.push_str(" <span class=\"kb-c-status\">resolved</span>");
    }
    s.push_str("</div>\n<div class=\"kb-c-body\">");
    s.push_str(&body_html);
    s.push_str("</div>\n");
    if !attachments.is_empty() {
        s.push_str("<div class=\"kb-c-atts\">");
        for a in attachments {
            let Some(rel) = ref_map.get(&a.id) else {
                continue;
            };
            if is_inline_image(&a.content_type) {
                s.push_str(&format!(
                    "<a href=\"{rel}\"><img class=\"kb-c-thumb\" src=\"{rel}\" alt=\"{}\"></a>",
                    esc(&a.filename)
                ));
            } else {
                s.push_str(&format!(
                    "<a class=\"kb-c-file\" href=\"{rel}\">\u{1F4CE} {}</a>",
                    esc(&a.filename)
                ));
            }
        }
        s.push_str("</div>");
    }
    s.push_str("</article>\n");
    s
}

fn share_anchor_label(a: &Anchor) -> String {
    match a {
        Anchor::File => "file".into(),
        Anchor::Chapter { path } => format!("chapter: {path}"),
        Anchor::Section { id, .. } => format!("section: {id}"),
        Anchor::Selection { snippet, .. } => format!("selection: {snippet}"),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Insert `section` just before the closing `</body>` (case-insensitive),
/// or append when there's no body tag.
fn inject_before_body_end(html: &str, section: &str) -> String {
    if let Some(pos) = html.to_ascii_lowercase().rfind("</body>") {
        let mut out = String::with_capacity(html.len() + section.len());
        out.push_str(&html[..pos]);
        out.push_str(section);
        out.push_str(&html[pos..]);
        out
    } else {
        format!("{html}{section}")
    }
}

const KB_COMMENTS_CSS: &str = "<style>\
.kb-comments{max-width:48rem;margin:3rem auto;padding:1.5rem 0;border-top:2px solid #ddd;font-family:ui-sans-serif,system-ui,sans-serif}\
.kb-comments h2{font-size:1.1rem}\
.kb-c{border:1px solid #e2e2e2;border-radius:8px;padding:.6rem .8rem;margin:.6rem 0;background:#fafafa}\
.kb-c--resolved{opacity:.6}\
.kb-c-meta{font-size:.8rem;color:#666;margin-bottom:.3rem}\
.kb-c-author{font-weight:600;color:#333}.kb-c-anchor{color:#888}.kb-c-status{color:#2a7}\
.kb-c-reply{margin-left:1.4rem}\
.kb-c-atts{display:flex;flex-wrap:wrap;gap:.4rem;margin-top:.4rem}\
.kb-c-thumb{max-width:120px;max-height:120px;border:1px solid #ddd;border-radius:6px}\
.kb-c-file{font-size:.85rem}.kb-c-body img{max-width:100%}\
</style>\n";

/// Publish a share end-to-end: validate flags → stage → push to the host
/// → record in the registry. `--update` reuses the recorded deployment.
pub async fn run_share(
    ctx: &ShareCtx<'_>,
    backend: &ShareBackend,
    opts: &ShareOpts,
) -> Result<ShareOutcome> {
    validate(opts)?;

    let existing = if opts.update {
        ctx.handle.shares_get_by_target(opts.target.clone()).await?
    } else {
        None
    };
    let name = match &existing {
        Some(row) => row.name.clone(),
        None => derive_share_name(&opts.target)?,
    };

    let staged = stage_files(ctx, opts).await?;
    let result = backend
        .publish(&name, &staged.files, &opts.gate, existing.as_ref())
        .await?;

    let gate = if opts.gate.is_empty() {
        None
    } else {
        Some(opts.gate.join(","))
    };
    let deployed_url = join_url(&result.url, &staged.entry_path);
    let now = now_unix();
    let created_at = existing.as_ref().map(|r| r.created_at_unix).unwrap_or(now);

    let row = ShareRow {
        name: name.clone(),
        target: opts.target.clone(),
        host: opts.host.as_str().to_string(),
        deployed_url: deployed_url.clone(),
        gate: gate.clone(),
        cf_account_id: result.cf_account_id,
        pages_project: result.pages_project,
        cf_deployment_id: result.cf_deployment_id,
        access_app_id: result.access_app_id,
        access_policy_id: result.access_policy_id,
        github_repo: result.github_repo,
        created_at_unix: created_at,
        updated_at_unix: now,
    };
    ctx.handle.shares_insert(row).await?;

    Ok(ShareOutcome {
        name,
        url: deployed_url,
        host: opts.host.as_str().to_string(),
        gate,
        danglers: staged.danglers,
        files: staged.files.len(),
        updated: existing.is_some(),
    })
}

/// Tear down a share by name: undo the host objects, then drop the
/// registry row (last, so a failed teardown leaves recovery info). Returns
/// `false` when no such share is recorded.
pub async fn revoke(handle: &StorageHandle, backend: &ShareBackend, name: &str) -> Result<bool> {
    let Some(row) = handle.shares_get(name.to_string()).await? else {
        return Ok(false);
    };
    backend.teardown(&row).await?;
    handle.shares_delete(name.to_string()).await?;
    Ok(true)
}

/// List recorded shares (newest-first).
pub async fn list_shares(handle: &StorageHandle) -> Result<Vec<ShareRow>> {
    handle.shares_list().await
}

// --- helpers -----------------------------------------------------------------

/// Validate the host/gate/public combination (cheap; no I/O). Exposed so
/// the daemon route can 400 before building a backend.
pub fn validate(opts: &ShareOpts) -> Result<()> {
    match opts.host {
        ShareHostKind::GithubPages => {
            if !opts.gate.is_empty() {
                return Err(Error::BadRequest(
                    "--gate is not supported for github-pages (it cannot enforce a gate); \
                     use the default cloudflare-pages host for a gated share"
                        .into(),
                ));
            }
            if !opts.public {
                return Err(Error::BadRequest(
                    "github-pages is public-only; pass --public to confirm".into(),
                ));
            }
        }
        ShareHostKind::CloudflarePages => {
            if opts.public && !opts.gate.is_empty() {
                return Err(Error::BadRequest(
                    "use either --gate <rule> or --public, not both".into(),
                ));
            }
            if !opts.public && opts.gate.is_empty() {
                return Err(Error::BadRequest(
                    "cloudflare-pages needs --gate <rule> (email:DOMAIN | email:a,b | google | \
                     github) or --public"
                        .into(),
                ));
            }
        }
    }
    Ok(())
}

fn is_html(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

/// Strip a trailing `.md`/`.markdown` (ASCII, case-insensitive) from `path`,
/// returning the stem; `None` when it isn't a markdown name.
fn md_stem(path: &str) -> Option<&str> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".md") {
        Some(&path[..path.len() - 3])
    } else if lower.ends_with(".markdown") {
        Some(&path[..path.len() - 9])
    } else {
        None
    }
}

/// Deployment-relative path for a source file. Markdown ships as a rendered
/// `.html` page (`foo.md`/`foo.markdown` → `foo.html`); everything else keeps
/// its name. Mirrors the serve path, where `.md` is served as HTML.
fn deploy_rel(rel: &str) -> String {
    match md_stem(rel) {
        Some(stem) => format!("{stem}.html"),
        None => rel.to_string(),
    }
}

/// `./a/b.md#x` → `./a/b.html#x`. `None` when `href` is absolute,
/// protocol-relative, or doesn't point at a `.md`/`.markdown` path. The
/// `#fragment` / `?query` suffix (whichever comes first) is preserved.
fn md_href_to_html(href: &str) -> Option<String> {
    // Protocol-relative (`//host/…`) — external, not co-deployed.
    if href.starts_with("//") {
        return None;
    }
    // A `:` before any `/` is a URI scheme (`http:`, `mailto:`, `tel:`,
    // `data:`, `javascript:`…) — not a relative co-deployed file. This catches
    // `mailto:user@host.md`, which the old `://`-only guard let through.
    if let Some(colon) = href.find(':') {
        if !href[..colon].contains('/') {
            return None;
        }
    }
    let (path, suffix) = match href.find(['#', '?']) {
        Some(i) => (&href[..i], &href[i..]),
        None => (href, ""),
    };
    let stem = md_stem(path)?;
    Some(format!("{stem}.html{suffix}"))
}

/// Rewrite relative `*.md`/`*.markdown` `href`s to `*.html` so links between
/// co-deployed markdown siblings resolve to their rendered pages. Absolute /
/// protocol-relative URLs and non-markdown hrefs are left untouched. Same
/// lol-html shape as [`prefix_permalinks`].
fn rewrite_md_links(html: &str) -> String {
    use lol_html::{element, HtmlRewriter, Settings};
    let mut out: Vec<u8> = Vec::with_capacity(html.len());
    let mut rewriter = HtmlRewriter::new(
        Settings {
            element_content_handlers: vec![element!("a[href]", |el| {
                if let Some(href) = el.get_attribute("href") {
                    if let Some(rewritten) = md_href_to_html(&href) {
                        let _ = el.set_attribute("href", &rewritten);
                    }
                }
                Ok(())
            })],
            ..Settings::default()
        },
        |c: &[u8]| out.extend_from_slice(c),
    );
    if rewriter.write(html.as_bytes()).is_err() || rewriter.end().is_err() {
        return html.to_string();
    }
    String::from_utf8(out).unwrap_or_else(|_| html.to_string())
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn join_url(base: &str, entry: &str) -> String {
    if entry.is_empty() {
        base.to_string()
    } else {
        format!("{base}{entry}")
    }
}

/// `kb-share-<slug>-<6hex>` from the target's basename. The global
/// `<project>.pages.dev` namespace makes the random suffix load-bearing, so
/// an RNG failure is surfaced (not silently collapsed to a zeroed suffix that
/// would invite collisions) — propagate rather than `let _ =`.
fn derive_share_name(target: &str) -> Result<String> {
    let base = target
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(target);
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    let slug = slugify(stem);
    let slug = if slug.is_empty() {
        "share".to_string()
    } else {
        slug
    };
    let mut buf = [0u8; 3];
    getrandom::getrandom(&mut buf)
        .map_err(|e| Error::Share(format!("share-name RNG failed: {e}")))?;
    Ok(format!("kb-share-{slug}-{}", hex::encode(buf)))
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    trimmed.chars().take(40).collect()
}

/// Normalise a document-relative asset ref against `parent`, returning the
/// `(deployment-relative path, absolute path)` if it stays within `parent`
/// (refs that escape via `..` or are absolute are rejected → `None`).
fn resolve_rel_asset(parent: &Path, relref: &str) -> Option<(String, PathBuf)> {
    let mut stack: Vec<String> = Vec::new();
    for comp in Path::new(relref).components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => {
                stack.pop()?;
            }
            Normal(c) => stack.push(c.to_string_lossy().to_string()),
            RootDir | Prefix(_) => return None,
        }
    }
    if stack.is_empty() {
        return None;
    }
    let rel = stack.join("/");
    let abs = parent.join(&rel);
    Some((rel, abs))
}

/// Rewrite `<a href="/a/…">` permalinks to absolute `<origin>/a/…` so the
/// static page's cross-artifact links bounce back to the live instance.
fn prefix_permalinks(html: &str, origin: &str) -> String {
    use lol_html::{element, HtmlRewriter, Settings};
    let origin = origin.trim_end_matches('/').to_string();
    let mut out: Vec<u8> = Vec::with_capacity(html.len());
    let mut rewriter = HtmlRewriter::new(
        Settings {
            element_content_handlers: vec![element!("a[href]", |el| {
                if let Some(href) = el.get_attribute("href") {
                    if href.starts_with("/a/") {
                        let _ = el.set_attribute("href", &format!("{origin}{href}"));
                    }
                }
                Ok(())
            })],
            ..Settings::default()
        },
        |c: &[u8]| out.extend_from_slice(c),
    );
    if rewriter.write(html.as_bytes()).is_err() || rewriter.end().is_err() {
        return html.to_string();
    }
    String::from_utf8(out).unwrap_or_else(|_| html.to_string())
}

/// Split a URL remainder at the first `?`/`#`, returning `(path, suffix)`; the
/// `suffix` keeps its leading separator (`""` when absent).
fn split_url_tail(s: &str) -> (&str, &str) {
    match s.find(['?', '#']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// A `<a href>` that points at another kb artifact, in one of the two shapes
/// the authoring guide blesses (`docs/authoring-artifacts.md`).
enum CrossLink {
    /// `/a/<kb>/<kb-relative-path>` — `kb_rel` percent-decoded; `tail` keeps any
    /// `?query`/`#fragment`.
    Permalink { kb_rel: String, tail: String },
    /// `http(s)?://<id><suffix>[:port]/<subpath>` — `subpath` keeps its own
    /// `?`/`#`; empty for a bare artifact-root link.
    Subdomain { id: String, subpath: String },
}

/// Recognise a cross-artifact link shape — `None` for plain relative hrefs,
/// in-page `#frag`, and external/`mailto:` links (all left untouched).
fn classify_cross_link(href: &str, suffix: &str) -> Option<CrossLink> {
    let trimmed = href.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    // SPA permalink: /a/<kb>/<rel...>. Strip `/a/`, drop the kb segment, keep
    // the remainder verbatim (it is the source-relative path, slashes and all —
    // NOT a 12-hex id; `parser::parse_artifact_link`'s `nth(1)` is wrong here).
    if let Some(rest) = trimmed.strip_prefix("/a/") {
        let slash = rest.find('/')?; // need both a kb segment and a path
        let (path, tail) = split_url_tail(&rest[slash + 1..]);
        if path.is_empty() {
            return None;
        }
        return Some(CrossLink::Permalink {
            kb_rel: crate::strutil::percent_decode(path),
            tail: tail.to_string(),
        });
    }
    // Subdomain URL: scheme://<id-or-kb--id><suffix>[:port]/<subpath>.
    let after_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("//"))?;
    let (host, after_host) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i + 1..]),
        None => (after_scheme, ""),
    };
    // A host carrying `?`/`#` but no path (`…localhost?x`) → trim it off.
    let host = split_url_tail(host).0;
    // ARTIFACT HOST GRAMMAR v2 — a qualified label (`{kb_enc}--{id}`) also
    // resolves here: only the id is needed (`ShareCtx` is already
    // single-kb, so the kb_enc part is informational and dropped). A bare
    // label (id or legacy stem) is used verbatim, matching pre-v2 behaviour.
    let id = match crate::iframe::parse_artifact_host_id(host, suffix)? {
        crate::iframe::ArtifactHostId::Bare(label) => label,
        crate::iframe::ArtifactHostId::Qualified { id, .. } => id,
    };
    Some(CrossLink::Subdomain {
        id,
        subpath: after_host.to_string(),
    })
}

/// Parent directory of a deploy path (`sub/a.html` → `sub`; `a.html` → ``).
fn deploy_dir(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(i) => &rel[..i],
        None => "",
    }
}

/// Resolve a cross-artifact link to `(target_deploy_path, tail)` when it lands
/// INSIDE the share, else `None` (an out-of-share dangler).
fn resolve_cross_link(
    link: &CrossLink,
    id_to_deploy: &HashMap<String, String>,
    path_to_deploy: &HashMap<String, String>,
    deploy_set: &HashSet<String>,
) -> Option<(String, String)> {
    match link {
        CrossLink::Permalink { kb_rel, tail } => {
            // A permalink segment is EITHER a source-relative path
            // (`/a/kb/pm/01-timeline.html`, the SPA's current shape) OR a bare
            // 12-hex artifact id (`/a/kb/089ac8361706`, still emitted by older
            // artifacts). Try the path map (raw + `.md`→`.html`) then the id
            // map; the two key spaces don't overlap, so trying both is safe.
            let to = path_to_deploy
                .get(kb_rel)
                .or_else(|| path_to_deploy.get(&deploy_rel(kb_rel)))
                .or_else(|| id_to_deploy.get(kb_rel))?;
            Some((to.clone(), tail.clone()))
        }
        CrossLink::Subdomain { id, subpath } => {
            let entry = id_to_deploy.get(id)?;
            let (sub, tail) = split_url_tail(subpath);
            let sub = sub.trim_start_matches('/');
            if sub.is_empty() {
                // Bare artifact-root link → the artifact's own entry page.
                return Some((entry.clone(), tail.to_string()));
            }
            // A specific page inside a multi-page artifact is served relative to
            // the artifact's folder, so its deploy path is the entry's directory
            // + the sub-path. Only rewrite when that file is in the bundle.
            let dir = deploy_dir(entry);
            let candidate = if dir.is_empty() {
                sub.to_string()
            } else {
                format!("{dir}/{sub}")
            };
            if deploy_set.contains(&candidate) {
                Some((candidate, tail.to_string()))
            } else {
                None
            }
        }
    }
}

/// A bare 12-hex artifact id (the id-form permalink / subdomain label shape).
fn looks_like_artifact_id(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A best-effort artifact id for an out-of-share dangler (for the warning list).
fn dangler_id(link: &CrossLink) -> String {
    match link {
        CrossLink::Subdomain { id, .. } => id.clone(),
        // An id-form permalink already carries the id; a path-form one is
        // hashed the way the indexer derives the id.
        CrossLink::Permalink { kb_rel, .. } if looks_like_artifact_id(kb_rel) => kb_rel.clone(),
        CrossLink::Permalink { kb_rel, .. } => crate::ids::ArtifactId::from_path(kb_rel)
            .as_str()
            .to_string(),
    }
}

/// Relative href from one deploy file to another (`sub/a.html` → `other/b.html`
/// ⇒ `../other/b.html`), with `tail` (`?q`/`#frag`) appended. A self-link
/// collapses to its `tail` (or its own basename when there is none).
fn deploy_relative_href(from_deploy: &str, to_deploy: &str, tail: &str) -> String {
    if from_deploy == to_deploy {
        return if tail.is_empty() {
            to_deploy
                .rsplit('/')
                .next()
                .unwrap_or(to_deploy)
                .to_string()
        } else {
            tail.to_string()
        };
    }
    let from_parts: Vec<&str> = from_deploy.split('/').collect();
    let from_dirs = &from_parts[..from_parts.len().saturating_sub(1)];
    let to_parts: Vec<&str> = to_deploy.split('/').collect();
    let to_dirs = to_parts.len().saturating_sub(1);
    let mut common = 0;
    while common < from_dirs.len() && common < to_dirs && from_dirs[common] == to_parts[common] {
        common += 1;
    }
    let mut segs: Vec<&str> = vec![".."; from_dirs.len() - common];
    segs.extend_from_slice(&to_parts[common..]);
    let mut href = segs.join("/");
    href.push_str(tail);
    href
}

/// Rewrite every in-share cross-artifact `<a href>` in `html` to a relative
/// deploy path (so the static export is self-contained), leaving out-of-share
/// links untouched. Returns the rewritten HTML and the ids of out-of-share
/// danglers. Same lol-html shape as [`prefix_permalinks`] / [`rewrite_md_links`].
fn relativize_links(
    html: &str,
    from_deploy: &str,
    suffix: &str,
    id_to_deploy: &HashMap<String, String>,
    path_to_deploy: &HashMap<String, String>,
    deploy_set: &HashSet<String>,
) -> (String, Vec<String>) {
    use lol_html::{element, HtmlRewriter, Settings};
    let mut danglers: Vec<String> = Vec::new();
    let mut out: Vec<u8> = Vec::with_capacity(html.len());
    let mut rewriter = HtmlRewriter::new(
        Settings {
            element_content_handlers: vec![element!("a[href]", |el| {
                if let Some(href) = el.get_attribute("href") {
                    if let Some(link) = classify_cross_link(&href, suffix) {
                        match resolve_cross_link(&link, id_to_deploy, path_to_deploy, deploy_set) {
                            Some((to_deploy, tail)) => {
                                let _ = el.set_attribute(
                                    "href",
                                    &deploy_relative_href(from_deploy, &to_deploy, &tail),
                                );
                            }
                            None => danglers.push(dangler_id(&link)),
                        }
                    }
                }
                Ok(())
            })],
            ..Settings::default()
        },
        |c: &[u8]| out.extend_from_slice(c),
    );
    if rewriter.write(html.as_bytes()).is_err() || rewriter.end().is_err() {
        return (html.to_string(), Vec::new());
    }
    (
        String::from_utf8(out).unwrap_or_else(|_| html.to_string()),
        danglers,
    )
}

/// Resolve a staged HTML file's absolute path to its indexed artifact id,
/// trying the canonical path first then the raw path (mirrors the daemon's
/// `get_by_path` canonicalize-then-fallback). `None` when not indexed.
async fn resolve_id(handle: &StorageHandle, abs: &Path) -> Option<String> {
    if let Ok(canon) = abs.canonicalize() {
        if let Ok(Some(doc)) = handle
            .get_by_source_path(canon.to_string_lossy().to_string())
            .await
        {
            return Some(doc.id);
        }
    }
    match handle
        .get_by_source_path(abs.to_string_lossy().to_string())
        .await
    {
        Ok(Some(doc)) => Some(doc.id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageActor;

    #[test]
    fn share_host_kind_round_trips() {
        for k in [ShareHostKind::CloudflarePages, ShareHostKind::GithubPages] {
            assert_eq!(ShareHostKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ShareHostKind::parse("vercel"), None);
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("23-static-oauth-share"), "23-static-oauth-share");
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  __  "), "");
    }

    #[test]
    fn derive_share_name_shape() {
        let n = derive_share_name("docs/research/foo.html").unwrap();
        assert!(n.starts_with("kb-share-foo-"), "got {n}");
        assert_eq!(n.len(), "kb-share-foo-".len() + 6);
    }

    #[test]
    fn resolve_rel_asset_stays_within_parent() {
        let parent = Path::new("/src/blog");
        assert_eq!(
            resolve_rel_asset(parent, "assets/style.css"),
            Some(("assets/style.css".into(), parent.join("assets/style.css")))
        );
        // `./a/../b.css` normalises to `b.css`.
        assert_eq!(
            resolve_rel_asset(parent, "./a/../b.css"),
            Some(("b.css".into(), parent.join("b.css")))
        );
        // Escaping above the parent is rejected.
        assert_eq!(resolve_rel_asset(parent, "../secret.css"), None);
        assert_eq!(resolve_rel_asset(parent, "/abs.css"), None);
    }

    #[test]
    fn prefix_permalinks_absolutises_a_links_only() {
        let html = r##"<a href="/a/kb/x.html">x</a> <a href="https://ext/">e</a> <a href="rel.html">r</a>"##;
        let out = prefix_permalinks(html, "https://kb.example.com/");
        assert!(out.contains(r#"href="https://kb.example.com/a/kb/x.html""#));
        assert!(out.contains(r#"href="https://ext/""#), "external untouched");
        assert!(out.contains(r#"href="rel.html""#), "relative untouched");
    }

    #[test]
    fn validate_rejects_bad_host_gate_combos() {
        let base = ShareOpts {
            target: "x".into(),
            host: ShareHostKind::GithubPages,
            gate: vec!["email:x.com".into()],
            public: true,
            links: LinksMode::Warn,
            update: false,
            no_scrub: false,
            include_comments: false,
            review_dir: None,
            attachments_root: None,
        };
        assert!(validate(&base).is_err(), "github + gate");
        let gh_no_public = ShareOpts {
            gate: vec![],
            public: false,
            ..base.clone()
        };
        assert!(validate(&gh_no_public).is_err(), "github needs --public");
        let cf_neither = ShareOpts {
            host: ShareHostKind::CloudflarePages,
            gate: vec![],
            public: false,
            ..base.clone()
        };
        assert!(
            validate(&cf_neither).is_err(),
            "cloudflare needs gate or public"
        );
        let cf_ok = ShareOpts {
            host: ShareHostKind::CloudflarePages,
            gate: vec!["email:example.com".into()],
            public: false,
            ..base.clone()
        };
        assert!(validate(&cf_ok).is_ok());
    }

    async fn handle() -> (StorageHandle, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let lance = tmp.path().join("lance");
        let sqlite = tmp.path().join("index.db");
        let h = StorageActor::spawn(lance, sqlite, None).await.unwrap();
        (h, tmp)
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    const PROMPTFUL: &str = r##"<!doctype html><html><head>
        <link rel="stylesheet" href="assets/style.css">
        <template id="kb-prompt">SECRET PROMPT</template>
        </head><body><h1>Hi</h1></body></html>"##;

    fn folder_opts(target: &str, no_scrub: bool) -> ShareOpts {
        ShareOpts {
            target: target.into(),
            host: ShareHostKind::CloudflarePages,
            gate: vec!["email:example.com".into()],
            public: false,
            links: LinksMode::Warn,
            update: false,
            no_scrub,
            include_comments: false,
            review_dir: None,
            attachments_root: None,
        }
    }

    // invariant:9 export-strip
    #[tokio::test]
    async fn stage_folder_includes_assets_and_strips_prompt() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/index.html"), PROMPTFUL);
        write(&root.join("blog/assets/style.css"), "body{color:red}");
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let staged = stage_files(&ctx, &folder_opts("blog", false))
            .await
            .unwrap();
        let paths: Vec<&str> = staged.files.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"index.html"));
        assert!(paths.contains(&"assets/style.css"));
        assert_eq!(staged.entry_path, "index.html");
        let index = staged
            .files
            .iter()
            .find(|(p, _)| p == "index.html")
            .unwrap();
        let body = String::from_utf8_lossy(&index.1);
        assert!(
            !body.contains("SECRET PROMPT"),
            "export scrub strips kb-prompt"
        );
        assert!(body.contains("<h1>Hi</h1>"));
        assert!(staged.danglers.is_empty());
    }

    /// Build a `ShareCtx` borrowing `h`/`root` — the single-page tests don't
    /// touch storage (no comment injection), so a fresh `handle()` is just
    /// scaffolding to satisfy the borrow.
    fn page_ctx<'a>(h: &'a StorageHandle, root: &'a Path) -> ShareCtx<'a> {
        ShareCtx {
            handle: h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        }
    }

    #[tokio::test]
    async fn single_page_html_scrubs_prompt_native_format() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/post.html"), PROMPTFUL);
        let ctx = page_ctx(&h, root);
        let page = stage_single_page(&ctx, &folder_opts("blog/post.html", false)).unwrap();
        assert_eq!(page.filename, "post.html");
        assert_eq!(page.content_type, "text/html; charset=utf-8");
        let body = String::from_utf8_lossy(&page.bytes);
        assert!(
            !body.contains("SECRET PROMPT"),
            "export scrub strips kb-prompt"
        );
        assert!(body.contains("<h1>Hi</h1>"));
        // No cross-artifact links → no danglers (the local asset link is fine).
        assert!(page.danglers.is_empty());
    }

    #[tokio::test]
    async fn single_page_markdown_returns_raw_source_not_html() {
        let (h, src) = handle().await;
        let root = src.path();
        let md = "---\nkb-tags: a, b\n---\n\n# Note\n\nBody with `<code>` & an entity.\n";
        write(&root.join("notes/n.md"), md);
        let ctx = page_ctx(&h, root);
        let page = stage_single_page(&ctx, &folder_opts("notes/n.md", false)).unwrap();
        assert_eq!(page.filename, "n.md", "native .md extension, not .html");
        assert_eq!(page.content_type, "text/markdown; charset=utf-8");
        // Raw SOURCE — byte-identical, never rendered to HTML.
        assert_eq!(String::from_utf8_lossy(&page.bytes), md);
        assert!(page.danglers.is_empty());
    }

    #[tokio::test]
    async fn single_page_no_scrub_keeps_prompt() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("post.html"), PROMPTFUL);
        let ctx = page_ctx(&h, root);
        let page = stage_single_page(&ctx, &folder_opts("post.html", true)).unwrap();
        assert!(
            String::from_utf8_lossy(&page.bytes).contains("SECRET PROMPT"),
            "--no-scrub keeps the kb-prompt"
        );
    }

    #[tokio::test]
    async fn single_page_html_reports_cross_artifact_danglers() {
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("p.html"),
            r##"<!doctype html><html><body>
                <a href="/a/kb/other.html">elsewhere</a>
            </body></html>"##,
        );
        let ctx = page_ctx(&h, root);
        let page = stage_single_page(&ctx, &folder_opts("p.html", false)).unwrap();
        assert!(
            !page.danglers.is_empty(),
            "a permalink to another artifact is a dangler in a lone page"
        );
    }

    #[tokio::test]
    async fn single_page_rejects_folder_target() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/post.html"), PROMPTFUL);
        let ctx = page_ctx(&h, root);
        let err = stage_single_page(&ctx, &folder_opts("blog", false)).unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "folder → BadRequest");
    }

    #[tokio::test]
    async fn stage_no_scrub_keeps_prompt() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/index.html"), PROMPTFUL);
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let staged = stage_files(&ctx, &folder_opts("blog", true)).await.unwrap();
        let index = staged
            .files
            .iter()
            .find(|(p, _)| p == "index.html")
            .unwrap();
        assert!(String::from_utf8_lossy(&index.1).contains("SECRET PROMPT"));
    }

    #[tokio::test]
    async fn stage_single_artifact_pulls_in_assets() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/page.html"), PROMPTFUL);
        write(&root.join("blog/assets/style.css"), "body{}");
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let mut opts = folder_opts("blog/page.html", false);
        opts.target = "blog/page.html".into();
        let staged = stage_files(&ctx, &opts).await.unwrap();
        let paths: Vec<&str> = staged.files.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"page.html"), "entry file");
        assert!(paths.contains(&"assets/style.css"), "asset closure");
        assert_eq!(staged.entry_path, "page.html");
    }

    #[test]
    fn deploy_rel_and_md_href_rewrites() {
        assert_eq!(deploy_rel("a/b.md"), "a/b.html");
        assert_eq!(deploy_rel("a/b.MARKDOWN"), "a/b.html");
        assert_eq!(deploy_rel("a/b.html"), "a/b.html");
        assert_eq!(deploy_rel("a/b.css"), "a/b.css");

        assert_eq!(
            md_href_to_html("./other.md").as_deref(),
            Some("./other.html")
        );
        assert_eq!(
            md_href_to_html("../x/y.markdown#sec").as_deref(),
            Some("../x/y.html#sec")
        );
        assert_eq!(
            md_href_to_html("note.md?v=1").as_deref(),
            Some("note.html?v=1")
        );
        // External / protocol-relative / scheme / non-markdown hrefs left alone.
        assert_eq!(md_href_to_html("https://example.com/a.md"), None);
        assert_eq!(md_href_to_html("//cdn/a.md"), None);
        assert_eq!(md_href_to_html("mailto:user@host.md"), None);
        assert_eq!(md_href_to_html("tel:+1.md"), None);
        assert_eq!(md_href_to_html("./img.png"), None);
        assert_eq!(md_href_to_html("#frag"), None);
    }

    #[tokio::test]
    async fn stage_rejects_md_html_deploy_path_collision() {
        let (h, src) = handle().await;
        let root = src.path();
        // `report.md` renders to `report.html`, colliding with the real
        // `report.html` — must be refused, not silently last-writer-win.
        write(&root.join("blog/report.md"), "# From MD\n");
        write(&root.join("blog/report.html"), "<h1>From HTML</h1>");
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let err = stage_files(&ctx, &folder_opts("blog", false))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("collision") && msg.contains("report.html"),
            "expected a deploy-path collision error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn stage_rejects_target_path_traversal() {
        let (h, tmp) = handle().await;
        // Corpus is a SUBDIR, so there's a parent to try to escape into — the
        // common real deployment shape. A secret lives outside the corpus.
        let root = tmp.path().join("corpus");
        write(&root.join("blog/index.html"), "<h1>Hi</h1>");
        let secret = tmp.path().join("secret.txt");
        std::fs::write(&secret, "ssh-private-key").unwrap();
        let ctx = ShareCtx {
            handle: &h,
            source_path: &root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };

        // `..` component → rejected as a 400, never reads the file.
        let err = stage_files(&ctx, &folder_opts("../secret.txt", false))
            .await
            .unwrap_err();
        assert_eq!(
            err.http_status(),
            400,
            "relative-traversal target must be a 400"
        );
        assert!(err.to_string().contains("escapes the corpus"));

        // Absolute path (join() resolves it wholesale, bypassing the `..`
        // check) is caught by the canonical containment check.
        let abs_target = secret.to_string_lossy().to_string();
        let err = stage_files(&ctx, &folder_opts(&abs_target, false))
            .await
            .unwrap_err();
        assert_eq!(
            err.http_status(),
            400,
            "absolute escaping target must be a 400"
        );

        // Positive control: a legitimate in-corpus target still stages.
        let staged = stage_files(&ctx, &folder_opts("blog", false))
            .await
            .unwrap();
        assert_eq!(staged.entry_path, "index.html");
    }

    const MD_INDEX: &str = "---\ntitle: Home\n---\n# Home\n\n\
        See the [details](./details.md) and an [external](https://example.com/x.md).\n\n\
        <template id=\"kb-prompt\">SECRET MD PROMPT</template>\n";

    #[tokio::test]
    async fn stage_folder_renders_markdown_rewrites_links_and_strips_prompt() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/index.md"), MD_INDEX);
        write(&root.join("blog/details.md"), "# Details\n\nDetail text.\n");
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let staged = stage_files(&ctx, &folder_opts("blog", false))
            .await
            .unwrap();
        let paths: Vec<&str> = staged.files.iter().map(|(p, _)| p.as_str()).collect();
        // `.md` sources ship as rendered `.html`; no `.md` files remain.
        assert!(paths.contains(&"index.html"), "got {paths:?}");
        assert!(paths.contains(&"details.html"), "got {paths:?}");
        assert!(!paths.iter().any(|p| p.ends_with(".md")), "got {paths:?}");
        // A markdown index is picked as the entry (treated as HTML).
        assert_eq!(staged.entry_path, "index.html");

        let index = staged
            .files
            .iter()
            .find(|(p, _)| p == "index.html")
            .unwrap();
        let body = String::from_utf8_lossy(&index.1);
        assert!(body.contains("Home"), "rendered heading: {body}");
        // Sibling `.md` link rewritten to its rendered `.html` page.
        assert!(
            body.contains(r#"href="./details.html""#),
            "sibling link rewritten: {body}"
        );
        assert!(!body.contains("./details.md"), "no raw .md link: {body}");
        // External `.md` URL left untouched.
        assert!(
            body.contains("https://example.com/x.md"),
            "external .md link preserved: {body}"
        );
        // Export scrub always strips the inline kb-prompt.
        assert!(
            !body.contains("SECRET MD PROMPT"),
            "prompt stripped: {body}"
        );
        // Relative sibling links aren't cross-artifact edges → no danglers.
        assert!(
            staged.danglers.is_empty(),
            "danglers: {:?}",
            staged.danglers
        );
    }

    #[tokio::test]
    async fn stage_markdown_no_scrub_keeps_prompt_but_still_rewrites_links() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/index.md"), MD_INDEX);
        write(&root.join("blog/details.md"), "# Details\n");
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let staged = stage_files(&ctx, &folder_opts("blog", true)).await.unwrap();
        let index = staged
            .files
            .iter()
            .find(|(p, _)| p == "index.html")
            .unwrap();
        let body = String::from_utf8_lossy(&index.1);
        // --no-scrub keeps the prompt, but the `.md`→`.html` link rewrite is
        // structural (the sibling is deployed as `.html`) and still runs.
        assert!(body.contains("SECRET MD PROMPT"), "no-scrub keeps prompt");
        assert!(
            body.contains(r#"href="./details.html""#),
            "link still rewritten under --no-scrub: {body}"
        );
    }

    #[tokio::test]
    async fn stage_single_markdown_pulls_in_image_asset() {
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("solo/page.md"),
            "# Page\n\n![diagram](./diagram.png)\n",
        );
        write(&root.join("solo/diagram.png"), "PNGBYTES");
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let mut opts = folder_opts("solo/page.md", false);
        opts.target = "solo/page.md".into();
        let staged = stage_files(&ctx, &opts).await.unwrap();
        let paths: Vec<&str> = staged.files.iter().map(|(p, _)| p.as_str()).collect();
        // Entry renders to `.html`; the rendered `<img>` closure pulls the png.
        assert!(paths.contains(&"page.html"), "rendered entry: {paths:?}");
        assert!(paths.contains(&"diagram.png"), "asset closure: {paths:?}");
        assert_eq!(staged.entry_path, "page.html");
    }

    #[tokio::test]
    async fn run_share_then_revoke_via_fake_backend() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/index.html"), PROMPTFUL);
        write(&root.join("blog/assets/style.css"), "x");
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let state = std::sync::Arc::new(std::sync::Mutex::new(host::FakeState::default()));
        let backend = ShareBackend::Fake(state.clone());

        let outcome = run_share(&ctx, &backend, &folder_opts("blog", false))
            .await
            .unwrap();
        assert!(outcome.url.starts_with("https://fake.test/"));
        assert!(outcome.url.ends_with("/index.html"));
        assert_eq!(outcome.host, "cloudflare-pages");
        assert_eq!(outcome.gate.as_deref(), Some("email:example.com"));
        assert!(!outcome.updated);

        // Registry row written.
        let row = h.shares_get(outcome.name.clone()).await.unwrap().unwrap();
        assert_eq!(row.target, "blog");
        assert_eq!(row.host, "cloudflare-pages");

        // The fake recorded the staged files (scrubbed).
        let published = state.lock().unwrap().published.clone().unwrap();
        assert_eq!(published.0, outcome.name);
        assert!(published.1.iter().any(|(p, _)| p == "assets/style.css"));

        // Revoke tears down + drops the row.
        assert!(revoke(&h, &backend, &outcome.name).await.unwrap());
        assert!(h.shares_get(outcome.name.clone()).await.unwrap().is_none());
        assert!(state.lock().unwrap().revoked.contains(&outcome.name));
        // Revoking a missing share is Ok(false).
        assert!(!revoke(&h, &backend, "nope").await.unwrap());
    }

    // --- Y-track: comment publishing into a static share -----------------

    #[test]
    fn render_comments_section_rewrites_refs_escapes_html_and_lists_attachments() {
        let kb = crate::types::KbName::new("kb").unwrap();
        let mut file = ReviewFile::empty_skeleton(&kb, "id1", "T");
        file.comments.push(crate::review::Comment {
            id: "c_1".into(),
            status: CommentStatus::Open,
            file: "id1".into(),
            file_label: "main".into(),
            anchor: Anchor::File,
            author: Author::You,
            body: "see ![chart](attachment:a_1) <script>alert(1)</script>".into(),
            created_at: chrono::Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![Attachment {
                id: "a_1".into(),
                filename: "chart.png".into(),
                content_type: "image/png".into(),
                size: 10,
                created_at: chrono::Utc::now(),
                author: Author::You,
                user: None,
            }],
            user: None,
        });
        let mut map = BTreeMap::new();
        map.insert("a_1".to_string(), "attachments/a_1-chart.png".to_string());
        let html = render_comments_section(&file, &map);
        // Inline `attachment:` ref rewritten to the relative bundle path.
        assert!(
            html.contains("attachments/a_1-chart.png"),
            "ref rewritten: {html}"
        );
        // Untrusted raw HTML in the body is neutralized (no live <script>).
        assert!(!html.contains("<script>"), "raw html neutralized: {html}");
        // The attachment strip renders a thumbnail.
        assert!(html.contains("kb-c-thumb"), "strip thumbnail: {html}");
        assert!(html.contains("Comments</h2>"));
    }

    #[test]
    fn inject_before_body_end_inserts_or_appends() {
        let out = inject_before_body_end(
            "<html><body><h1>x</h1></body></html>",
            "<section>S</section>",
        );
        assert!(
            out.contains("<h1>x</h1><section>S</section></body>"),
            "inserted before </body>: {out}"
        );
        // No </body> → appended.
        let out2 = inject_before_body_end("<h1>x</h1>", "<section>S</section>");
        assert!(out2.ends_with("<section>S</section>"));
    }

    // --- live spike (network; #[ignore] — run with `--ignored` + real
    // creds). Each reads its creds from the env; a missing var early-skips
    // so `--ignored` is a no-op without setup. Each creates a uniquely
    // named, throwaway project/repo and tears it down. Run e.g.:
    //   KB_CF_API_TOKEN='pass://Claude/cloudflare/API Token' \
    //   KB_CF_TEST_ACCOUNT=<id> KB_CF_TEST_TEAM=<team>.cloudflareaccess.com \
    //   ~/.claude/hooks/pass-run.sh \
    //   cargo test -p kb-core -- --ignored --nocapture live_

    fn env_opt(key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|s| !s.trim().is_empty())
    }

    fn rand_suffix() -> String {
        let mut b = [0u8; 4];
        let _ = getrandom::getrandom(&mut b);
        hex::encode(b)
    }

    /// R1 — the load-bearing wrangler-compatibility check. Upload one
    /// asset under kb's computed hash, then `check-missing` the same hash:
    /// Cloudflare must report it PRESENT. If kb's hash disagreed with
    /// wrangler's content-addressing, CF would have stored it under a
    /// different key and still report it missing.
    #[tokio::test]
    #[ignore = "live: needs KB_CF_API_TOKEN + KB_CF_TEST_ACCOUNT"]
    async fn live_cloudflare_asset_hash_dedups() {
        let (Some(token), Some(account)) =
            (env_opt("KB_CF_API_TOKEN"), env_opt("KB_CF_TEST_ACCOUNT"))
        else {
            eprintln!("skip: set KB_CF_API_TOKEN + KB_CF_TEST_ACCOUNT");
            return;
        };
        use base64::Engine as _;
        use cloudflare::{
            asset_hash, content_type_for, AssetMetadata, AssetUpload, CloudflareClient,
        };
        let client = CloudflareClient::new(account, token).unwrap();
        let project = format!("kb-hashtest-{}", rand_suffix());
        client
            .ensure_project(&project)
            .await
            .expect("ensure_project");

        let bytes = b"<!doctype html><h1>kb asset-hash probe</h1>".to_vec();
        let hash = asset_hash(&bytes, "html");
        let jwt = client.upload_token(&project).await.expect("upload-token");

        let missing1 = client
            .check_missing(&jwt, std::slice::from_ref(&hash))
            .await
            .expect("check-missing #1");
        assert!(
            missing1.contains(&hash),
            "a fresh hash must be reported missing"
        );

        client
            .upload_assets(
                &jwt,
                &[AssetUpload {
                    key: hash.clone(),
                    value: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    metadata: AssetMetadata {
                        content_type: content_type_for("html").to_string(),
                    },
                    base64: true,
                }],
            )
            .await
            .expect("upload");
        client
            .upsert_hashes(&jwt, std::slice::from_ref(&hash))
            .await
            .expect("upsert-hashes");

        let missing2 = client
            .check_missing(&jwt, std::slice::from_ref(&hash))
            .await
            .expect("check-missing #2");
        assert!(
            !missing2.contains(&hash),
            "kb's asset hash must match Cloudflare's content-addressing \
             (wrangler-compatible) — else dedup never engages"
        );

        client.delete_project(&project).await.expect("cleanup");
    }

    /// Full Cloudflare engine round-trip: deploy + gate, --update reuses
    /// the project, then revoke tears it down.
    #[tokio::test]
    #[ignore = "live: needs KB_CF_API_TOKEN + KB_CF_TEST_ACCOUNT + KB_CF_TEST_TEAM"]
    async fn live_cloudflare_round_trip() {
        let (Some(token), Some(account), Some(team)) = (
            env_opt("KB_CF_API_TOKEN"),
            env_opt("KB_CF_TEST_ACCOUNT"),
            env_opt("KB_CF_TEST_TEAM"),
        ) else {
            eprintln!("skip: set KB_CF_API_TOKEN + KB_CF_TEST_ACCOUNT + KB_CF_TEST_TEAM");
            return;
        };
        let cfg = crate::config::CloudflareShareConfig {
            account_id: account.clone(),
            team_domain: team,
            google_idp: None,
            github_idp: None,
            api_token_env: "KB_CF_API_TOKEN".into(),
        };
        let client = cloudflare::CloudflareClient::new(account, token).unwrap();
        let backend = ShareBackend::Cloudflare { client, cfg };

        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("live/index.html"),
            "<!doctype html><html><body><h1>kb live share</h1></body></html>",
        );
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let opts = ShareOpts {
            target: "live".into(),
            host: ShareHostKind::CloudflarePages,
            gate: vec!["email:example.com".into()],
            public: false,
            links: LinksMode::Warn,
            update: false,
            no_scrub: false,
            include_comments: false,
            review_dir: None,
            attachments_root: None,
        };

        let outcome = run_share(&ctx, &backend, &opts)
            .await
            .expect("deploy + gate");
        assert!(outcome.url.contains(".pages.dev"), "url: {}", outcome.url);

        let updated = run_share(
            &ctx,
            &backend,
            &ShareOpts {
                update: true,
                ..opts
            },
        )
        .await
        .expect("--update");
        assert_eq!(updated.name, outcome.name, "--update reuses the project");

        assert!(revoke(&h, &backend, &outcome.name).await.expect("revoke"));
    }

    /// Full GitHub Pages round-trip: deploy a public share, then delete the
    /// repo. (Pages may still be building when revoke runs — that's fine.)
    #[tokio::test]
    #[ignore = "live: needs KB_GH_TOKEN"]
    async fn live_github_round_trip() {
        let Some(token) = env_opt("KB_GH_TOKEN") else {
            eprintln!("skip: set KB_GH_TOKEN");
            return;
        };
        let client = github::GithubClient::new(token).unwrap();
        let backend = ShareBackend::Github { client };

        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("live/index.html"),
            "<!doctype html><html><body><h1>kb gh live share</h1></body></html>",
        );
        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let opts = ShareOpts {
            target: "live".into(),
            host: ShareHostKind::GithubPages,
            gate: vec![],
            public: true,
            links: LinksMode::Warn,
            update: false,
            no_scrub: false,
            include_comments: false,
            review_dir: None,
            attachments_root: None,
        };

        let outcome = run_share(&ctx, &backend, &opts).await.expect("gh deploy");
        assert!(outcome.url.contains(".github.io"), "url: {}", outcome.url);
        assert!(revoke(&h, &backend, &outcome.name).await.expect("revoke"));
    }

    /// Persistent demo deploy — exercises the same engine the daemon would,
    /// but **does not revoke** so the URL stays live for a human to hit.
    /// Stages a small self-contained artifact (with a `<template
    /// id="kb-prompt">` to confirm scrub_export strips it) and gates it
    /// with `KB_CF_DEMO_GATE` (e.g. `email:you@example.com`). Tear it down
    /// later via the CF dashboard or another targeted DELETE.
    #[tokio::test]
    #[ignore = "live: demo deploy that stays up; needs KB_CF_API_TOKEN + KB_CF_TEST_ACCOUNT + KB_CF_TEST_TEAM + KB_CF_DEMO_GATE"]
    async fn live_cloudflare_demo() {
        let (Some(token), Some(account), Some(team), Some(gate)) = (
            env_opt("KB_CF_API_TOKEN"),
            env_opt("KB_CF_TEST_ACCOUNT"),
            env_opt("KB_CF_TEST_TEAM"),
            env_opt("KB_CF_DEMO_GATE"),
        ) else {
            eprintln!(
                "skip: set KB_CF_API_TOKEN + KB_CF_TEST_ACCOUNT + KB_CF_TEST_TEAM + KB_CF_DEMO_GATE"
            );
            return;
        };
        let cfg = crate::config::CloudflareShareConfig {
            account_id: account.clone(),
            team_domain: team,
            google_idp: None,
            github_idp: None,
            api_token_env: "KB_CF_API_TOKEN".into(),
        };
        let client = cloudflare::CloudflareClient::new(account, token).unwrap();
        let backend = ShareBackend::Cloudflare { client, cfg };

        let (h, src) = handle().await;
        let root = src.path();
        let html = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>kb share — live demo</title>
  <meta name="kb-tags" content="kb-share, demo, cloudflare, access">
  <meta name="kb-category" content="notes">
  <template id="kb-prompt">If you can see this in view-source on the live URL, scrub_export failed. The engine must strip every kb-prompt template at export time, regardless of the kb outbound config.</template>
  <style>
    :root { font-family: ui-sans-serif, system-ui, -apple-system, sans-serif; line-height: 1.55; color: #1c1c1c; background: #fafaf7; }
    body { max-width: 40rem; margin: 4rem auto; padding: 0 1.5rem; }
    h1 { font-size: 1.5rem; margin: 0 0 0.25rem; letter-spacing: -0.01em; }
    .sub { color: #707070; font-size: 0.9rem; margin: 0 0 1.5rem; }
    .pill { display: inline-block; padding: 0.15rem 0.55rem; border-radius: 1rem; background: #fff3d4; color: #5e4500; font-size: 0.78rem; margin: 0 0.35rem 0.35rem 0; border: 1px solid #f0dfae; }
    h2 { font-size: 1.05rem; margin: 1.7rem 0 0.5rem; }
    code { background: #ececea; padding: 0.1rem 0.35rem; border-radius: 0.3rem; font-size: 0.88em; }
    ol li { margin: 0.3rem 0; }
    .footer { margin-top: 3rem; padding-top: 1rem; border-top: 1px solid #e6e6e3; color: #888; font-size: 0.82rem; }
  </style>
</head>
<body>
  <h1>kb share — a live deploy</h1>
  <p class="sub">cloudflare-pages + access · example-com.cloudflareaccess.com · 2026-05-24</p>

  <div>
    <span class="pill">blake3(base64‖ext) hashes</span>
    <span class="pill">Pages Direct Upload</span>
    <span class="pill">Access self-hosted app</span>
    <span class="pill">email-gate (allow rule)</span>
    <span class="pill">scrub_export</span>
  </div>

  <h2>What just happened</h2>
  <ol>
    <li>The engine in <code>kb-core::share</code> walked this artifact + its asset closure.</li>
    <li>Every <code>.html</code> went through <code>scrub_export</code>; the <code>&lt;template id="kb-prompt"&gt;</code> in this file's head was stripped before upload.</li>
    <li>Each file got hashed with <code>blake3(base64(bytes)‖ext)[..32]</code> — wrangler-compatible content addressing.</li>
    <li>The Pages Direct Upload protocol ran: <code>upload-token → check-missing → upload → upsert-hashes → create_deployment</code>.</li>
    <li>A self-hosted Access app was created over the canonical <code>&lt;project&gt;.pages.dev</code> hostname, with an allow-policy carrying your email gate.</li>
  </ol>

  <h2>The point</h2>
  <p>You got past Cloudflare Access (OTP via <code>example-com.cloudflareaccess.com</code>), which means the gate works. The bytes above are real kb output, not a mock. Run <code>view-source</code> and the kb-prompt template won't be in there.</p>

  <p class="footer">Deployed via <code>cargo test live_cloudflare_demo</code>. Tear it down with a DELETE on the Pages project, or via the dashboard.</p>
</body>
</html>"##;
        write(&root.join("demo/index.html"), html);

        let ctx = ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let opts = ShareOpts {
            target: "demo".into(),
            host: ShareHostKind::CloudflarePages,
            gate: vec![gate],
            public: false,
            links: LinksMode::Warn,
            update: false,
            no_scrub: false,
            include_comments: false,
            review_dir: None,
            attachments_root: None,
        };

        let outcome = run_share(&ctx, &backend, &opts).await.expect("deploy");
        eprintln!();
        eprintln!("==> kb share demo deployed (NOT revoked — URL stays live):");
        eprintln!("    name:  {}", outcome.name);
        eprintln!("    url:   {}", outcome.url);
        eprintln!("    gate:  {:?}", outcome.gate);
        eprintln!("    files: {}", outcome.files);
        eprintln!();
    }

    /// Two persistent demos in one run, each exercising a different CF
    /// Access `include` rule shape: a specific-email rule (`email:user@x`)
    /// and an email-domain rule (`email:x`). Both stay live for human
    /// verification. Run e.g.:
    ///   KB_CF_API_TOKEN='pass://Claude/cloudflare/API Token' \
    ///   KB_CF_TEST_ACCOUNT=<id> KB_CF_TEST_TEAM=<team>.cloudflareaccess.com \
    ///   KB_CF_DEMO_GATE_A='email:you@example.com' \
    ///   KB_CF_DEMO_GATE_B='email:example.com' \
    ///   ~/.claude/hooks/pass-run.sh \
    ///   cargo test -p kb-core -- --ignored --nocapture live_cloudflare_demo_pair
    #[tokio::test]
    #[ignore = "live: two persistent demo deploys; needs KB_CF_API_TOKEN + KB_CF_TEST_ACCOUNT + KB_CF_TEST_TEAM + KB_CF_DEMO_GATE_A + KB_CF_DEMO_GATE_B"]
    async fn live_cloudflare_demo_pair() {
        let (Some(token), Some(account), Some(team), Some(gate_a), Some(gate_b)) = (
            env_opt("KB_CF_API_TOKEN"),
            env_opt("KB_CF_TEST_ACCOUNT"),
            env_opt("KB_CF_TEST_TEAM"),
            env_opt("KB_CF_DEMO_GATE_A"),
            env_opt("KB_CF_DEMO_GATE_B"),
        ) else {
            eprintln!(
                "skip: set KB_CF_API_TOKEN + KB_CF_TEST_ACCOUNT + KB_CF_TEST_TEAM + KB_CF_DEMO_GATE_A + KB_CF_DEMO_GATE_B"
            );
            return;
        };
        let cfg = crate::config::CloudflareShareConfig {
            account_id: account.clone(),
            team_domain: team,
            google_idp: None,
            github_idp: None,
            api_token_env: "KB_CF_API_TOKEN".into(),
        };
        let client = cloudflare::CloudflareClient::new(account, token).unwrap();
        let backend = ShareBackend::Cloudflare { client, cfg };

        let (h, src) = handle().await;
        let root = src.path();

        let page = |title: &str, lede: &str, gate_summary: &str| {
            format!(
                r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{title} — kb share</title>
  <meta name="kb-tags" content="kb-share, demo, cloudflare, access">
  <meta name="kb-category" content="notes">
  <template id="kb-prompt">If this template survives in the live HTML, scrub_export failed. The engine must strip every kb-prompt regardless of the kb's outbound config.</template>
  <style>
    :root {{ font-family: ui-sans-serif, system-ui, -apple-system, sans-serif; line-height: 1.55; color: #1c1c1c; background: #fafaf7; }}
    body {{ max-width: 38rem; margin: 4rem auto; padding: 0 1.5rem; }}
    h1 {{ font-size: 1.5rem; margin: 0 0 0.25rem; letter-spacing: -0.01em; }}
    .sub {{ color: #707070; font-size: 0.9rem; margin: 0 0 1.4rem; }}
    .pill {{ display: inline-block; padding: 0.15rem 0.55rem; border-radius: 1rem; background: #eaf2ff; color: #14365e; font-size: 0.78rem; border: 1px solid #c8dbf3; }}
    h2 {{ font-size: 1.05rem; margin: 1.7rem 0 0.5rem; }}
    code {{ background: #ececea; padding: 0.1rem 0.35rem; border-radius: 0.3rem; font-size: 0.88em; }}
    .footer {{ margin-top: 3rem; padding-top: 1rem; border-top: 1px solid #e6e6e3; color: #888; font-size: 0.82rem; }}
  </style>
</head>
<body>
  <h1>{title}</h1>
  <p class="sub">cloudflare-pages + access · 2026-05-24</p>

  <p><span class="pill">gate: {gate_summary}</span></p>

  <h2>This deploy</h2>
  <p>{lede}</p>

  <h2>What the gate does</h2>
  <p>Cloudflare Access has an allow-policy attached to this page's
  canonical hostname. The <code>include</code> rule was built by kb's
  <code>gate_to_include_rules</code> from the <code>--gate</code> flag.
  Access is deny-by-default — the allow-policy IS the gate.</p>

  <p class="footer">Deployed via <code>cargo test live_cloudflare_demo_pair</code>. View-source: no kb-prompt in the body.</p>
</body>
</html>"##
            )
        };

        write(
            &root.join("page_a/index.html"),
            &page(
                "Demo A — specific-email gate",
                "Only the exact email in the gate flag can OTP in. The Access include rule is <code>{\"email\": {\"email\": \"...\"}}</code>.",
                &gate_a,
            ),
        );
        write(
            &root.join("page_b/index.html"),
            &page(
                "Demo B — email-domain gate",
                "Anyone with a matching email domain can OTP in. The Access include rule is <code>{\"email_domain\": {\"domain\": \"...\"}}</code>.",
                &gate_b,
            ),
        );

        let mk_ctx = || ShareCtx {
            handle: &h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        };
        let mk_opts = |target: &str, gate: String| ShareOpts {
            target: target.into(),
            host: ShareHostKind::CloudflarePages,
            gate: vec![gate],
            public: false,
            links: LinksMode::Warn,
            update: false,
            no_scrub: false,
            include_comments: false,
            review_dir: None,
            attachments_root: None,
        };

        let a = run_share(&mk_ctx(), &backend, &mk_opts("page_a", gate_a.clone()))
            .await
            .expect("deploy A");
        let b = run_share(&mk_ctx(), &backend, &mk_opts("page_b", gate_b.clone()))
            .await
            .expect("deploy B");

        eprintln!();
        eprintln!("==> kb share demo PAIR deployed (both stay live):");
        eprintln!("    A  name={}  url={}  gate={:?}", a.name, a.url, a.gate);
        eprintln!("    B  name={}  url={}  gate={:?}", b.name, b.url, b.gate);
        eprintln!();
    }

    // ---- in-share cross-artifact link relativization (FS-A) ----

    #[test]
    fn deploy_relative_href_cases() {
        // same directory
        assert_eq!(
            deploy_relative_href("sub/a.html", "sub/b.html", ""),
            "b.html"
        );
        // root file → a subdir
        assert_eq!(
            deploy_relative_href("a.html", "sub/b.html", ""),
            "sub/b.html"
        );
        // sibling directory (one parent hop)
        assert_eq!(
            deploy_relative_href("sub/a.html", "other/b.html", ""),
            "../other/b.html"
        );
        // partial common prefix, fragment appended
        assert_eq!(
            deploy_relative_href("x/y/a.html", "x/z/b.html", "#s"),
            "../z/b.html#s"
        );
        // root → root sibling, query appended
        assert_eq!(
            deploy_relative_href("a.html", "b.html", "?q=1"),
            "b.html?q=1"
        );
        // self-link: fragment kept, path dropped
        assert_eq!(
            deploy_relative_href("sub/a.html", "sub/a.html", "#sec"),
            "#sec"
        );
        // self-link, no tail → bare basename (never an empty href)
        assert_eq!(
            deploy_relative_href("sub/a.html", "sub/a.html", ""),
            "a.html"
        );
        // two parent hops
        assert_eq!(
            deploy_relative_href("x/y/a.html", "b.html", ""),
            "../../b.html"
        );
    }

    #[test]
    fn classify_cross_link_shapes() {
        let sfx = crate::iframe::DEFAULT_HOST_SUFFIX;
        // permalink: kb segment dropped, path kept verbatim, tail split off
        match classify_cross_link("/a/mykb/pm/01-timeline.html#x", sfx) {
            Some(CrossLink::Permalink { kb_rel, tail }) => {
                assert_eq!(kb_rel, "pm/01-timeline.html");
                assert_eq!(tail, "#x");
            }
            _ => panic!("expected permalink"),
        }
        // percent-decoded segment
        match classify_cross_link("/a/kb/a%20b/c.html", sfx) {
            Some(CrossLink::Permalink { kb_rel, .. }) => assert_eq!(kb_rel, "a b/c.html"),
            _ => panic!("expected permalink"),
        }
        // subdomain with a sub-path + fragment
        let id = "abcdef012345";
        match classify_cross_link(&format!("http://{id}{sfx}/02-cause.html#z"), sfx) {
            Some(CrossLink::Subdomain { id: got, subpath }) => {
                assert_eq!(got, id);
                assert_eq!(subpath, "02-cause.html#z");
            }
            _ => panic!("expected subdomain"),
        }
        // non-cross-artifact hrefs are ignored (left untouched downstream)
        assert!(classify_cross_link("./sibling.html", sfx).is_none());
        assert!(classify_cross_link("#frag", sfx).is_none());
        assert!(classify_cross_link("https://example.com/x", sfx).is_none());
        assert!(classify_cross_link("mailto:a@b.c", sfx).is_none());
    }

    /// ARTIFACT HOST GRAMMAR v2 — a qualified subdomain link (`{kb_enc}--
    /// {id}`) still resolves to just the id; the kb part is dropped, since
    /// a `ShareCtx` is already scoped to one kb.
    #[test]
    fn classify_cross_link_accepts_qualified_subdomain_label() {
        let sfx = crate::iframe::DEFAULT_HOST_SUFFIX;
        let label = crate::iframe::qualified_label("docs", "abcdef012345");
        match classify_cross_link(&format!("http://{label}{sfx}/02-cause.html#z"), sfx) {
            Some(CrossLink::Subdomain { id, subpath }) => {
                assert_eq!(id, "abcdef012345");
                assert_eq!(subpath, "02-cause.html#z");
            }
            _ => panic!("expected subdomain"),
        }
        // Bare artifact-root qualified link (no sub-path).
        match classify_cross_link(&format!("http://{label}{sfx}"), sfx) {
            Some(CrossLink::Subdomain { id, subpath }) => {
                assert_eq!(id, "abcdef012345");
                assert_eq!(subpath, "");
            }
            _ => panic!("expected subdomain"),
        }
    }

    /// Build a `Warn` `ShareCtx` over `root` (the common shape for these tests).
    fn warn_ctx<'a>(h: &'a StorageHandle, root: &'a Path) -> ShareCtx<'a> {
        ShareCtx {
            handle: h,
            source_path: root,
            kb_name: "kb",
            suffix: crate::iframe::DEFAULT_HOST_SUFFIX,
            live_origin: None,
            outbound: None,
        }
    }

    fn staged_body<'a>(staged: &'a StagedShare, deploy: &str) -> std::borrow::Cow<'a, str> {
        let f = staged
            .files
            .iter()
            .find(|(p, _)| p == deploy)
            .unwrap_or_else(|| panic!("no staged file {deploy:?}"));
        String::from_utf8_lossy(&f.1)
    }

    #[tokio::test]
    async fn relativize_inshare_permalink_to_relative() {
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("blog/a.html"),
            r##"<!doctype html><html><body>
            <a href="/a/kb/blog/sub/b.html">b</a>
            <a href="./local.html">local</a>
            </body></html>"##,
        );
        write(&root.join("blog/sub/b.html"), "<h1>B</h1>");
        write(&root.join("blog/local.html"), "<h1>L</h1>");
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("blog", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "a.html");
        assert!(
            body.contains(r#"href="sub/b.html""#),
            "permalink relativized: {body}"
        );
        assert!(!body.contains("/a/kb/"), "no permalink remains: {body}");
        assert!(
            body.contains(r#"href="./local.html""#),
            "plain relative link untouched: {body}"
        );
        assert!(
            staged.danglers.is_empty(),
            "danglers: {:?}",
            staged.danglers
        );
    }

    #[test]
    fn looks_like_artifact_id_only_12_hex() {
        assert!(looks_like_artifact_id("089ac8361706"));
        assert!(!looks_like_artifact_id("pm/01.html")); // a path
        assert!(!looks_like_artifact_id("089ac836170")); // 11 chars
        assert!(!looks_like_artifact_id("089ac8361706a")); // 13 chars
        assert!(!looks_like_artifact_id("zzzzzzzzzzzz")); // not hex
    }

    #[tokio::test]
    async fn relativize_inshare_id_form_permalink() {
        // The id-form permalink `/a/<kb>/<12-hex-id>` (still emitted by older
        // artifacts, e.g. the prod `research` corpus) must relativize too.
        let (h, src) = handle().await;
        let root = src.path();
        let id_b = crate::ids::ArtifactId::from_path("blog/b.html")
            .as_str()
            .to_string();
        write(
            &root.join("blog/a.html"),
            &format!(r##"<a href="/a/kb/{id_b}#sec">b</a>"##),
        );
        write(&root.join("blog/b.html"), "<h1>B</h1>");
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("blog", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "a.html");
        assert!(
            body.contains(r##"href="b.html#sec""##),
            "id-form permalink relativized: {body}"
        );
        assert!(
            staged.danglers.is_empty(),
            "danglers: {:?}",
            staged.danglers
        );
    }

    #[tokio::test]
    async fn relativize_inshare_subdomain_bare_and_subpage() {
        let (h, src) = handle().await;
        let root = src.path();
        let sfx = crate::iframe::DEFAULT_HOST_SUFFIX;
        let id_b = crate::ids::ArtifactId::from_path("blog/sub/b.html")
            .as_str()
            .to_string();
        // A multi-page artifact: the entry's id + a sub-page path.
        let id_idx = crate::ids::ArtifactId::from_path("blog/sub/index.html")
            .as_str()
            .to_string();
        write(
            &root.join("blog/a.html"),
            &format!(
                r##"<!doctype html><html><body>
                <a href="http://{id_b}{sfx}/">bare</a>
                <a href="http://{id_idx}{sfx}/two.html#sec">page</a>
                </body></html>"##
            ),
        );
        write(&root.join("blog/sub/b.html"), "<h1>B</h1>");
        write(&root.join("blog/sub/index.html"), "<h1>Index</h1>");
        write(&root.join("blog/sub/two.html"), "<h1>Two</h1>");
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("blog", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "a.html");
        assert!(
            body.contains(r#"href="sub/b.html""#),
            "bare subdomain → entry: {body}"
        );
        assert!(
            body.contains(r#"href="sub/two.html#sec""#),
            "subdomain sub-page + fragment: {body}"
        );
        assert!(
            !body.contains(".artifacts."),
            "no subdomain URL remains: {body}"
        );
        assert!(
            staged.danglers.is_empty(),
            "danglers: {:?}",
            staged.danglers
        );
    }

    #[tokio::test]
    async fn relativize_permalink_preserves_tail_and_decodes() {
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("blog/a.html"),
            r##"<a href="/a/kb/blog/p%20q/b.html?x=1#frag">b</a>"##,
        );
        write(&root.join("blog/p q/b.html"), "<h1>B</h1>");
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("blog", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "a.html");
        assert!(
            body.contains(r#"href="p q/b.html?x=1#frag""#),
            "decoded path + preserved tail: {body}"
        );
    }

    #[tokio::test]
    async fn relativize_nested_parent_traversal() {
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("doc/sub/a.html"),
            r##"<a href="/a/kb/doc/other/b.html">b</a>"##,
        );
        write(&root.join("doc/other/b.html"), "<h1>B</h1>");
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("doc", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "sub/a.html");
        assert!(
            body.contains(r#"href="../other/b.html""#),
            "parent traversal: {body}"
        );
    }

    #[tokio::test]
    async fn dangler_left_under_warn_and_bounced_under_absolute() {
        let (h, src) = handle().await;
        let root = src.path();
        let doc = r##"<!doctype html><html><body>
            <a href="/a/kb/blog/b.html">in</a>
            <a href="/a/kb/elsewhere/c.html">out</a>
            </body></html>"##;
        write(&root.join("blog/a.html"), doc);
        write(&root.join("blog/b.html"), "<h1>B</h1>");
        // warn (default): in-share relativized; dangler left + reported.
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("blog", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "a.html");
        assert!(
            body.contains(r#"href="b.html""#),
            "in-share relativized: {body}"
        );
        assert!(
            body.contains(r#"href="/a/kb/elsewhere/c.html""#),
            "dangler left as-is: {body}"
        );
        let want = crate::ids::ArtifactId::from_path("elsewhere/c.html")
            .as_str()
            .to_string();
        assert!(
            staged.danglers.contains(&want),
            "dangler reported: {:?}",
            staged.danglers
        );

        // absolute: dangler bounced to the live origin; in-share still relative.
        let mut opts = folder_opts("blog", false);
        opts.links = LinksMode::Absolute;
        let ctx = ShareCtx {
            live_origin: Some("https://kb.example.com/"),
            ..warn_ctx(&h, root)
        };
        let staged = stage_files(&ctx, &opts).await.unwrap();
        let body = staged_body(&staged, "a.html");
        assert!(
            body.contains(r#"href="b.html""#),
            "in-share still relative: {body}"
        );
        assert!(
            body.contains(r#"href="https://kb.example.com/a/kb/elsewhere/c.html""#),
            "dangler bounced: {body}"
        );
    }

    #[tokio::test]
    async fn relative_siblings_left_untouched_like_pm_corpus() {
        // The canonical multi-page shape (corpus/canon/pm/*): plain relative
        // `.html`/`.css` siblings — must survive untouched (they already work in
        // a static export, and the relativize pass must not fight them).
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("pm/00-summary.html"),
            r##"<!doctype html><html><head><link rel="stylesheet" href="style.css"></head>
            <body><a class="next" href="01-timeline.html">next</a></body></html>"##,
        );
        write(&root.join("pm/01-timeline.html"), "<h1>T</h1>");
        write(&root.join("pm/style.css"), "body{}");
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("pm", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "00-summary.html");
        assert!(
            body.contains(r#"href="01-timeline.html""#),
            "relative sibling kept: {body}"
        );
        assert!(
            body.contains(r#"href="style.css""#),
            "asset link kept: {body}"
        );
        assert!(
            staged.danglers.is_empty(),
            "danglers: {:?}",
            staged.danglers
        );
    }

    #[tokio::test]
    async fn md_link_rewrite_and_relativize_coexist() {
        // A markdown doc whose `.md` sibling link is rewritten to `.html` AND
        // whose in-share permalink is relativized — the two passes don't fight.
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("blog/index.md"),
            "# Home\n\nSee [details](./details.md) and [other](/a/kb/blog/other.html).\n",
        );
        write(&root.join("blog/details.md"), "# Details\n");
        write(&root.join("blog/other.html"), "<h1>Other</h1>");
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("blog", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "index.html");
        assert!(
            body.contains(r#"href="./details.html""#),
            "md sibling rewritten: {body}"
        );
        assert!(
            body.contains(r#"href="other.html""#),
            "in-share permalink relativized: {body}"
        );
        assert!(!body.contains("/a/kb/"), "no permalink remains: {body}");
        assert!(
            staged.danglers.is_empty(),
            "danglers: {:?}",
            staged.danglers
        );
    }

    #[tokio::test]
    async fn pre_example_anchor_is_rewritten_like_existing_passes() {
        // Documents the shared lol-html limitation: an <a> inside <pre> is a
        // real DOM element, so an example anchor IS rewritten (same as
        // rewrite_md_links / prefix_permalinks). Authors entity-escape example
        // HTML (`&lt;a&gt;`), which is text and stays untouched.
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("doc/a.html"),
            "<pre><a href=\"/a/kb/doc/b.html\">x</a></pre>\
             <p>&lt;a href=\"/a/kb/doc/b.html\"&gt;</p>",
        );
        write(&root.join("doc/b.html"), "<h1>B</h1>");
        let staged = stage_files(&warn_ctx(&h, root), &folder_opts("doc", false))
            .await
            .unwrap();
        let body = staged_body(&staged, "a.html");
        assert!(
            body.contains(r#"<a href="b.html">x</a>"#),
            "live <pre> anchor rewritten: {body}"
        );
        assert!(
            body.contains("/a/kb/doc/b.html"),
            "entity-escaped example text left as-is: {body}"
        );
    }

    fn file_set_opts() -> ShareFileSetOpts {
        ShareFileSetOpts {
            links: LinksMode::Warn,
            no_scrub: false,
            include_comments: false,
            review_dir: None,
            attachments_root: None,
        }
    }

    fn index_for(paths: &[(&str, &str, Option<&str>, Option<&str>)]) -> ShareIndexPage {
        // (source_rel, title, note, section_id)
        ShareIndexPage {
            title: "Reading list".into(),
            description: Some("A curated set".into()),
            entries: paths
                .iter()
                .map(|(src, title, note, sec)| ShareIndexEntry {
                    title: (*title).into(),
                    source_relative: (*src).into(),
                    note: note.map(|s| s.to_string()),
                    section_id: sec.map(|s| s.to_string()),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn file_set_index_order_follows_caller_order() {
        let (h, src) = handle().await;
        let root = src.path();
        // Lexicographic order would be a then z; we pass z then a.
        write(&root.join("z.html"), "<h1>Z</h1>");
        write(&root.join("a.html"), "<h1>A</h1>");
        let paths = vec!["z.html".into(), "a.html".into()];
        let index = index_for(&[
            ("z.html", "Z first", None, None),
            ("a.html", "A second", Some("note on a"), None),
        ]);
        let staged = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &paths, &index)
            .await
            .unwrap();
        assert_eq!(staged.entry_path, "index.html");
        let body = staged_body(&staged, "index.html");
        let z_pos = body.find("Z first").expect("z title");
        let a_pos = body.find("A second").expect("a title");
        assert!(z_pos < a_pos, "index order follows list order: {body}");
        assert!(body.contains(r#"href="z.html""#), "link z: {body}");
        assert!(body.contains(r#"href="a.html""#), "link a: {body}");
        assert!(body.contains("note on a"), "entry note: {body}");
        assert!(body.contains("A curated set"), "description: {body}");
    }

    #[tokio::test]
    async fn file_set_strips_prompt_from_pages_and_index() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("blog/post.html"), PROMPTFUL);
        let paths = vec!["blog/post.html".into()];
        let index = index_for(&[("blog/post.html", "Post", None, None)]);
        let staged = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &paths, &index)
            .await
            .unwrap();
        let page = staged_body(&staged, "blog/post.html");
        assert!(!page.contains("SECRET PROMPT"), "page scrubbed: {page}");
        assert!(!page.contains("kb-prompt"), "no prompt template on page");
        let idx = staged_body(&staged, "index.html");
        assert!(!idx.contains("kb-prompt"), "index has no kb-prompt: {idx}");
        assert!(!idx.contains("SECRET PROMPT"), "index has no secret: {idx}");
        assert!(!idx.contains("<script"), "index has no scripts: {idx}");
    }

    #[tokio::test]
    async fn file_set_relativizes_inshare_cross_artifact_link() {
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("blog/a.html"),
            r##"<!doctype html><html><body>
            <a href="/a/kb/blog/b.html">b</a>
            </body></html>"##,
        );
        write(&root.join("blog/b.html"), "<h1>B</h1>");
        let paths = vec!["blog/a.html".into(), "blog/b.html".into()];
        let index = index_for(&[
            ("blog/a.html", "A", None, None),
            ("blog/b.html", "B", None, None),
        ]);
        let staged = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &paths, &index)
            .await
            .unwrap();
        let body = staged_body(&staged, "blog/a.html");
        assert!(
            body.contains(r#"href="b.html""#),
            "in-set permalink relativized: {body}"
        );
        assert!(!body.contains("/a/kb/"), "no permalink remains: {body}");
        assert!(
            staged.danglers.is_empty(),
            "danglers: {:?}",
            staged.danglers
        );
    }

    #[tokio::test]
    async fn file_set_out_of_set_link_is_dangler() {
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("blog/a.html"),
            r##"<!doctype html><html><body>
            <a href="/a/kb/elsewhere/c.html">out</a>
            </body></html>"##,
        );
        // Target of the link exists on disk but is NOT in the file set.
        write(&root.join("elsewhere/c.html"), "<h1>C</h1>");
        let paths = vec!["blog/a.html".into()];
        let index = index_for(&[("blog/a.html", "A", None, None)]);
        let staged = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &paths, &index)
            .await
            .unwrap();
        let want = crate::ids::ArtifactId::from_path("elsewhere/c.html")
            .as_str()
            .to_string();
        assert!(
            staged.danglers.contains(&want),
            "out-of-set dangler: {:?}",
            staged.danglers
        );
        let body = staged_body(&staged, "blog/a.html");
        assert!(
            body.contains(r#"href="/a/kb/elsewhere/c.html""#),
            "dangler left as-is under warn: {body}"
        );
    }

    #[tokio::test]
    async fn file_set_deploy_path_collision_errors() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("report.md"), "# From MD\n");
        write(&root.join("report.html"), "<h1>From HTML</h1>");
        let paths = vec!["report.md".into(), "report.html".into()];
        let index = index_for(&[
            ("report.md", "MD", None, None),
            ("report.html", "HTML", None, None),
        ]);
        let err = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &paths, &index)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("collision") && msg.contains("report.html"),
            "expected collision error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn file_set_markdown_renders_to_html_and_index_links_html() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("notes/n.md"), "# Note\n\nBody.\n");
        let paths = vec!["notes/n.md".into()];
        let index = index_for(&[("notes/n.md", "Note", None, None)]);
        let staged = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &paths, &index)
            .await
            .unwrap();
        let paths_on: Vec<&str> = staged.files.iter().map(|(p, _)| p.as_str()).collect();
        assert!(
            paths_on.contains(&"notes/n.html"),
            "md renders to .html: {paths_on:?}"
        );
        assert!(
            !paths_on.iter().any(|p| p.ends_with(".md")),
            "no raw .md in bundle: {paths_on:?}"
        );
        let idx = staged_body(&staged, "index.html");
        assert!(
            idx.contains(r#"href="notes/n.html""#),
            "index links .html path: {idx}"
        );
        let page = staged_body(&staged, "notes/n.html");
        assert!(page.contains("Note"), "rendered heading: {page}");
    }

    #[tokio::test]
    async fn file_set_section_anchor_produces_fragment_in_index() {
        let (h, src) = handle().await;
        let root = src.path();
        write(
            &root.join("doc.html"),
            r##"<!doctype html><html><body>
            <h2 id="overview">Overview</h2>
            </body></html>"##,
        );
        let paths = vec!["doc.html".into()];
        let index = index_for(&[("doc.html", "Doc", None, Some("overview"))]);
        let staged = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &paths, &index)
            .await
            .unwrap();
        let idx = staged_body(&staged, "index.html");
        assert!(
            idx.contains(r##"href="doc.html#overview""##),
            "section fragment link: {idx}"
        );
    }

    #[tokio::test]
    async fn file_set_index_html_collision_with_artifact_errors() {
        let (h, src) = handle().await;
        let root = src.path();
        write(&root.join("index.html"), "<h1>Real index</h1>");
        write(&root.join("other.html"), "<h1>Other</h1>");
        let paths = vec!["index.html".into(), "other.html".into()];
        let index = index_for(&[
            ("index.html", "Root", None, None),
            ("other.html", "Other", None, None),
        ]);
        let err = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &paths, &index)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("collision") && msg.contains("index.html"),
            "expected index.html collision, got: {msg}"
        );
    }

    #[tokio::test]
    async fn file_set_empty_paths_errors() {
        let (h, src) = handle().await;
        let root = src.path();
        let index = ShareIndexPage {
            title: "Empty".into(),
            description: None,
            entries: vec![],
        };
        let err = stage_file_set(&warn_ctx(&h, root), &file_set_opts(), &[], &index)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::BadRequest(_)),
            "empty set → BadRequest"
        );
        assert!(err.to_string().contains("empty"));
    }
}
