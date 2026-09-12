//! `kb token {generate, rotate, show, path, issue, revoke}` — bearer-token
//! lifecycle for v0.4 self-host + v0.34 multi-user registry.
//!
//! - **Legacy shared token** at `<XDG_CONFIG_HOME>/kb/token` (`generate` /
//!   `rotate` / `show` / `path`). The daemon reads it once at startup
//!   (loopback bypasses auth so local dev is unaffected); rotation
//!   requires a daemon restart.
//! - **Per-user registry** at `<XDG_CONFIG_HOME>/kb/tokens` (`issue` /
//!   `revoke`). Lines are `<user>:sha256:<hex-of-token>` (preferred) or
//!   `<user>:<plaintext>`. Same restart-to-load discipline as the legacy
//!   file.
//!
//! Production deployment guide: docs/self-host.md.

use anyhow::{anyhow, Context, Result};
use kb_core::identity::username_is_valid;
use kb_core::paths::KbPaths;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const TOKEN_BYTES: usize = 32;

/// Resolve the token file path the daemon will read at startup. Same
/// resolver as `KbPaths::new("default").token_file()`; centralised
/// here so the CLI doesn't import the daemon name elsewhere.
pub fn token_path() -> Result<PathBuf> {
    let paths = KbPaths::new("default").context("XDG paths")?;
    Ok(paths.token_file())
}

/// Resolve the multi-user token registry path (`<config>/tokens`).
pub fn tokens_registry_path() -> Result<PathBuf> {
    let paths = KbPaths::new("default").context("XDG paths")?;
    Ok(paths.tokens_file())
}

/// Generate a fresh 32-byte token, write to `~/.config/kb/token` mode
/// 0600, REFUSE if the file already exists. Use `rotate` to overwrite
/// intentionally.
pub fn generate() -> Result<()> {
    let path = token_path()?;
    if path.exists() {
        return Err(anyhow!(
            "token file already exists at {}. Use `kb token rotate` to overwrite.",
            path.display()
        ));
    }
    write_token(&path, &random_token()?)?;
    eprintln!("✓ wrote {}", path.display());
    Ok(())
}

/// Same as `generate` but overwrites any existing token. Daemon picks
/// up the new value on next restart.
pub fn rotate() -> Result<()> {
    let path = token_path()?;
    write_token(&path, &random_token()?)?;
    eprintln!("✓ rotated {}", path.display());
    eprintln!("  restart the daemon to pick up the new value");
    Ok(())
}

/// Print the token. Defaults to stderr; `--print` flag echoes to
/// stdout for shell capture (`TOKEN=$(kb token show --print)`).
///
/// **Note on shell history:** stderr-vs-stdout doesn't affect whether
/// the *command line* lands in shell history — `bash`/`zsh` log
/// invocations, not output streams. The stderr default is only useful
/// for one thing: a typo'd `kb token show` that the user pipes into
/// `grep`/`less`/`tee` won't pass the token through the pipe (stdout
/// stays empty). If you don't want `kb token show` in history at all,
/// prefix with a space (`HISTCONTROL=ignorespace`) or use `kb token
/// path` + `cat` instead. Deep-review LOW.
pub fn show(print: bool) -> Result<()> {
    let path = token_path()?;
    let token = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(anyhow!("{} is empty", path.display()));
    }
    if print {
        println!("{token}");
    } else {
        eprintln!("{token}");
    }
    Ok(())
}

/// Print the resolved file path.
pub fn path() -> Result<()> {
    let path = token_path()?;
    println!("{}", path.display());
    Ok(())
}

