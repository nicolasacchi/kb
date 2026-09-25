//! RS-U3 — `store_key`: the normalized forge-project identity a review
//! store is keyed by (README §5.1).
//!
//! `store_key` = `host[:port]/path` with
//!
//! * the host lower-cased,
//! * any userinfo dropped (it never reaches the key, the DB, or a log),
//! * the scheme's DEFAULT port dropped (`https` 443, `http` 80, `ssh` 22,
//!   `git` 9418) so the ssh and https forms of one project unify; a
//!   non-default port is kept (a Bitbucket Server `:7999` ssh remote and
//!   its `:7990` https twin are NOT unified — nothing here can know they
//!   are the same project, and a guess would merge two stores),
//! * leading/trailing `/`, a trailing `.git` and repeated `/` removed,
//! * the PATH case preserved — except on `github.com`, whose owner/name
//!   are case-insensitive, so `Acme/Widgets` and `acme/widgets` are one
//!   project there.
//! * any percent-encoding REFUSED (`%` is not a path character): git
//!   transmits a remote URL's path UNDECODED while an HTTP forge may
//!   decode it, so `acme/..%2F..%2Fwidgets/secret` and `widgets/secret`
//!   can be the same project under two keys — and the decoded reading is
//!   a traversal out of the project the URL literally names. A URL
//!   carrying a percent escape has no store key at all.
//!
//! Accepted forms: `https://`, `http://`, `ssh://`, `git://`,
//! `git+ssh://`/`ssh+git://` URLs and scp form (`[user@]host:path`). A
//! local path or `file://` URL is not a forge project and yields `None` —
//! a repo with only local remotes gets a `local:<uuid>` store of its own
//! ([`local_store_key`]).
//!
//! Pure and allocation-light: no network, no git, safe to call anywhere.

/// `local:<uuid>` — the key of a store for a repo with no forge remote.
///
/// README §5.1 names this `local:<repo uuid>`; `repos` has no uuid column
/// (only an integer id, which is reusable after a DB restore — the same
/// reason the store directory is named by a uuid, design §3.1), so the
/// store's OWN uuid is used. It is minted once, at registration, and is
/// what the manifest carries, so it is exactly as stable as the store.
pub fn local_store_key(store_uuid: &str) -> String {
    format!("{LOCAL_PREFIX}{store_uuid}")
}

/// The `local:` prefix.
pub const LOCAL_PREFIX: &str = "local:";

/// Is `key` a `local:` (no-forge) store key?
pub fn is_local_key(key: &str) -> bool {
    key.starts_with(LOCAL_PREFIX)
}

/// Normalize a remote URL (as written in a clone's `remote.<name>.url`, or
/// given by an operator) to its `store_key`. `None` for local paths,
/// `file://`, transport-helper forms (`ext::…`), and anything malformed.
pub fn store_key_for_url(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    if s.contains("::") {
        return None; // `<transport>::<address>` remote-helper form
    }
    let (host, port, path) = if let Some((scheme, rest)) = s.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "https" => "443",
            "http" => "80",
            "ssh" | "git+ssh" | "ssh+git" => "22",
            "git" => "9418",
            _ => return None, // file://, ext, unknown
        };
        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a, p),
            None => return None,
        };
        // Drop userinfo — rightmost `@` (a password may itself contain one).
        let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = split_port(hostport)?;
        let port = port.filter(|p| *p != default_port);
        (host, port, path)
    } else {
        // scp form: `[user@]host:path`. git's own rule: it is scp form only
        // when a `:` comes before any `/`; a leading `/` or `.` is a path.
        if s.starts_with('/') || s.starts_with('.') || s.starts_with('~') {
            return None;
        }
        let colon = s.find(':')?;
        if s[..colon].contains('/') {
            return None;
        }
        let userhost = &s[..colon];
        let host = userhost.rsplit_once('@').map_or(userhost, |(_, h)| h);
        let path = &s[colon + 1..];
        (host, None, path)
    };
    let host = host.to_ascii_lowercase();
    if !host_ok(&host) {
        return None;
    }
    let path = normalize_path(path)?;
    let path = if host == "github.com" {
        path.to_ascii_lowercase()
    } else {
        path
    };
    Some(match port {
        Some(p) => format!("{host}:{p}/{path}"),
        None => format!("{host}/{path}"),
    })
}

