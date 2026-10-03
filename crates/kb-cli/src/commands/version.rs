//! `kb version` — name this binary's identity, and the hook contract it
//! implements (v0.44 F8).
//!
//! Hooks run straight from the repo (a marketplace `directory` source), the
//! daemon is redeployed from images, and the host `kb` is whatever was last
//! copied in. A hook feature that needs newer CLI behaviour then does
//! nothing, silently. Two cheap, deterministic facts name that skew:
//!
//!   * the binary's own build stamp (`kb_buildstamp`: git describe + sha),
//!     which `kb doctor --hooks`'s `cli-skew` check compares with the
//!     daemon's `/api/identity` `build_sha`;
//!   * [`HOOK_CONTRACT`], an integer the CLI prints for `kb version
//!     --contract`. The hooks (`kb-wake.sh`) declare the contract they were
//!     written against; a binary that prints less, or does not know the
//!     flag at all (an old binary fails the call), is older than the hooks.
//!
//! BUMP [`HOOK_CONTRACT`] — and `KB_HOOK_CONTRACT` in
//! `plugins/kb-memory/hooks/kb-wake.sh`, in the same commit — whenever a
//! hook starts depending on CLI behaviour an older binary lacks. A unit
//! test pins the two numbers together. Detection and naming only: nothing
//! here updates anything.

use anyhow::Result;

/// The hook contract this binary implements. See the module docs.
pub const HOOK_CONTRACT: u32 = 1;

/// The stamp as printed by `kb version`: `(describe, sha)`.
pub fn stamp() -> (&'static str, &'static str) {
    (kb_buildstamp::VERSION, kb_buildstamp::BUILD_SHA)
}

/// True when the binary carries no usable build stamp (built outside git,
/// or with the stamp overridden away). Reported, never hidden.
pub fn stamp_missing(describe: &str, sha: &str) -> bool {
    sha.is_empty() || sha == "unknown" || describe == "0.0.0-dev"
}

fn render(contract_only: bool, describe: &str, sha: &str) -> String {
    if contract_only {
        return format!("{HOOK_CONTRACT}\n");
    }
    let missing = if stamp_missing(describe, sha) {
        " (stamp missing)"
    } else {
        ""
    };
    format!("kb {describe} build {sha} hook-contract {HOOK_CONTRACT}{missing}\n")
}

pub fn run(contract: bool, json: bool) -> Result<()> {
    let (describe, sha) = stamp();
    if json && !contract {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "version": describe,
                "build_sha": sha,
                "hook_contract": HOOK_CONTRACT,
                "stamp_missing": stamp_missing(describe, sha),
            }))?
        );
    } else {
        print!("{}", render(contract, describe, sha));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_is_one_bare_integer() {
        assert_eq!(render(true, "0.43-1-gabc", "abc"), "1\n");
    }

    #[test]
    fn a_missing_stamp_is_named_not_hidden() {
        let line = render(false, "0.0.0-dev", "unknown");
        assert!(line.contains("stamp missing"), "{line}");
        assert!(stamp_missing("0.0.0-dev", "unknown"));
        assert!(!stamp_missing("0.43-1-gabc", "abcdef123456"));
        let ok = render(false, "0.43-1-gabc", "abcdef123456");
        assert!(!ok.contains("stamp missing"), "{ok}");
    }

    /// The number the hook declares and the number this binary prints are
    /// one contract. If a hook starts needing new CLI behaviour, both move.
    #[test]
    fn kb_wake_declares_the_contract_this_binary_implements() {
        let wake = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../plugins/kb-memory/hooks/kb-wake.sh"),
        )
        .unwrap();
        let declared: u32 = wake
            .lines()
            .find_map(|l| l.strip_prefix("KB_HOOK_CONTRACT="))
            .expect("kb-wake.sh must declare KB_HOOK_CONTRACT=<n> at column 0")
            .trim()
            .parse()
            .expect("KB_HOOK_CONTRACT must be an integer");
        assert_eq!(declared, HOOK_CONTRACT);
    }
}