/// `kb token issue <user> [--force]` — mint a 32-byte token for `user`,
/// store `<user>:sha256:<hex>` in the daemon-side registry, print the
/// PLAINTEXT once. Refuses a duplicate user line unless `--force`
/// (replaces every existing line for that user).
pub fn issue(user: &str, force: bool) -> Result<()> {
    if !username_is_valid(user) {
        return Err(anyhow!(
            "invalid username {user:?}: must match ^[a-z0-9._@-]{{1,64}}$ \
             (lowercase only — case forks in append-only history are permanent)"
        ));
    }
    let path = tokens_registry_path()?;
    let existing = read_registry_body(&path)?;
    let has_user = registry_has_user(&existing, user);
    if has_user && !force {
        return Err(anyhow!(
            "user {user:?} already has a registry entry in {}. \
             Pass --force to replace it.",
            path.display()
        ));
    }

    let plaintext = random_token()?;
    let line = format_registry_line(user, &plaintext);
    let new_body = if has_user {
        // --force: drop every line for this user, then append the fresh one.
        let mut kept = filter_registry_lines(&existing, user, /*drop_user=*/ true);
        if !kept.is_empty() && !kept.ends_with('\n') {
            kept.push('\n');
        }
        kept.push_str(&line);
        kept.push('\n');
        kept
    } else if existing.is_empty() {
        format!("{line}\n")
    } else {
        let mut body = existing;
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&line);
        body.push('\n');
        body
    };

    write_token(&path, new_body.trim_end())?;
    // Print plaintext ONCE (stdout so shell capture works):
    //   TOKEN=$(kb token issue alice | head -1)
    println!("{plaintext}");
    eprintln!("✓ issued token for {user} → {}", path.display());
    eprintln!("  restart the daemon to load the new registry entry");
    Ok(())
}

/// `kb token revoke <user>` — remove every registry line for `user`;
/// atomic rewrite; report how many lines were dropped.
pub fn revoke(user: &str) -> Result<()> {
    if !username_is_valid(user) {
        return Err(anyhow!(
            "invalid username {user:?}: must match ^[a-z0-9._@-]{{1,64}}$"
        ));
    }
    let path = tokens_registry_path()?;
    let existing = read_registry_body(&path)?;
    if existing.is_empty() && !path.exists() {
        eprintln!("no registry at {} — nothing to revoke", path.display());
        return Ok(());
    }
    let (new_body, removed) = remove_user_lines(&existing, user);
    if removed == 0 {
        eprintln!("no registry entries for {user:?} in {}", path.display());
        return Ok(());
    }
    if new_body.trim().is_empty() {
        // Empty registry: write an empty file (or remove). Prefer empty
        // file so a subsequent issue has a known path; mode 0600 still.
        write_token(&path, "")?;
    } else {
        write_token(&path, new_body.trim_end())?;
    }
    eprintln!(
        "✓ revoked {removed} entr{} for {user} in {}",
        if removed == 1 { "y" } else { "ies" },
        path.display()
    );
    eprintln!("  restart the daemon to drop the token");
    Ok(())
}

/// Format a preferred hashed registry line: `<user>:sha256:<hex>`.
/// Pure — unit-tested.
pub fn format_registry_line(user: &str, plaintext_token: &str) -> String {
    format!("{user}:sha256:{}", hash_token_hex(plaintext_token))
}

/// SHA-256 hex of the presented bearer (matches the daemon's compare).
pub fn hash_token_hex(plaintext: &str) -> String {
    let mut h = Sha256::new();
    h.update(plaintext.as_bytes());
    hex::encode(h.finalize())
}

/// True if any non-comment registry line starts with `<user>:`.
fn registry_has_user(body: &str, user: &str) -> bool {
    body.lines()
        .any(|line| line_user(line).as_deref() == Some(user))
}

