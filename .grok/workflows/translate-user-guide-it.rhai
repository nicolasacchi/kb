let meta = #{
    name: "translate-user-guide-it",
    description: "Double-check English docs/guide, translate to Italian under docs/guide/it/, then double-check the Italian",
    phases: [
        #{ title: "English double-check", detail: "accuracy + links + shippability" },
        #{ title: "Fix English if needed", detail: "apply high-severity EN fixes before translate" },
        #{ title: "Translate to Italian", detail: "one agent per HTML page → docs/guide/it/" },
        #{ title: "Italian double-check", detail: "fluency + terminology + parity + links" },
        #{ title: "Fix Italian if needed", detail: "apply high/medium IT review fixes" },
    ],
    when_to_use: "When docs/guide needs an Italian localization pass with verification",
};

let findings_schema = #{
    "type": "object",
    "required": ["findings", "summary", "shippable"],
    "properties": #{
        "summary": #{ "type": "string" },
        "shippable": #{ "type": "boolean" },
        "findings": #{
            "type": "array",
            "maxItems": 12,
            "items": #{
                "type": "object",
                "required": ["severity", "page", "issue", "fix"],
                "properties": #{
                    "severity": #{ "type": "string" },
                    "page": #{ "type": "string" },
                    "issue": #{ "type": "string" },
                    "fix": #{ "type": "string" },
                },
            },
        },
    },
};

let fix_schema = #{
    "type": "object",
    "required": ["applied", "notes"],
    "properties": #{
        "applied": #{ "type": "array", "items": #{ "type": "string" } },
        "notes": #{ "type": "string" },
    },
};

let page_schema = #{
    "type": "object",
    "required": ["ok", "src", "dst", "notes"],
    "properties": #{
        "ok": #{ "type": "boolean" },
        "src": #{ "type": "string" },
        "dst": #{ "type": "string" },
        "notes": #{ "type": "string" },
    },
};

let en_root = "docs/guide";
let it_root = "docs/guide/it";
if args != () {
    if args.en_root != () { en_root = args.en_root; }
    if args.it_root != () { it_root = args.it_root; }
}

let pages = [
    "index.html",
    "start.html",
    "concepts.html",
    "search.html",
    "web.html",
    "comments.html",
    "agent.html",
    "organize.html",
    "operate.html",
];

// ─── Phase 1: English double-check ─────────────────────────────────
phase("English double-check");

let en_acc = "";
en_acc += "Double-check the ENGLISH standalone user guide at ";
en_acc += en_root;
en_acc += " AFTER prior review fixes.\n";
en_acc += "Use read_file + grep + shell (kb --help / kb tools) as needed.\n";
en_acc += "1. Read every HTML page under the guide (not it/ subfolder).\n";
en_acc += "2. Verify CLI examples still match live clap (list update/move/prune,\n";
en_acc += "   comments resolve, recollect --similar-to, token issue/revoke, search scope).\n";
en_acc += "3. Verify relative links resolve within the folder; no broken hrefs.\n";
en_acc += "4. Flag only REAL remaining defects. severity high|medium|low.\n";
en_acc += "5. shippable=true if no high-severity issues remain.\n";
en_acc += "Empty findings only after reading all pages.\n";

let en_link = "";
en_link += "Link and structure double-check for ENGLISH guide at ";
en_link += en_root;
en_link += ".\n";
en_link += "1. List all *.html and _assets/guide.css.\n";
en_link += "2. For every href= in those pages: relative targets must exist when\n";
en_link += "   served from the guide root alone (except http(s): and #anchors).\n";
en_link += "3. Prev/next chain must visit every page; sticky nav labels consistent.\n";
en_link += "4. shippable=true if structure is sound for localization.\n";
en_link += "Use read_file/grep; do not invent missing files.\n";

let en_results = parallel([
    #{
        prompt: en_acc,
        label: "en-accuracy",
        capability_mode: "execute",
        output_schema: findings_schema,
    },
    #{
        prompt: en_link,
        label: "en-links",
        capability_mode: "read-only",
        output_schema: findings_schema,
    },
]);

let en_findings = [];
let en_shippable = true;
let ri = 0;
for r in en_results {
    if r == () || !r.success {
        log("EN panel failed slot " + ri.to_string());
        en_shippable = false;
    } else {
        log("EN: " + r.output.summary);
        if r.output.shippable == false { en_shippable = false; }
        if r.output.findings != () {
            for f in r.output.findings { en_findings.push(f); }
        }
    }
    ri += 1;
}