/// `(host, owner/name path)` of a forge key; `None` for `local:` keys.
pub fn split_key(key: &str) -> Option<(&str, &str)> {
    if is_local_key(key) {
        return None;
    }
    key.split_once('/')
}

/// The HTTPS transport URL for a forge key (README §8: "the store's
/// transport URL is the HTTPS form, derived from the canonical slug").
/// `None` for `local:` keys and for keys carrying a non-default port — the
/// port may belong to ssh, and an https guess on it would be wrong.
///
/// A key can never carry a percent escape ([`store_key_for_url`] refuses
/// `%`), so re-emitting the path verbatim here cannot turn one project's
/// key into another project's URL.
pub fn https_url_for_key(key: &str) -> Option<String> {
    let (host, path) = split_key(key)?;
    if host.contains(':') {
        return None;
    }
    Some(format!("https://{host}/{path}.git"))
}

/// Does forge key `key` name the `owner/name` slug `slug` (the
/// `reviews.pr_repo_slug` shape, host-less)? Case-insensitive on
/// `github.com`, exact elsewhere — the same rule the key itself uses.
pub fn key_matches_slug(key: &str, slug: &str) -> bool {
    let Some((host, path)) = split_key(key) else {
        return false;
    };
    let slug = slug.trim().trim_matches('/');
    let slug = slug.strip_suffix(".git").unwrap_or(slug);
    if host == "github.com" {
        path.eq_ignore_ascii_case(slug)
    } else {
        path == slug
    }
}

fn split_port(hostport: &str) -> Option<(&str, Option<&str>)> {
    // IPv6 literals need brackets; none of the forges this store supports
    // use them, and the URL allowlist (`url.rs`) refuses them too.
    if hostport.starts_with('[') {
        return None;
    }
    match hostport.split_once(':') {
        Some((h, p)) => {
            if p.is_empty() {
                return Some((h, None));
            }
            if p.len() > 5 || !p.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            Some((h, Some(p)))
        }
        None => Some((hostport, None)),
    }
}

fn host_ok(h: &str) -> bool {
    let b = h.as_bytes();
    !b.is_empty()
        && b.len() <= 253
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'.' || *c == b'-')
        && !h.ends_with('.')
        && !h.contains("..")
}