/// Drop (or keep) lines belonging to `user`. When `drop_user` is true,
/// returns the body with those lines removed (comments/blanks kept).
fn filter_registry_lines(body: &str, user: &str, drop_user: bool) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let is_user = line_user(line).as_deref() == Some(user);
        if drop_user && is_user {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Remove every line for `user`; return `(new_body, removed_count)`.
fn remove_user_lines(body: &str, user: &str) -> (String, usize) {
    let mut out = String::new();
    let mut removed = 0usize;
    for line in body.lines() {
        if line_user(line).as_deref() == Some(user) {
            removed += 1;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    (out, removed)
}

/// Parse the username of a registry line via the DAEMON's own parser
/// (`kb_core::identity::parse_tokens_file`, fed one line) so the CLI's
/// notion of "a live registry line" can never drift from what the daemon
/// loads — a line the daemon skips (bad username, empty secret, comment)
/// parses to None here too.
fn line_user(line: &str) -> Option<String> {
    kb_core::identity::parse_tokens_file(line)
        .into_iter()
        .next()
        .map(|e| e.user)
}

fn read_registry_body(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

fn random_token() -> Result<String> {
    let mut buf = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut buf).context("getrandom")?;
    Ok(hex::encode(buf))
}

fn write_token(path: &std::path::Path, token: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    // Write atomically: tmpfile + rename. Mode 0600 set on the tmpfile
    // before content lands so a concurrent reader can't see plaintext
    // through a more-permissive mode window.
    //
    // LOW (deep-review): pre-fix used a fixed `.tmp` suffix; concurrent
    // `kb token rotate` invocations would race against the same tmpfile
    // and clobber each other. Use a pid + nanos suffix so each rotation
    // gets its own staging file. AND fsync the parent directory after
    // the rename — without it, a kernel crash between rename and the
    // dirent flush could leave the daemon reading the old token. The
    // file itself is already sync_all'd in write_with_mode_0600.
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("token");
    let tmp = parent.join(format!(".{fname}.{pid}.{nanos}.tmp"));
    write_with_mode_0600(&tmp, token)?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    sync_parent_dir(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent_dir(parent: &std::path::Path) -> Result<()> {
    let dir = std::fs::File::open(parent)
        .with_context(|| format!("open parent dir {}", parent.display()))?;
    dir.sync_all()
        .with_context(|| format!("fsync parent dir {}", parent.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_dir(_parent: &std::path::Path) -> Result<()> {
    // Windows doesn't expose directory fsync the same way; the
    // rename(2)/MoveFileEx atomicity story is different (the FAT/NTFS
    // journal handles the rename itself). No-op here.
    Ok(())
}

#[cfg(unix)]
fn write_with_mode_0600(path: &std::path::Path, token: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(token.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_with_mode_0600(path: &std::path::Path, token: &str) -> Result<()> {
    // Windows has no chmod 0600 equivalent; fall back to plain write.
    std::fs::write(path, format!("{token}\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_registry_line_is_sha256_form() {
        let line = format_registry_line("alice", "deadbeef");
        assert!(line.starts_with("alice:sha256:"));
        let hex = line.strip_prefix("alice:sha256:").unwrap();
        assert_eq!(hex, hash_token_hex("deadbeef"));
        // Known SHA-256 of "deadbeef" (ascii).
        assert_eq!(
            hex,
            "2baf1f40105d9501fe319a8ec463fdf4325a2a5df445adf3f572f626253678c9"
        );
    }

    #[test]
    fn line_user_parses_both_forms_skips_comments() {
        assert_eq!(line_user("alice:sha256:abc").as_deref(), Some("alice"));
        assert_eq!(line_user("bob:plaintext").as_deref(), Some("bob"));
        assert_eq!(line_user("# comment"), None);
        assert_eq!(line_user(""), None);
        assert_eq!(line_user("  # c"), None);
        assert_eq!(line_user("  eve : secret ").as_deref(), Some("eve"));
    }

    #[test]
    fn remove_user_lines_counts_and_preserves_others() {
        let body = "\
# comment
alice:sha256:aaa
bob:plain
alice:sha256:bbb
charlie:x
";
        let (new, n) = remove_user_lines(body, "alice");
        assert_eq!(n, 2);
        assert!(!new.contains("alice"));
        assert!(new.contains("bob:plain"));
        assert!(new.contains("charlie:x"));
        assert!(new.contains("# comment"));
    }

    #[test]
    fn registry_has_user_detects_entry() {
        let body = "alice:sha256:aaa\nbob:plain\n";
        assert!(registry_has_user(body, "alice"));
        assert!(registry_has_user(body, "bob"));
        assert!(!registry_has_user(body, "charlie"));
        assert!(!registry_has_user("# only comments\n", "alice"));
    }

    #[test]
    fn username_is_valid_pass_through() {
        // The issue/revoke gates call kb_core::identity::username_is_valid —
        // pin the boundary so a core rule drift surfaces here too.
        assert!(username_is_valid("alice"));
        assert!(username_is_valid("a.b-c_d@e"));
        assert!(!username_is_valid("Alice"));
        assert!(!username_is_valid("has space"));
        assert!(!username_is_valid(""));
    }
}
