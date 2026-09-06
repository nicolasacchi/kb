//! Wikilinks — `[[target]]` / `[[target|alias]]` references inside Markdown,
//! the connective tissue that lets a note point at any other artifact, note,
//! or session in the corpus. Two pure, LLM-free pieces the indexer's
//! edge-record hook + the notes routes drive:
//!
//! - [`parse_wikilinks`] — locate every wikilink in a Markdown body, in
//!   document order, via comrak's wikilink AST. Because it parses (not
//!   regex-scans), code spans and fenced code blocks are skipped for free —
//!   the same trick [`crate::notes::scan_tasks`] uses for task lines.
//! - [`resolve`] — map a wikilink target string to a single corpus artifact
//!   by a deterministic ladder (id → source-relative path → exact title →
//!   unique basename), corpus-local and ambiguity-aware. No I/O: the caller
//!   supplies the candidate set ([`DocLite`]s) so resolution stays a pure
//!   function the unit tests pin.
//!
//! The grammar is golden-pinned here and mirrored client-side by
//! `web/src/lib/wikilink.ts` (the same dual-render contract callouts use:
//! comrak server-side, a hast pass in the SPA, kept in lock-step by tests).
//! Edges ride the existing `kind = "link"` `edges` table, so wikilinks flow
//! into the backlink/outlink counts, the atlas link-lines, and the graph
//! route with zero new storage — they simply make Markdown notes visible to
//! a graph that previously only saw HTML `<a href>` links.

/// A located wikilink reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    /// The destination as written — a title (`Deploy checklist`), a
    /// source-relative path (`ops/deploy.md`), or a 12-hex artifact id. Always
    /// trimmed and non-empty; any `#section` suffix is preserved here (callers
    /// strip it via [`normalize_target`] before resolution).
    pub target: String,
    /// Explicit display text from `[[target|alias]]`, present only when it
    /// differs from `target`. `None` → render the target (or, once resolved,
    /// the destination's title).
    pub alias: Option<String>,
}

/// A lightweight projection of an indexed artifact — the only fields
/// [`resolve`] needs. Built from a lance row by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLite {
    pub id: String,
    /// Source-relative path, forward-slash separated (`ops/deploy.md`).
    pub rel_path: String,
    pub title: String,
}

/// The outcome of resolving one wikilink target against the corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one artifact matched — its id.
    One(String),
    /// Several artifacts share the matched title/basename. The graph treats
    /// this as dangling (no edge), but a route can surface the candidates.
    Ambiguous(Vec<String>),
    /// Nothing matched — a dangling link (kept in Obsidian; renders as a
    /// "create me" affordance, never an error).
    None,
}

/// comrak options for the wikilink PARSE pass only: the shared
/// [`crate::markdown::kb_options`] plus Obsidian-style `[[target|alias]]`
/// (target before the pipe). Deliberately NOT folded into the render path —
/// resolving a target to a permalink needs storage, which `render_fragment`
/// (pure) doesn't have, so the served HTML leaves `[[…]]` as literal text and
/// the SPA renders links from the resolved map. Enabling the extension only
/// here keeps every existing artifact's server-rendered HTML byte-identical.
fn wikilink_parse_options() -> comrak::Options<'static> {
    let mut o = crate::markdown::kb_options();
    o.extension.wikilinks_title_after_pipe = true;
    o
}

/// Locate every wikilink in a Markdown `body`, in document order, duplicates
/// kept (the caller dedups — by resolved id for edges, by target for the
/// render map). Code spans + fenced blocks are skipped because comrak never
/// emits a `WikiLink` node inside them.
pub fn parse_wikilinks(body: &str) -> Vec<WikiLink> {
    use comrak::nodes::NodeValue;
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, body, &wikilink_parse_options());
    let mut out = Vec::new();
    for node in root.descendants() {
        let NodeValue::WikiLink(wl) = &node.data().value else {
            continue;
        };
        let target = wl.url.trim().to_string();
        if target.is_empty() {
            continue;
        }
        // The display text is the concatenation of the node's text/code
        // descendants. comrak sets it to the target for the no-pipe form, so
        // `alias` collapses to None there.
        let mut label = String::new();
        for c in node.descendants() {
            match &c.data().value {
                NodeValue::Text(t) => label.push_str(t),
                NodeValue::Code(code) => label.push_str(&code.literal),
                _ => {}
            }
        }
        let label = label.trim();
        let alias = (!label.is_empty() && label != target).then(|| label.to_string());
        out.push(WikiLink { target, alias });
    }
    out
}

