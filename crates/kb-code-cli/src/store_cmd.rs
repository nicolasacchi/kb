//! RS-U3 — `kb-code store …`: the kb-owned internal review store
//! (README §12). Every verb talks to a running daemon:
//!
//! * `store show --repo R` — the store card (`GET /api/repos/R/store`);
//! * `store members --repo R` — the clones sharing R's store;
//! * `store doctor --repo R` — the card's doctor findings plus the fetch
//!   credential; exits 1 when any finding is an `error`;
//! * `store sync --repo R [--offline]` — seed an absent store or sync a
//!   ready one (loopback-only on the daemon);
//! * `store set-base-url --repo R <URL>` — the ladder's explicit rung;
//! * `store credentials --repo R [--test]` — the fetch credential as last
//!   resolved; `--test` walks the ladder live (loopback-only; runs `gh`).
//! * `store gc --repo R [--dry-run|--yes]` — the store-wide ref GC (RS-U9,
//!   README §5.4): default (or `--dry-run`) computes candidates without
//!   deleting anything; `--yes` applies (with old-value guards) and, if
//!   the restore guard was flagged (a detected DB restore), acknowledges
//!   it — the ONE way to unblock scheduled GC after a restore
//!   (loopback-only).
//! * `store maintain --repo R [--task daily|weekly|monthly]` — run the
//!   git-housekeeping cadences now: the named one, or whatever is due
//!   (RS-U9, loopback-only).
//!
//! `--json` prints the D20 envelope (`envelope::print_ok`/`print_err`).
//! Exit codes: the shipped table (1 generic, 3 conflict — incl. a store
//! that is still seeding, 4 refused, 5 unreachable) plus
//! [`envelope::EXIT_UPSTREAM`] (6), [`envelope::EXIT_PARTIAL`] (7) and
//! [`envelope::EXIT_NOT_FOUND`] (8).

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::Value;

use crate::envelope;

