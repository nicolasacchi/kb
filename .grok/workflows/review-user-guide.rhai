let meta = #{
    name: "review-user-guide",
    description: "Review the standalone docs/guide HTML user guide for accuracy, completeness, and readability",
    phases: [
        #{ title: "Parallel review", detail: "accuracy + completeness + readability in parallel" },
        #{ title: "Synthesize", detail: "merge findings into a prioritized fix list" },
    ],
};

let findings_schema = #{
    "type": "object",
    "required": ["findings", "summary"],
    "properties": #{
        "summary": #{ "type": "string" },
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

let synth_schema = #{
    "type": "object",
    "required": ["priority_fixes", "overall"],
    "properties": #{
        "overall": #{ "type": "string" },
        "priority_fixes": #{
            "type": "array",
            "maxItems": 15,
            "items": #{
                "type": "object",
                "required": ["priority", "page", "action"],
                "properties": #{
                    "priority": #{ "type": "string" },
                    "page": #{ "type": "string" },
                    "action": #{ "type": "string" },
                },
            },
        },
    },
};

let guide_root = if args == () { () } else { args.guide_root };
if guide_root == () {
    guide_root = "docs/guide";
}

phase("Parallel review");

let accuracy_prompt = "";
accuracy_prompt += "You are reviewing a STANDALONE HTML user guide for the kb project at ";
accuracy_prompt += guide_root;
accuracy_prompt += ".\n\n";
accuracy_prompt += "TASK: factual accuracy only. Use read_file and grep — do not invent APIs.\n";
accuracy_prompt += "1. List all HTML pages under the guide folder and read each fully.\n";
accuracy_prompt += "2. Cross-check claims against the live product:\n";
accuracy_prompt += "   - README.md (status, five concepts, non-goals, TUI retired, install)\n";
accuracy_prompt += "   - shell: kb tools (verb names); crates/ layout (no kb-tui)\n";
accuracy_prompt += "   - docs/quickstart.md for install/bind/token behavior\n";
accuracy_prompt += "3. Flag: wrong verbs, retired features presented as live (esp. TUI), wrong ports,\n";
accuracy_prompt += "   wrong auth model, session/memory confusion, outdated version claims.\n";
accuracy_prompt += "4. severity: high|medium|low. fix must be concrete (what to change to what).\n";
accuracy_prompt += "5. Empty findings only if you read every page and spot-checked README/tools.\n";

let complete_prompt = "";
complete_prompt += "You are reviewing a STANDALONE HTML user guide for kb at ";
complete_prompt += guide_root;
complete_prompt += ".\n\n";
complete_prompt += "TASK: completeness for a NEW operator (human or coding agent).\n";
complete_prompt += "1. Read every HTML page in the guide folder.\n";
complete_prompt += "2. Compare coverage to README five concepts + install + non-goals + CLI surfaces:\n";
complete_prompt += "   search, web reader, comments, memory, sessions (why/recollect), lists, notes,\n";
complete_prompt += "   share, fleet (post-TUI), tokens, loopback safety.\n";
complete_prompt += "3. Flag missing must-know topics or dead-end pages (no next step).\n";
complete_prompt += "4. Do NOT demand HTTP API tables or architecture invariants — those stay in main docs.\n";
complete_prompt += "5. severity high|medium|low; concrete fix string.\n";

let read_prompt = "";
read_prompt += "You are reviewing READABILITY of the standalone HTML guide at ";
read_prompt += guide_root;
read_prompt += ".\n\n";
read_prompt += "TASK: reading experience only (not product accuracy).\n";
read_prompt += "1. Read guide.css and every HTML page.\n";
read_prompt += "2. Check: sticky nav consistency, skip links, serif/sans hierarchy, TOC, callouts,\n";
read_prompt += "   mobile columns, dark mode, relative-only links (must serve alone without repo root),\n";
read_prompt += "   jargon unexplained on first use, walls of text, broken page-prev/next chain.\n";
read_prompt += "3. Flag absolute links out of the folder that would 404 when served alone (except\n";
read_prompt += "   intentional external https URLs and localhost daemon URLs).\n";
read_prompt += "4. severity high|medium|low; concrete fix.\n";

let results = parallel([
    #{
        prompt: accuracy_prompt,
        label: "accuracy",
        capability_mode: "read-only",
        output_schema: findings_schema,
    },
    #{
        prompt: complete_prompt,
        label: "completeness",
        capability_mode: "read-only",
        output_schema: findings_schema,
    },
    #{
        prompt: read_prompt,
        label: "readability",
        capability_mode: "read-only",
        output_schema: findings_schema,
    },
]);

let all_findings = [];
let i = 0;
let labels = ["accuracy", "completeness", "readability"];
for r in results {
    let label = labels[i];
    if r == () || !r.success {
        log("panel failed: " + label);
    } else if r.output.findings != () {
        log(label + ": " + r.output.summary);
        for f in r.output.findings {
            // stamp dimension into page field if empty
            all_findings.push(f);
        }
    }
    i += 1;
}

if all_findings.len() == 0 {
    complete(#{
        summary: "No findings from any panel",
        priority_fixes: [],
        overall: "clean",
    });
}

phase("Synthesize");

let blob = json_encode(all_findings);
let synth_prompt = "";
synth_prompt += "You are synthesizing review findings for the kb docs/guide HTML user guide.\n";
synth_prompt += "Deduplicate and prioritize. Prefer high-severity factual errors first, then\n";
synth_prompt += "completeness gaps, then readability polish. Max 15 priority_fixes.\n";
synth_prompt += "Each action must be an implementable edit instruction (file + change).\n";
synth_prompt += "overall: one short paragraph on whether the guide is shippable.\n\n";
synth_prompt += "FINDINGS JSON:\n";
synth_prompt += blob;

let synth = agent(synth_prompt, #{
    label: "synthesize",
    capability_mode: "read-only",
    output_schema: synth_schema,
});

if synth == () || !synth.success || synth.output.priority_fixes == () {
    complete(#{
        summary: "Synthesis failed; raw finding count " + all_findings.len().to_string(),
        raw_count: all_findings.len(),
        overall: "incomplete-review",
    });
}

let report = "# User guide review\n\n";
report += synth.output.overall;
report += "\n\n## Priority fixes\n\n";
for fix in synth.output.priority_fixes {
    report += "- **";
    report += fix.priority;
    report += "** `";
    report += fix.page;
    report += "`: ";
    report += fix.action;
    report += "\n";
}
let path = write_scratch_file("guide-review.md", report);

complete(#{
    summary: synth.output.overall,
    priority_fixes: synth.output.priority_fixes,
    report_path: path,
    raw_finding_count: all_findings.len(),
});