/// The scannable PROSE of a Markdown body: every text node comrak walks,
/// with four subtrees dropped whole —
///
/// - **code** (`` `spans` ``, fenced + indented blocks, math),
/// - **raw HTML** (inline + block; comrak never emits `Text` inside these
///   anyway, so this is belt-and-braces),
/// - **an existing `[[wikilink]]`'s display text**, and
/// - **an inline link/image label** (`[text](url)`, `![alt](src)`).
///
/// This is the SAME parse (and therefore the same code-skip guarantee)
/// [`parse_wikilinks`] relies on — one scanner, two consumers: the wikilink
/// grammar and [`crate::mentions`]'s unlinked-mention scan (CT-F3). The last
/// two skips are what make it *unlinked*-mention prose rather than plain
/// prose: text that is already a link must never be offered as a link
/// candidate, and re-linking a Markdown link's label would break the link
/// it sits in.
///
/// Blocks are separated by `\n` so a needle can never match across a
/// paragraph/heading/table-cell boundary; inline runs are concatenated
/// verbatim (`**Deploy** checklist` reads as `Deploy checklist`, exactly
/// what the rendered artifact says). Offsets in the result do NOT map back
/// to the source — a caller that must EDIT the source verifies its splice
/// by re-parsing (see [`crate::mentions::apply_wikilink`]), never by
/// translating an offset.
pub fn prose_text(body: &str) -> String {
    use comrak::nodes::NodeValue;
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, body, &wikilink_parse_options());
    let mut out = String::new();
    // Iterative DFS in document order (children pushed reversed) — a
    // recursive walk would recurse per nesting level, and unlike
    // `descendants()` this one can skip a whole subtree.
    let mut stack: Vec<&comrak::nodes::AstNode<'_>> = vec![root];
    while let Some(node) = stack.pop() {
        let data = node.data();
        match &data.value {
            // Inert for mention scanning — skipped WHOLE, children included.
            NodeValue::Code(_)
            | NodeValue::CodeBlock(_)
            | NodeValue::Math(_)
            | NodeValue::HtmlInline(_)
            | NodeValue::HtmlBlock(_)
            | NodeValue::Raw(_)
            | NodeValue::FrontMatter(_)
            | NodeValue::Link(_)
            | NodeValue::Image(_)
            | NodeValue::WikiLink(_) => continue,
            NodeValue::Text(t) => out.push_str(t),
            NodeValue::SoftBreak | NodeValue::LineBreak => push_break(&mut out),
            v => {
                if v.block() {
                    push_break(&mut out);
                }
            }
        }
        drop(data);
        for child in node.reverse_children() {
            stack.push(child);
        }
    }
    out
}

/// Append a single `\n` separator, never doubling one and never leading.
fn push_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Strip the cosmetic bits a target may carry before resolution: a leading
/// `/`, Windows separators, and a trailing `#section` anchor. The result is
/// what [`resolve`] (and the SPA's render map key) matches on.
pub fn normalize_target(target: &str) -> String {
    let t = target.trim();
    // Drop a `#fragment` (section anchor) — resolution is per-artifact.
    let t = t.split('#').next().unwrap_or(t).trim();
    t.trim_start_matches('/').replace('\\', "/")
}