let high_en = [];
for f in en_findings {
    if f.severity == "high" { high_en.push(f); }
}
log("EN findings=" + en_findings.len().to_string() + " high=" + high_en.len().to_string()
    + " shippable=" + en_shippable.to_string());

// ─── Phase 2: Fix English if needed ────────────────────────────────
phase("Fix English if needed");
if high_en.len() > 0 {
    let fix_blob = json_encode(high_en);
    let fix_p = "";
    fix_p += "Apply HIGH-severity fixes to the ENGLISH guide at ";
    fix_p += en_root;
    fix_p += " so localization can start from correct copy.\n";
    fix_p += "Use search_replace / write tools. Do not rewrite whole pages unless needed.\n";
    fix_p += "Do not touch docs/guide/it/ yet.\n";
    fix_p += "FINDINGS JSON:\n";
    fix_p += fix_blob;
    fix_p += "\nReturn applied[] paths you edited and notes.\n";
    let en_fix = agent(fix_p, #{
        label: "en-fix",
        capability_mode: "read-write",
        output_schema: fix_schema,
    });
    if en_fix != () && en_fix.success {
        log("EN fixes: " + en_fix.output.notes);
    } else {
        log("EN fix agent failed — continuing carefully");
    }
} else {
    log("No high-severity EN issues — skip EN fix");
    // keep phase visible with a no-op agent so budget/accounting stays clear? skip is fine
}

// ─── Phase 3: Translate ────────────────────────────────────────────
phase("Translate to Italian");

// glossary baked into every prompt for term lock
let glossary = "";
glossary += "TERMINOLOGY (lock these; do not invent synonyms mid-site):\n";
glossary += "- kb (product name) → kb\n";
glossary += "- artifact → artefatto\n";
glossary += "- corpus / kb (a corpus) → corpus / kb (keep 'kb' for named corpora)\n";
glossary += "- daemon → daemon (loanword OK) or demone only if natural; prefer daemon\n";
glossary += "- hybrid search → ricerca ibrida\n";
glossary += "- keyword / semantic → keyword / semantic (or lessicale / semantica once, then English mode names in code)\n";
glossary += "- memory (curated) → memoria\n";
glossary += "- session (episodic) → sessione\n";
glossary += "- reading list → lista di lettura\n";
glossary += "- note → nota\n";
glossary += "- wikilink → wikilink\n";
glossary += "- embedder / embedding → embedder / embedding\n";
glossary += "- bearer token → token bearer\n";
glossary += "- gallery / atlas / reader → galleria / atlante / reader (or lettore)\n";
glossary += "- CLI verbs and flags stay English exactly as in code fences\n";
glossary += "- Keep <code>, <pre>, command examples, paths, env vars, TOML keys UNTRANSLATED\n";
glossary += "- Keep id= anchors on sections the same as English (link targets stable)\n";

let tjobs = [];
for page in pages {
    let p = "";
    p += "Translate one HTML page of the kb user guide EN→IT.\n";
    p += glossary;
    p += "\nSOURCE (read fully with read_file): ";
    p += en_root;
    p += "/";
    p += page;
    p += "\nDEST (write full file with write tool): ";
    p += it_root;
    p += "/";
    p += page;
    p += "\n\nREQUIREMENTS:\n";
    p += "1. Full Italian translation of visible prose, headings, nav labels, lede, callouts, table cells that are prose.\n";
    p += "2. html lang=\"it\". title and meta description in Italian.\n";
    p += "3. CSS href must be ../_assets/guide.css (one level up to shared assets).\n";
    p += "4. Internal page links stay same filenames (start.html etc.) — they are siblings under it/.\n";
    p += "5. In-page #anchors unchanged. Cross-page #fragments ok.\n";
    p += "6. Language switcher in .docnav after home link:\n";
    p += "   <span class=\"lang\"><a href=\"../";
    p += page;
    p += "\">EN</a><span class=\"sep\">·</span><a href=\"";
    p += page;
    p += "\" aria-current=\"page\">IT</a></span>\n";
    p += "7. Natural technical Italian — not machine-literal. Formal but friendly (dare/tu sparingly; prefer impersonal/voi-neutral).\n";
    p += "8. Do NOT leave English body paragraphs untranslated. Nav short labels may be concise Italian.\n";
    p += "9. Preserve structure: sections, TOC, callout classes, table-scroll wrappers, skip-link, footer.\n";
    p += "10. Create parent dir if needed. After write, re-read dest to confirm non-empty.\n";
    p += "Return ok=true only if dest file written successfully.\n";
    tjobs.push(#{
        prompt: p,
        label: "it:" + page,
        capability_mode: "read-write",
        output_schema: page_schema,
    });
}
let tresults = parallel(tjobs);