#[derive(Subcommand, Debug)]
pub enum StoreCmd {
    /// Show a repo's review store: key, state, base URL, members, disk.
    Show {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// List the clones (members) sharing a repo's review store.
    Members {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Check a repo's review store and fetch credential; exit 1 on errors.
    Doctor {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Seed (if absent) or sync (if ready) a repo's store. Loopback-only.
    Sync {
        #[arg(long)]
        repo: String,
        /// Skip the network base-branch fetch (local member fetches only).
        #[arg(long)]
        offline: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Register a repo against an explicit forge URL (never re-keys a
    /// store). Loopback-only.
    SetBaseUrl {
        #[arg(long)]
        repo: String,
        /// e.g. https://github.com/acme/widgets.git
        url: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the store's fetch credential (never the secret). `--test`
    /// resolves it live and checks the helper answers. Loopback-only with
    /// `--test`.
    Credentials {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        test: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Store-wide ref GC (RS-U9). Default is a dry run; `--yes` applies
    /// and acknowledges the restore guard if it was flagged. Loopback-only.
    Gc {
        #[arg(long)]
        repo: String,
        /// Compute candidates only (the default when neither flag is
        /// given — kept for an explicit, self-documenting invocation).
        #[arg(long)]
        dry_run: bool,
        /// Apply the deletions. The restore guard's ONE acknowledgement
        /// path: if a DB restore was detected, this both applies and
        /// clears the flag.
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Run the git-housekeeping cadences now (RS-U9). `--task` forces one
    /// cadence regardless of when it last ran; omitted runs whatever is
    /// due. Loopback-only.
    Maintain {
        #[arg(long)]
        repo: String,
        /// `daily` | `weekly` | `monthly`.
        #[arg(long)]
        task: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

fn enc(repo: &str) -> String {
    // Repo names are config-chosen; percent-encode everything outside the
    // unreserved set so a name can never retarget the path.
    repo.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn client(timeout: Duration) -> Result<reqwest::Client> {
    crate::client_builder()
        .timeout(timeout)
        .build()
        .context("build http client")
}

async fn get(daemon: &str, path: &str) -> Result<(reqwest::StatusCode, Value)> {
    let c = client(Duration::from_secs(30))?;
    crate::get_json_raw(&c, daemon, path, &[]).await
}

async fn post(
    daemon: &str,
    path: &str,
    body: &Value,
    timeout: Duration,
) -> Result<(reqwest::StatusCode, Value)> {
    let c = client(timeout)?;
    crate::post_json_raw(&c, daemon, path, body).await
}

/// Map a non-2xx daemon answer to `(code, exit)`.
fn failure(status: reqwest::StatusCode, body: &Value) -> (String, i32) {
    let code = body["code"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            body["type"]
                .as_str()
                .and_then(|t| t.strip_prefix("urn:kb:errors:"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("http-{}", status.as_u16()));
    let exit = match status.as_u16() {
        404 => envelope::EXIT_NOT_FOUND,
        401 | 403 => envelope::EXIT_REFUSED,
        409 | 503 => envelope::EXIT_CONFLICT,
        _ => envelope::EXIT_GENERIC,
    };
    (code, exit)
}

fn fail(json: bool, status: reqwest::StatusCode, body: &Value) -> ! {
    let (code, exit) = failure(status, body);
    let msg = body["error"]
        .as_str()
        .unwrap_or("request failed")
        .to_string();
    let hint = match code.as_str() {
        "store-seeding" => Some("the store is seeding in the background; retry shortly"),
        "base-url-ambiguous" => {
            Some("run `kb-code store set-base-url --repo R <url>` or set [[review.repos]] base_url")
        }
        "not-found" => Some("check the repo name with `kb-code repos`"),
        _ => None,
    };
    if json {
        envelope::print_err(&code, &msg, hint);
    } else {
        eprintln!("error ({code}): {msg}");
        if let Some(h) = hint {
            eprintln!("hint: {h}");
        }
    }
    std::process::exit(exit)
}

fn s(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::String(x) => x.clone(),
        other => other.to_string(),
    }
}

fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < U.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    format!("{f:.1} {}", U[i])
}

fn print_card(card: &Value) {
    println!("repo:        {}", s(&card["repo"]));
    let st = &card["store"];
    if st.is_null() {
        println!("store:       none");
        if let Some(r) = card.get("registration").filter(|r| !r.is_null()) {
            println!("registration: {}", s(&r["outcome"]));
        }
    } else {
        println!("store key:   {}", s(&st["store_key"]));
        println!(
            "state:       {}{}",
            s(&st["state"]),
            st["state_code"]
                .as_str()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default()
        );
        println!(
            "base url:    {} [{}]",
            s(&st["base_url"]),
            s(&st["base_url_source"])
        );
        println!(
            "forge:       {} ({})",
            s(&st["forge_kind"]),
            s(&st["forge_verified"])
        );
        println!("git dir:     {}", s(&st["git_dir"]));
        if let Some(d) = card.get("disk").filter(|d| !d.is_null()) {
            println!(
                "disk:        {} · {} pack(s) · {} loose",
                human_bytes(d["total_bytes"].as_u64().unwrap_or(0)),
                s(&d["packs"]),
                s(&d["loose_objects"])
            );
        }
        let members: Vec<String> = card["members"]
            .as_array()
            .map(|a| a.iter().map(|m| s(&m["name"])).collect())
            .unwrap_or_default();
        println!("members:     {}", members.join(", "));
    }
    print_doctor(card);
}

fn print_doctor(card: &Value) {
    if let Some(ds) = card["doctor"].as_array() {
        for d in ds {
            println!(
                "  [{}] {}: {}",
                s(&d["level"]),
                s(&d["code"]),
                s(&d["message"])
            );
        }
    }
}

pub async fn run(cmd: StoreCmd) -> Result<()> {
    match cmd {
        StoreCmd::Show { repo, daemon, json } => {
            let (st, body) = get(&daemon, &format!("/api/repos/{}/store", enc(&repo))).await?;
            if !st.is_success() {
                fail(json, st, &body);
            }
            if json {
                envelope::print_ok("kbc-store/1", &body, vec![], false, None);
            } else {
                print_card(&body);
            }
        }
        StoreCmd::Members { repo, daemon, json } => {
            let (st, body) = get(&daemon, &format!("/api/repos/{}/store", enc(&repo))).await?;
            if !st.is_success() {
                fail(json, st, &body);
            }
            let members = body["members"].clone();
            let empty = members.as_array().is_none_or(|a| a.is_empty());
            if json {
                envelope::print_ok(
                    "kbc-store-members/1",
                    serde_json::json!({
                        "repo": repo,
                        "store_key": body["store"]["store_key"],
                        "members": members,
                    }),
                    vec![],
                    false,
                    empty.then_some("store-not-registered"),
                );
            } else if empty {
                println!("{repo}: no store yet");
            } else {
                println!("store {}", s(&body["store"]["store_key"]));
                for m in members.as_array().into_iter().flatten() {
                    println!(
                        "  {:<24} id {:<4} remote {}",
                        s(&m["name"]),
                        s(&m["repo_id"]),
                        s(&m["remote"])
                    );
                }
            }
        }
        StoreCmd::Doctor { repo, daemon, json } => {
            let (st, card) = get(&daemon, &format!("/api/repos/{}/store", enc(&repo))).await?;
            if !st.is_success() {
                fail(json, st, &card);
            }
            let (cst, cred) =
                get(&daemon, &format!("/api/repos/{}/credentials", enc(&repo))).await?;
            let cred = if cst.is_success() { cred } else { Value::Null };
            let findings = card["doctor"].as_array().cloned().unwrap_or_default();
            let errors = findings.iter().filter(|d| d["level"] == "error").count();
            if json {
                envelope::print_ok(
                    "kbc-store-doctor/1",
                    serde_json::json!({
                        "repo": repo,
                        "findings": findings,
                        "credential": cred["fetch"],
                        "errors": errors,
                    }),
                    vec![],
                    errors > 0,
                    None,
                );
            } else {
                println!("store doctor: {repo}");
                print_doctor(&card);
                let f = &cred["fetch"];
                if f["resolved"] == true {
                    println!(
                        "  credential: {} ({}){}",
                        s(&f["cred_kind"]),
                        s(&f["account"]),
                        if f["broader_than_needed"] == true {
                            " · broader than needed"
                        } else {
                            ""
                        }
                    );
                } else {
                    println!("  credential: not resolved yet (`kb-code store credentials --test`)");
                }
                if findings.is_empty() {
                    println!("  ok");
                }
            }
            if errors > 0 {
                std::process::exit(envelope::EXIT_GENERIC);
            }
        }
        StoreCmd::Sync {
            repo,
            offline,
            daemon,
            json,
        } => {
            let path = format!(
                "/api/repos/{}/store/sync{}",
                enc(&repo),
                if offline { "?offline=true" } else { "" }
            );
            // Seeding a large project is minutes of local fetch.
            let (st, body) =
                post(&daemon, &path, &Value::Null, Duration::from_secs(4 * 3600)).await?;
            if !st.is_success() {
                fail(json, st, &body);
            }
            let report = &body["report"];
            let base_state = report["base"]["state"].as_str().unwrap_or("");
            let partial = report["member_errors"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
            let upstream = base_state == "failed";
            if json {
                envelope::print_ok("kbc-store-sync/1", &body, vec![], partial || upstream, None);
            } else {
                println!("{}: {}", repo, s(&body["action"]));
                for m in report["members"].as_array().into_iter().flatten() {
                    println!(
                        "  work-{}: {} heads, {} review refs{}",
                        s(&m["repo_id"]),
                        s(&m["heads"]),
                        s(&m["review_refs"]),
                        m["conflicts"]
                            .as_array()
                            .filter(|c| !c.is_empty())
                            .map(|c| format!(", {} conflict(s)", c.len()))
                            .unwrap_or_default()
                    );
                }
                println!(
                    "  base: {}{}",
                    base_state,
                    report["base"]["code"]
                        .as_str()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default()
                );
                let missing = report["objects_missing"].as_array().map_or(0, Vec::len);
                if missing > 0 {
                    println!("  {missing} review(s) objects-missing");
                }
            }
            if upstream {
                std::process::exit(envelope::EXIT_UPSTREAM);
            }
            if partial {
                std::process::exit(envelope::EXIT_PARTIAL);
            }
        }
        StoreCmd::SetBaseUrl {
            repo,
            url,
            daemon,
            json,
        } => {
            let (st, body) = post(
                &daemon,
                &format!("/api/repos/{}/store/base-url", enc(&repo)),
                &serde_json::json!({ "base_url": url }),
                Duration::from_secs(60),
            )
            .await?;
            if !st.is_success() {
                fail(json, st, &body);
            }
            if json {
                envelope::print_ok("kbc-store/1", &body, vec![], false, None);
            } else {
                println!(
                    "{}: {} ({})",
                    repo,
                    s(&body["registration"]["store_key"]),
                    s(&body["registration"]["source"])
                );
            }
        }
        StoreCmd::Credentials {
            repo,
            test,
            daemon,
            json,
        } => {
            let path = format!(
                "/api/repos/{}/credentials{}",
                enc(&repo),
                if test { "/test" } else { "" }
            );
            let (st, body) = if test {
                post(&daemon, &path, &Value::Null, Duration::from_secs(90)).await?
            } else {
                get(&daemon, &path).await?
            };
            if !st.is_success() {
                fail(json, st, &body);
            }
            if json {
                envelope::print_ok("kbc-credentials/1", &body, vec![], false, None);
            } else {
                let f = &body["fetch"];
                println!("repo:        {repo}");
                println!("fetch:       {}", s(&f["cred_kind"]));
                println!("account:     {}", s(&f["account"]));
                println!("reason:      {}", s(&f["reason"]));
                if f["broader_than_needed"] == true {
                    println!("scopes:      broader than needed (used read-only: fetch + GET)");
                }
                if f["amber"] == true {
                    println!("note:        ambient environment (legacy, amber)");
                }
                if test {
                    println!("helper:      {}", s(&f["helper_answers"]));
                    for sk in f["skipped"].as_array().into_iter().flatten() {
                        println!(
                            "  skipped {}: {} ({})",
                            s(&sk["rung"]),
                            s(&sk["reason"]),
                            s(&sk["class"])
                        );
                    }
                }
            }
        }
        StoreCmd::Gc {
            repo,
            dry_run,
            yes,
            daemon,
            json,
        } => {
            let apply = yes && !dry_run;
            let (st, body) = post(
                &daemon,
                &format!("/api/repos/{}/store/gc", enc(&repo)),
                &serde_json::json!({ "yes": apply }),
                Duration::from_secs(10 * 60),
            )
            .await?;
            if !st.is_success() {
                fail(json, st, &body);
            }
            if json {
                envelope::print_ok("kbc-store-gc/1", &body, vec![], false, None);
            } else {
                let report = &body["report"];
                println!(
                    "{}: gc {} — {} candidate(s){}",
                    repo,
                    s(&report["reason"]),
                    s(&report["candidates"]),
                    if report["applied"] == true {
                        ", applied"
                    } else {
                        ""
                    }
                );
            }
        }
        StoreCmd::Maintain {
            repo,
            task,
            daemon,
            json,
        } => {
            let body_in = match &task {
                Some(t) => serde_json::json!({ "task": t }),
                None => serde_json::json!({}),
            };
            let (st, body) = post(
                &daemon,
                &format!("/api/repos/{}/store/maintain", enc(&repo)),
                &body_in,
                Duration::from_secs(30 * 60),
            )
            .await?;
            if !st.is_success() {
                fail(json, st, &body);
            }
            let report = &body["report"];
            let errors = report["errors"].as_array().map(Vec::len).unwrap_or(0);
            if json {
                envelope::print_ok("kbc-store-maintain/1", &body, vec![], errors > 0, None);
            } else {
                let tasks: Vec<String> = report["tasks_run"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(s)
                    .collect();
                println!("{}: ran [{}]", repo, tasks.join(", "));
                if let Some(inv) = report.get("invariant").filter(|v| !v.is_null()) {
                    println!(
                        "  invariant: {} missing, {} ok, {} recovered, {} recreated",
                        s(&inv["objects_missing"]),
                        s(&inv["objects_ok"]),
                        s(&inv["recovered_by_sha"]),
                        s(&inv["refs_recreated"]),
                    );
                }
                if let Some(gc) = report.get("gc").filter(|v| !v.is_null()) {
                    println!(
                        "  gc: {} — {} candidate(s){}",
                        s(&gc["reason"]),
                        s(&gc["candidates"]),
                        if gc["applied"] == true {
                            ", applied"
                        } else {
                            ""
                        }
                    );
                }
                let swept = report["swept_tmp_pack"].as_u64().unwrap_or(0);
                if swept > 0 {
                    println!("  swept {swept} stale tmp_pack file(s)");
                }
                for e in report["errors"].as_array().into_iter().flatten() {
                    println!("  error: {}", s(e));
                }
            }
            if errors > 0 {
                std::process::exit(envelope::EXIT_PARTIAL);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names_are_percent_encoded() {
        assert_eq!(enc("widgets-01"), "widgets-01");
        assert_eq!(enc("a/../b"), "a%2F..%2Fb");
        assert_eq!(enc("x y?z"), "x%20y%3Fz");
    }

    #[test]
    fn failures_map_onto_the_exit_table() {
        let seeding = serde_json::json!({"type": "urn:kb:errors:store-seeding", "error": "x"});
        assert_eq!(
            failure(reqwest::StatusCode::SERVICE_UNAVAILABLE, &seeding),
            ("store-seeding".to_string(), envelope::EXIT_CONFLICT)
        );
        let nf = serde_json::json!({"type": "urn:kb:errors:not-found"});
        assert_eq!(
            failure(reqwest::StatusCode::NOT_FOUND, &nf).1,
            envelope::EXIT_NOT_FOUND
        );
        let amb = serde_json::json!({"code": "base-url-ambiguous"});
        assert_eq!(
            failure(reqwest::StatusCode::CONFLICT, &amb),
            ("base-url-ambiguous".to_string(), envelope::EXIT_CONFLICT)
        );
        assert_eq!(
            failure(reqwest::StatusCode::INTERNAL_SERVER_ERROR, &Value::Null).0,
            "http-500"
        );
    }
}