pub(crate) fn basename(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Filename without its extension (`ops/deploy.md` → `deploy`).
pub(crate) fn stem(rel: &str) -> &str {
    let b = basename(rel);
    b.rsplit_once('.').map(|(s, _)| s).unwrap_or(b)
}

fn is_id_shape(t: &str) -> bool {
    t.len() == 12
        && t.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Resolve a wikilink `target` against the corpus `docs`, corpus-local, by a
/// deterministic first-hit-wins ladder:
///
///   1. **id** — `target` is a 12-hex id of some doc.
///   2. **path** — `target` equals a doc's source-relative path, exact or with
///      the final extension elided (case-sensitive, the filesystem's rule).
///   3. **title** — case-insensitive exact title match (Obsidian's default).
///   4. **basename** — case-insensitive bare filename, with or without
///      extension (`deploy` / `deploy.md`).
///
/// Tiers 1–2 are inherently UNIQUE: an id and a *folder-qualified* path (even
/// ext-elided, `ops/deploy`) name at most one doc. Tiers 3–4 can match several
/// → [`Resolution::Ambiguous`]; the graph drops those (a title — or a bare
/// basename like `deploy` shared by `ops/deploy.md` + `infra/deploy.md` — isn't
/// an unambiguous edge), but a route can surface the candidates. Bare-basename
/// matching lives ONLY in tier 4 (which collects, so it can return Ambiguous);
/// folding it into tier 2's first-hit `.find()` would silently pick one of
/// several same-named files by index order — nondeterministic, and it would
/// flip on reindex. Empty/unmatched → [`Resolution::None`].
///
/// One-shot convenience over [`ResolveIndex`] — callers with many targets
/// against one candidate set (the edge-record hook, the notes routes) build
/// the index once and reuse it, avoiding an O(docs) re-scan (and the tier
/// 3–4 per-candidate re-lowercasing) per target.
pub fn resolve(target: &str, docs: &[DocLite]) -> Resolution {
    ResolveIndex::new(docs).resolve(target)
}

/// Precomputed lookup tables for [`resolve`]'s ladder over one candidate
/// set: the lowercased title/basename/stem keys are computed ONCE per doc
/// at build time, and each [`ResolveIndex::resolve`] is then hashmap work —
/// instead of a full O(docs) scan re-allocating lowercased strings per
/// target. Semantics are identical to the pre-index linear ladder by
/// construction: this is the ONLY implementation (the free function
/// delegates), so the golden grammar tests pin both entry points.
pub struct ResolveIndex<'a> {
    /// Tier 1 — the id set.
    ids: std::collections::HashSet<&'a str>,
    /// Tier 2 — exact rel_path AND ext-elided rel_path, keyed together with
    /// first-doc-wins inserts so a key claimed by an earlier doc (in slice
    /// order) is never overwritten — mirroring the linear `.find()`'s
    /// first-hit semantics when e.g. `ops/deploy` (extension-less file) and
    /// `ops/deploy.md` collide on the elided form.
    paths: std::collections::HashMap<&'a str, &'a str>,
    /// Tier 3 — lowercased title → ids in doc order.
    titles: std::collections::HashMap<String, Vec<&'a str>>,
    /// Tier 4 — lowercased basename + stem → ids in doc order (a doc whose
    /// basename equals its stem — no extension — is inserted once, like the
    /// linear filter that matched it once).
    basenames: std::collections::HashMap<String, Vec<&'a str>>,
}

impl<'a> ResolveIndex<'a> {
    pub fn new(docs: &'a [DocLite]) -> Self {
        let mut ids = std::collections::HashSet::with_capacity(docs.len());
        let mut paths: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::with_capacity(docs.len() * 2);
        let mut titles: std::collections::HashMap<String, Vec<&str>> =
            std::collections::HashMap::with_capacity(docs.len());
        let mut basenames: std::collections::HashMap<String, Vec<&str>> =
            std::collections::HashMap::with_capacity(docs.len());
        for d in docs {
            ids.insert(d.id.as_str());
            paths.entry(d.rel_path.as_str()).or_insert(d.id.as_str());
            paths.entry(strip_ext(&d.rel_path)).or_insert(d.id.as_str());
            titles
                .entry(d.title.to_lowercase())
                .or_default()
                .push(d.id.as_str());
            let base = basename(&d.rel_path).to_lowercase();
            let st = stem(&d.rel_path).to_lowercase();
            if st != base {
                basenames.entry(st).or_default().push(d.id.as_str());
            }
            basenames.entry(base).or_default().push(d.id.as_str());
        }
        Self {
            ids,
            paths,
            titles,
            basenames,
        }
    }

    /// The ladder — see [`resolve`] for the tier semantics.
    pub fn resolve(&self, target: &str) -> Resolution {
        let t = normalize_target(target);
        if t.is_empty() {
            return Resolution::None;
        }

        // 1. id passthrough.
        if is_id_shape(&t) && self.ids.contains(t.as_str()) {
            return Resolution::One(t);
        }

        // 2. source-relative path — exact, or with the final extension
        // elided. Both forms keep the folder, so they're unique (no bare
        // `stem` here — that's tier 4's job, where it can be Ambiguous).
        if let Some(id) = self.paths.get(t.as_str()) {
            return Resolution::One((*id).to_string());
        }

        // 3. exact title, case-insensitive.
        let tl = t.to_lowercase();
        if let Some(by_title) = self.titles.get(&tl) {
            match by_title.len() {
                1 => return Resolution::One(by_title[0].to_string()),
                n if n > 1 => {
                    return Resolution::Ambiguous(
                        by_title.iter().map(|id| (*id).to_string()).collect(),
                    )
                }
                _ => {}
            }
        }

        // 4. basename (with or without extension), case-insensitive.
        match self.basenames.get(&tl).map(Vec::as_slice) {
            Some([id]) => Resolution::One((*id).to_string()),
            Some(ids) if ids.len() > 1 => {
                Resolution::Ambiguous(ids.iter().map(|id| (*id).to_string()).collect())
            }
            _ => Resolution::None,
        }
    }
}

/// `ops/deploy.md` → `ops/deploy`. The extension is removed ONLY from the
/// final path segment, so a dotted directory with an extension-less file
/// (`a.b/c`) is left intact (the old `rsplit_once('.')` split on the dir dot).
fn strip_ext(rel: &str) -> &str {
    match rel.rsplit_once('/') {
        Some((dir, file)) => match file.rsplit_once('.') {
            // Keep `dir/` + the file's stem (slice at the byte before the dot).
            Some((s, _)) => &rel[..dir.len() + 1 + s.len()],
            None => rel,
        },
        None => rel.rsplit_once('.').map(|(s, _)| s).unwrap_or(rel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MI-close-out Unit 4 (2026-08) — the decisive evidence for why memory
    /// wikilinks are still declined: `parse_wikilinks` finds NOTHING inside
    /// ANY `<p>...</p>`-wrapped content, whether it's a full HTML document,
    /// a bare fragment, or several `<p>` blocks separated by blank lines —
    /// only genuinely tag-free plain text triggers the wikilink extension.
    /// See `kb_core::enrich`'s `EdgeRecordHook` doc for the full analysis of
    /// why this blocks widening its `is_markdown` gate to memory HTML.
    #[test]
    fn wikilinks_are_invisible_inside_any_p_wrapped_html_only_bare_text_works() {
        let full_doc = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Source Note</title><meta name=\"kb-category\" content=\"memory-user\"></head><body><h1>Source Note</h1><p>see [[Target Note]] and [[Nope]]</p></body></html>";
        assert_eq!(parse_wikilinks(full_doc), vec![], "full HTML document");

        let single_p = "<p>see [[Target Note]] and [[Nope]]</p>";
        assert_eq!(parse_wikilinks(single_p), vec![], "a single <p> fragment");

        let multi_p =
            "<p>see [[Target Note]] and [[Nope]]</p>\n\n<p>second para with [[Another]]</p>";
        assert_eq!(
            parse_wikilinks(multi_p),
            vec![],
            "multiple <p> blocks, blank-line separated"
        );

        // The control case: genuinely tag-free plain text DOES work — this
        // is what makes markdown notes' raw `.md` source parseable at all.
        let bare_text = "see [[Target Note]] and [[Nope]]";
        assert_eq!(
            parse_wikilinks(bare_text),
            vec![wl("Target Note", None), wl("Nope", None)],
            "bare text with no HTML tags"
        );
    }

    fn wl(target: &str, alias: Option<&str>) -> WikiLink {
        WikiLink {
            target: target.to_string(),
            alias: alias.map(str::to_string),
        }
    }

    #[test]
    fn parses_plain_and_aliased() {
        assert_eq!(
            parse_wikilinks("see [[Deploy checklist]] now"),
            vec![wl("Deploy checklist", None)]
        );
        assert_eq!(
            parse_wikilinks("see [[Deploy checklist|the checklist]]"),
            vec![wl("Deploy checklist", Some("the checklist"))]
        );
    }

    #[test]
    fn double_pipe_is_not_a_wikilink() {
        // comrak (wikilinks_title_after_pipe) rejects a second pipe entirely —
        // `[[a|b|c]]` yields NO wikilink. The SPA mirror (web/src/lib/wikilink.ts
        // parseWikilink) MUST agree (return null → leave it literal), or a
        // pipe-in-alias link would render dead in the SPA with no backlink.
        // Golden contract for invariant #29's dual-render lock-step.
        assert_eq!(parse_wikilinks("x [[a|b|c]] y"), vec![]);
        // Single pipe is fine (target before pipe, alias after).
        assert_eq!(parse_wikilinks("[[a|b]]"), vec![wl("a", Some("b"))]);
    }

    #[test]
    fn document_order_with_duplicates_kept() {
        let got = parse_wikilinks("[[a]] and [[b]] then [[a]] again");
        assert_eq!(got, vec![wl("a", None), wl("b", None), wl("a", None)]);
    }

    #[test]
    fn skips_code_span_and_fence() {
        assert_eq!(parse_wikilinks("inline `[[nope]]` here"), vec![]);
        let fenced = "```\n[[nope]]\n```\nreal [[yes]]\n";
        assert_eq!(parse_wikilinks(fenced), vec![wl("yes", None)]);
    }

    #[test]
    fn empty_target_is_not_a_link() {
        assert_eq!(parse_wikilinks("[[]] and [[ ]]"), vec![]);
    }

    #[test]
    fn target_in_path_and_id_forms() {
        assert_eq!(
            parse_wikilinks("[[ops/deploy.md]] [[ab12cd34ef56]]"),
            vec![wl("ops/deploy.md", None), wl("ab12cd34ef56", None)]
        );
    }

    fn doc(id: &str, rel: &str, title: &str) -> DocLite {
        DocLite {
            id: id.into(),
            rel_path: rel.into(),
            title: title.into(),
        }
    }

    #[test]
    fn resolve_by_id() {
        let docs = [doc("ab12cd34ef56", "ops/x.md", "X")];
        assert_eq!(
            resolve("ab12cd34ef56", &docs),
            Resolution::One("ab12cd34ef56".into())
        );
    }

    #[test]
    fn resolve_by_path_with_and_without_ext() {
        let docs = [doc("i1", "ops/deploy.md", "Deploy")];
        assert_eq!(
            resolve("ops/deploy.md", &docs),
            Resolution::One("i1".into())
        );
        assert_eq!(resolve("ops/deploy", &docs), Resolution::One("i1".into()));
        assert_eq!(
            resolve("/ops/deploy.md", &docs),
            Resolution::One("i1".into())
        );
    }

    #[test]
    fn resolve_by_title_case_insensitive() {
        let docs = [doc("i1", "n/a.md", "Deploy Checklist")];
        assert_eq!(
            resolve("deploy checklist", &docs),
            Resolution::One("i1".into())
        );
    }

    // invariant:29 resolution-ladder
    #[test]
    fn resolve_title_ambiguous() {
        let docs = [doc("i1", "a/x.md", "Notes"), doc("i2", "b/y.md", "notes")];
        match resolve("Notes", &docs) {
            Resolution::Ambiguous(ids) => {
                assert_eq!(ids.len(), 2);
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn resolve_by_basename() {
        let docs = [doc("i1", "deep/ops/deploy.md", "Some Other Title")];
        assert_eq!(resolve("deploy", &docs), Resolution::One("i1".into()));
        assert_eq!(resolve("deploy.md", &docs), Resolution::One("i1".into()));
    }

    // invariant:29 resolution-ladder
    #[test]
    fn resolve_bare_basename_ambiguous_across_folders() {
        // Two files share the stem `deploy` in different folders. `[[deploy]]`
        // (bare) must be Ambiguous (tier 4 collects), NOT a nondeterministic
        // first-hit — regression guard for the tier-2 stem bug. A
        // folder-qualified target still resolves uniquely (tier 2 strip_ext).
        let docs = [
            doc("ops_id", "ops/deploy.md", "Ops deploy"),
            doc("infra_id", "infra/deploy.md", "Infra deploy"),
        ];
        match resolve("deploy", &docs) {
            Resolution::Ambiguous(ids) => assert_eq!(ids.len(), 2),
            other => panic!("expected ambiguous, got {other:?}"),
        }
        assert_eq!(
            resolve("ops/deploy", &docs),
            Resolution::One("ops_id".into()),
            "a folder-qualified target stays unique via tier 2"
        );
        assert_eq!(
            resolve("infra/deploy.md", &docs),
            Resolution::One("infra_id".into())
        );
    }

    #[test]
    fn strip_ext_leaves_dotted_directory_intact() {
        // `a.b/c` (dotted dir, extension-less file) must NOT have its dir-dot
        // stripped — so target `a` does not false-match the path tier.
        let docs = [doc("i1", "a.b/c", "Title")];
        assert_eq!(resolve("a", &docs), Resolution::None);
        // The real path + the dir name still resolve where appropriate.
        assert_eq!(resolve("a.b/c", &docs), Resolution::One("i1".into()));
    }

    #[test]
    fn resolve_none_for_dangling() {
        let docs = [doc("i1", "a.md", "A")];
        assert_eq!(resolve("nonexistent", &docs), Resolution::None);
        assert_eq!(resolve("", &docs), Resolution::None);
    }

    #[test]
    fn normalize_strips_anchor_and_slash() {
        assert_eq!(normalize_target("/ops/x.md#section"), "ops/x.md");
        assert_eq!(normalize_target("  Title #frag "), "Title");
        assert_eq!(normalize_target("Title"), "Title");
    }

    #[test]
    fn resolve_index_reuse_matches_one_shot_resolve() {
        // The edge-record hook builds ONE index per note and resolves every
        // target through it; each answer must equal the one-shot form.
        let docs = [
            doc("ab12cd34ef56", "ops/x.md", "X"),
            doc("i1", "ops/deploy.md", "Deploy checklist"),
            doc("i2", "infra/deploy.md", "Infra deploy"),
            doc("i3", "a/notes.md", "Notes"),
            doc("i4", "b/notes.md", "notes"),
        ];
        let idx = ResolveIndex::new(&docs);
        for target in [
            "ab12cd34ef56",
            "ops/deploy.md",
            "ops/deploy",
            "deploy checklist",
            "deploy",
            "Notes",
            "notes.md",
            "dangling",
            "",
        ] {
            assert_eq!(idx.resolve(target), resolve(target, &docs), "{target:?}");
        }
    }

    #[test]
    fn path_tier_ext_elided_collision_is_first_doc_wins() {
        // `ops/deploy.md` elides to `ops/deploy`, colliding with the real
        // extension-less file. Tier 2 is first-hit in doc order (the linear
        // `.find()` contract) — golden-pinned so the hashmap build keeps
        // first-wins inserts.
        let a = doc("md_doc", "ops/deploy.md", "A");
        let b = doc("bare_doc", "ops/deploy", "B");
        assert_eq!(
            resolve("ops/deploy", &[a.clone(), b.clone()]),
            Resolution::One("md_doc".into())
        );
        assert_eq!(
            resolve("ops/deploy", &[b, a]),
            Resolution::One("bare_doc".into())
        );
    }

    // --- prose_text (CT-F3's scanner) ---------------------------------------

    #[test]
    fn prose_text_skips_code_span_and_fence() {
        // The whole point of reusing comrak: `parse_wikilinks`'s code-skip and
        // the mention scanner's code-skip are the SAME parse, not two rules
        // that could drift.
        let md = "real prose here\n\n`Deploy checklist` inline\n\n```\nDeploy checklist\n```\n";
        let prose = prose_text(md);
        assert!(prose.contains("real prose here"));
        assert!(
            !prose.contains("Deploy checklist"),
            "code span + fence must not reach the scanner: {prose:?}"
        );
    }

    #[test]
    fn prose_text_skips_existing_wikilink_and_link_labels() {
        // Text that is ALREADY a link is not an unlinked mention: neither a
        // `[[wikilink]]`'s display text nor a Markdown link's label.
        let md = "see [[Deploy checklist]] and [Deploy checklist](https://x/) and ![Deploy checklist](i.png)";
        let prose = prose_text(md);
        assert!(
            !prose.contains("Deploy checklist"),
            "already-linked text leaked into the scan: {prose:?}"
        );
        assert!(prose.contains("see"));
    }

    #[test]
    fn prose_text_joins_inline_runs_and_breaks_blocks() {
        // Inline emphasis is transparent (the reader sees one phrase), but a
        // block boundary is a hard separator so a needle can never match
        // across two paragraphs.
        assert_eq!(prose_text("**Deploy** checklist"), "Deploy checklist");
        assert_eq!(prose_text("ends with deploy\n\nchecklist starts"), {
            "ends with deploy\nchecklist starts"
        });
    }

    #[test]
    fn prose_text_keeps_plain_prose_and_headings() {
        let md = "# Ops runbook\n\nRead the Deploy checklist before shipping.\n";
        let prose = prose_text(md);
        assert!(prose.contains("Ops runbook"));
        assert!(prose.contains("Read the Deploy checklist before shipping."));
    }

    #[test]
    fn path_tier_beats_title_tier() {
        // A target that is both a valid path and someone else's title resolves
        // to the path (more specific), not the title.
        let docs = [
            doc("path_doc", "report.md", "Z"),
            doc("title_doc", "other.md", "report.md"),
        ];
        assert_eq!(
            resolve("report.md", &docs),
            Resolution::One("path_doc".into())
        );
    }
}