let translated = [];
let failed_pages = [];
let ti = 0;
for r in tresults {
    let page = pages[ti];
    if r == () || !r.success || r.output.ok != true {
        log("TRANSLATE FAIL: " + page);
        failed_pages.push(page);
    } else {
        log("translated " + page + " → " + r.output.dst);
        translated.push(page);
    }
    ti += 1;
}

if failed_pages.len() > 0 {
    // retry failures once serially
    for page in failed_pages {
        let p = "";
        p += "RETRY translate EN→IT. Previous attempt failed.\n";
        p += glossary;
        p += "\nRead ";
        p += en_root;
        p += "/";
        p += page;
        p += " and write complete Italian HTML to ";
        p += it_root;
        p += "/";
        p += page;
        p += " with lang=it, css ../_assets/guide.css, lang switcher EN/IT, code untranslated.\n";
        p += "ok=true only after successful write.\n";
        let rr = agent(p, #{
            label: "retry:" + page,
            capability_mode: "read-write",
            output_schema: page_schema,
        });
        if rr != () && rr.success && rr.output.ok == true {
            log("retry ok " + page);
            translated.push(page);
        } else {
            log("retry FAIL " + page);
        }
    }
}

// Also add EN language switchers on English pages pointing to it/
let en_sw = "";
en_sw += "Add a language switcher to EVERY English HTML page under ";
en_sw += en_root;
en_sw += " (not under it/).\n";
en_sw += "In each page's .docnav, after the home link (or after .sep following home),\n";
en_sw += "insert if missing:\n";
en_sw += "<span class=\"lang\"><a href=\"PAGE\" aria-current=\"page\">EN</a>";
en_sw += "<span class=\"sep\">·</span><a href=\"it/PAGE\">IT</a></span>\n";
en_sw += "where PAGE is that file's basename (e.g. start.html → it/start.html).\n";
en_sw += "Use search_replace; keep existing nav. Add minimal CSS only if needed:\n";
en_sw += "in _assets/guide.css, .lang { margin-left: .35rem; } .lang a[aria-current]{font-weight:700}\n";
en_sw += "if not already present.\n";
en_sw += "Return list of files edited.\n";
let sw = agent(en_sw, #{
    label: "en-lang-switcher",
    capability_mode: "read-write",
    output_schema: fix_schema,
});
if sw != () && sw.success {
    log("EN switchers: " + sw.output.notes);
}

// ─── Phase 4: Italian double-check ─────────────────────────────────
phase("Italian double-check");

let it_flu = "";
it_flu += "Review ITALIAN user guide at ";
it_flu += it_root;
it_flu += " for fluency and natural technical Italian.\n";
it_flu += "1. Read every *.html fully with read_file.\n";
it_flu += "2. Flag machine-literal phrasing, anglicismi awkward where Italian is natural,\n";
it_flu += "   mixed-language sentences, untranslated English body prose, grammar errors.\n";
it_flu += "3. Do NOT flag intentional untranslated code/commands/product names.\n";
it_flu += "4. shippable=true if a native IT reader can use the guide comfortably.\n";
it_flu += "severity high|medium|low; concrete fix text in Italian or describe the edit.\n";

let it_term = "";
it_term += "Terminology consistency check for Italian guide at ";
it_term += it_root;
it_term += ".\n";
it_term += "Expected locks: artefatto, memoria vs sessione (curated vs episodic),\n";
it_term += "lista di lettura, nota, ricerca ibrida, daemon, kb untranslated product name.\n";
it_term += "Flag same concept translated differently across pages.\n";
it_term += "Also compare each IT page to EN sibling in ";
it_term += en_root;
it_term += " for missing sections (parity).\n";
it_term += "shippable=true if terms stable and no missing sections.\n";
it_term += "Use read_file on pairs EN/IT.\n";