fn normalize_path(p: &str) -> Option<String> {
    let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() || segs.iter().any(|s| *s == ".." || *s == ".") {
        return None;
    }
    let mut out = segs.join("/");
    if let Some(stripped) = out.strip_suffix(".git") {
        out = stripped.trim_end_matches('/').to_string();
    }
    if out.is_empty() {
        return None;
    }
    // `%` is deliberately NOT a path character. A `..%2F..%2F` segment
    // passes the `..` check above — it is ONE segment, not two — while
    // the fetch, which git transmits undecoded and an HTTP forge may
    // decode, resolves to a different project than the key names: the
    // store would hold `widgets/secret`'s objects under a key naming
    // `acme/..%2F..%2Fwidgets/secret`, and the two distinct keys
    // `…/acme/..%2F..%2Fwidgets/secret` and `…/widgets/secret` would
    // address one project. Decoding here cannot repair that (which
    // reading the FORGE honours is its own business), so the only rule
    // that keeps the derivation injective is to refuse the escape.
    let ok = out
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'/' | b'~' | b'+'));
    ok.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Option<String> {
        store_key_for_url(s)
    }

    #[test]
    fn https_ssh_and_scp_forms_unify() {
        let want = Some("github.com/acme/widgets".to_string());
        for form in [
            "https://github.com/acme/widgets.git",
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets/",
            "https://github.com/acme/widgets.git/",
            "https://GitHub.COM/acme/widgets.git",
            "https://github.com:443/acme/widgets.git",
            "http://github.com/acme/widgets",
            "ssh://git@github.com/acme/widgets.git",
            "ssh://git@github.com:22/acme/widgets.git",
            "git+ssh://git@github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
            "git@github.com:/acme/widgets.git",
            "github.com:acme/widgets",
            "git://github.com/acme/widgets.git",
            "  https://github.com/acme/widgets.git  ",
            "https://github.com//acme//widgets.git",
        ] {
            assert_eq!(k(form), want, "{form}");
        }
    }

    #[test]
    fn userinfo_never_reaches_the_key() {
        for form in [
            "https://x-access-token:ghp_FAKE0000@github.com/acme/widgets.git",
            "https://user@github.com/acme/widgets.git",
            "https://us@er:p@ss@github.com/acme/widgets.git",
            "deploy@github.com:acme/widgets.git",
        ] {
            let key = k(form).unwrap();
            assert_eq!(key, "github.com/acme/widgets", "{form}");
            assert!(!key.contains("ghp_") && !key.contains('@'));
        }
    }

    #[test]
    fn github_paths_fold_case_other_forges_keep_it() {
        assert_eq!(
            k("https://github.com/Acme/Widgets.git").unwrap(),
            "github.com/acme/widgets"
        );
        assert_eq!(
            k("https://gitlab.example.com/Acme/Widgets.git").unwrap(),
            "gitlab.example.com/Acme/Widgets"
        );
    }

    #[test]
    fn non_default_ports_are_kept_and_not_unified() {
        assert_eq!(
            k("ssh://git@bitbucket.example.com:7999/acme/widgets.git").unwrap(),
            "bitbucket.example.com:7999/acme/widgets"
        );
        assert_eq!(
            k("https://bitbucket.example.com:7990/acme/widgets.git").unwrap(),
            "bitbucket.example.com:7990/acme/widgets"
        );
        assert_eq!(k("https://h.example:/a/b").unwrap(), "h.example/a/b");
        assert_eq!(k("https://h.example:99999999/a/b"), None);
    }

    #[test]
    fn nested_groups_are_kept_whole() {
        assert_eq!(
            k("git@gitlab.com:acme/platform/widgets.git").unwrap(),
            "gitlab.com/acme/platform/widgets"
        );
    }

    #[test]
    fn local_and_hostile_forms_have_no_key() {
        for form in [
            "",
            "   ",
            "/srv/git/widgets.git",
            "./widgets",
            "../widgets",
            "~/widgets",
            "file:///srv/git/widgets.git",
            "ext::sh -c touch% /tmp/pwned",
            "fd::17",
            "https://github.com",
            "https://github.com/",
            "https://github.com/.git",
            "https://github.com/acme/../widgets",
            "https://[::1]/acme/widgets",
            "https://bad_host/acme/widgets",
            "https://github.com/acme/wid gets",
            "https://github.com/acme/wid\ngets",
            "svn://example.com/acme/widgets",
            "dir/with:colon",
        ] {
            assert_eq!(k(form), None, "{form:?}");
        }
    }

    #[test]
    fn https_url_and_slug_helpers() {
        assert_eq!(
            https_url_for_key("github.com/acme/widgets").unwrap(),
            "https://github.com/acme/widgets.git"
        );
        assert_eq!(https_url_for_key("h.example:7999/acme/widgets"), None);
        assert_eq!(https_url_for_key(&local_store_key("u-1")), None);
        assert!(key_matches_slug("github.com/acme/widgets", "Acme/Widgets"));
        assert!(key_matches_slug(
            "github.com/acme/widgets",
            "acme/widgets.git"
        ));
        assert!(!key_matches_slug("gitlab.com/acme/widgets", "Acme/Widgets"));
        assert!(!key_matches_slug("github.com/acme/widgets", "acme/gadgets"));
        assert!(!key_matches_slug(&local_store_key("x"), "acme/widgets"));
        assert!(is_local_key(&local_store_key("x")));
        assert_eq!(
            split_key("github.com/acme/widgets"),
            Some(("github.com", "acme/widgets"))
        );
    }
}