let it_link = "";
it_link += "Link integrity for Italian guide at ";
it_link += it_root;
it_link += ".\n";
it_link += "1. Every page must exist for the EN set: index start concepts search web comments agent organize operate.\n";
it_link += "2. CSS must be ../_assets/guide.css and that file must exist at docs/guide/_assets/guide.css.\n";
it_link += "3. href targets among it/*.html must resolve; lang switcher EN must point to ../PAGE.\n";
it_link += "4. html lang=it on all pages.\n";
it_link += "shippable=true only if all pages present and links ok.\n";
it_link += "Use list_dir + grep + read_file.\n";

let it_results = parallel([
    #{
        prompt: it_flu,
        label: "it-fluency",
        capability_mode: "read-only",
        output_schema: findings_schema,
    },
    #{
        prompt: it_term,
        label: "it-terms",
        capability_mode: "read-only",
        output_schema: findings_schema,
    },
    #{
        prompt: it_link,
        label: "it-links",
        capability_mode: "read-only",
        output_schema: findings_schema,
    },
]);

let it_findings = [];
let it_shippable = true;
let ii = 0;
for r in it_results {
    if r == () || !r.success {
        log("IT panel failed " + ii.to_string());
        it_shippable = false;
    } else {
        log("IT: " + r.output.summary);
        if r.output.shippable == false { it_shippable = false; }
        if r.output.findings != () {
            for f in r.output.findings { it_findings.push(f); }
        }
    }
    ii += 1;
}

// ─── Phase 5: Fix Italian if needed ────────────────────────────────
phase("Fix Italian if needed");

let fix_it_list = [];
for f in it_findings {
    if f.severity == "high" || f.severity == "medium" {
        fix_it_list.push(f);
    }
}

if fix_it_list.len() > 0 {
    let blob = json_encode(fix_it_list);
    let fp = "";
    fp += "Apply high and medium fixes to the ITALIAN guide at ";
    fp += it_root;
    fp += ".\n";
    fp += "Use search_replace/write. Preserve code fences and CLI commands in English.\n";
    fp += "Keep terminology locks (artefatto, memoria/sessione, lista di lettura, …).\n";
    fp += "FINDINGS:\n";
    fp += blob;
    fp += "\nReturn applied files and notes.\n";
    let it_fix = agent(fp, #{
        label: "it-fix",
        capability_mode: "read-write",
        output_schema: fix_schema,
    });
    if it_fix != () && it_fix.success {
        log("IT fixes: " + it_fix.output.notes);
    } else {
        log("IT fix agent failed");
        it_shippable = false;
    }
} else {
    log("No high/medium IT findings — skip fix");
}

// ─── Report ────────────────────────────────────────────────────────
let report = "# Guide EN check + IT translation\n\n";
report += "## English\n";
report += "- findings: ";
report += en_findings.len().to_string();
report += " (high ";
report += high_en.len().to_string();
report += ")\n";
report += "- shippable gate: ";
report += en_shippable.to_string();
report += "\n\n## Translation\n";
report += "- pages ok: ";
report += translated.len().to_string();
report += " / ";
report += pages.len().to_string();
report += "\n";
report += "- dest: ";
report += it_root;
report += "\n\n## Italian review\n";
report += "- findings: ";
report += it_findings.len().to_string();
report += "\n";
report += "- shippable: ";
report += it_shippable.to_string();
report += "\n\n## EN findings\n";
for f in en_findings {
    report += "- **";
    report += f.severity;
    report += "** `";
    report += f.page;
    report += "`: ";
    report += f.issue;
    report += " → ";
    report += f.fix;
    report += "\n";
}
report += "\n## IT findings\n";
for f in it_findings {
    report += "- **";
    report += f.severity;
    report += "** `";
    report += f.page;
    report += "`: ";
    report += f.issue;
    report += " → ";
    report += f.fix;
    report += "\n";
}
let rpath = write_scratch_file("translate-it-report.md", report);

complete(#{
    summary: "EN shippable=" + en_shippable.to_string()
        + " translated=" + translated.len().to_string()
        + "/" + pages.len().to_string()
        + " IT shippable=" + it_shippable.to_string(),
    en_finding_count: en_findings.len(),
    en_high: high_en.len(),
    translated_count: translated.len(),
    it_finding_count: it_findings.len(),
    it_shippable: it_shippable,
    report_path: rpath,
    it_root: it_root,
});
