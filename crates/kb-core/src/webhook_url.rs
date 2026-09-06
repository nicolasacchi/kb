//! Outbound webhook URL policy (SSRF guard for `[webhooks]`).
//!
//! The event→webhook bridge POSTs operator-configured URLs. Without a
//! host policy, a compromised or mistaken `kb.toml` / `PUT /api/config`
//! can aim the daemon at cloud metadata, link-local, or LAN services
//! (operator-controlled SSRF).
//!
//! Default (`allow_private = false`): only **loopback** and **public
//! unicast** destinations. RFC1918 / ULA / CGNAT / special ranges fail.
//!
//! `allow_private = true`: also permit RFC1918 + ULA (LAN hooks).
//! **Always refused** (even with `allow_private`): link-local, cloud
//! metadata endpoints (incl. IPv4-mapped forms), multicast, unspecified.
//!
//! DNS rebinding: callers must dial only the IPs returned by
//! [`prepare_webhook_dial`] (e.g. `reqwest::ClientBuilder::resolve_to_addrs`).
//! Re-validating the hostname alone does **not** pin the connect path.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

/// Resolved, policy-filtered dial plan for one webhook POST.
#[derive(Debug, Clone)]
pub struct WebhookDial {
    /// Normalized request URL — the same `reqwest::Url` parse the client
    /// dials, so its re-parsed host equals `domain` and the pin binds.
    pub url: String,
    /// Host label for `Host` / TLS SNI / `resolve_to_addrs` (no brackets).
    pub domain: String,
    /// Effective port (80/443 defaulted from scheme when omitted).
    pub port: u16,
    /// Addresses that passed policy — dial **only** these.
    pub addrs: Vec<SocketAddr>,
}

/// Validate a webhook destination's *syntax* and any IP-literal policy —
/// **no DNS**, so it is safe on the synchronous config / `validate()` path
/// (`PUT /api/config`). Parsed with `reqwest::Url` (the client's own
/// parser), so the checked host is exactly the authority reqwest connects
/// to. For a hostname, DNS resolution + the connect-IP pin are deferred to
/// [`prepare_webhook_dial`], which the bridge runs off the runtime.
pub fn validate_webhook_url(raw: &str, allow_private: bool) -> Result<(), String> {
    let parts = parse_webhook_url(raw)?;
    if let Ok(ip) = parts.domain.parse::<IpAddr>() {
        return check_ip(ip, allow_private, raw.trim());
    }
    Ok(())
}

/// Resolve + filter to the set of socket addresses the bridge may dial.
///
/// Prefer this over bare validate when issuing HTTP: pin the client with
/// `resolve_to_addrs(&dial.domain, &dial.addrs)` so connect cannot re-resolve
/// to a blocked IP (TOCTOU rebinding).
pub fn prepare_webhook_dial(raw: &str, allow_private: bool) -> Result<WebhookDial, String> {
    let WebhookUrlParts {
        normalized,
        domain,
        port,
    } = parse_webhook_url(raw)?;

    let ips: Vec<IpAddr> = if let Ok(ip) = domain.parse::<IpAddr>() {
        vec![ip]
    } else {
        let addrs = match (domain.as_str(), port).to_socket_addrs() {
            Ok(iter) => iter.collect::<Vec<_>>(),
            Err(e) => {
                return Err(format!("webhook url host `{domain}` does not resolve: {e}"));
            }
        };
        if addrs.is_empty() {
            return Err(format!(
                "webhook url host `{domain}` resolves to no addresses"
            ));
        }
        // Dedup while preserving order.
        let mut seen = std::collections::HashSet::new();
        let mut ips = Vec::new();
        for a in addrs {
            if seen.insert(a.ip()) {
                ips.push(a.ip());
            }
        }
        ips
    };

    let mut allowed = Vec::new();
    for ip in ips {
        check_ip(ip, allow_private, &normalized)?;
        allowed.push(SocketAddr::new(ip, port));
    }
    if allowed.is_empty() {
        return Err(format!("`{normalized}` has no allowed dial addresses"));
    }

    Ok(WebhookDial {
        // The NORMALIZED url (same `reqwest::Url` parse) so the client's
        // re-parse of `dial.url` yields exactly `dial.domain`, and the
        // `resolve_to_addrs(&dial.domain, …)` pin actually binds.
        url: normalized,
        domain,
        port,
        addrs: allowed,
    })
}

fn check_ip(ip: IpAddr, allow_private: bool, raw: &str) -> Result<(), String> {
    let ip = canonical_ip(ip);
    if is_loopback(ip) {
        return Ok(());
    }
    // Always refuse metadata / link-local / non-dialable specials — even
    // when allow_private unlocks LAN (RFC1918 / ULA).
    if is_always_denied(ip) {
        return Err(format!(
            "`{raw}` targets blocked address {ip} (link-local, metadata, or non-unicast); refused even with allow_private"
        ));
    }
    if allow_private {
        if is_lan_private(ip) {
            return Ok(());
        }
        // Public unicast still OK with allow_private.
        if !is_blocked_non_loopback(ip) {
            return Ok(());
        }
        return Err(format!(
            "`{raw}` targets non-LAN address {ip} under allow_private policy"
        ));
    }
    if is_blocked_non_loopback(ip) {
        return Err(format!(
            "`{raw}` targets non-public address {ip}; set [webhooks] allow_private = true to permit LAN receivers (loopback is always allowed)"
        ));
    }
    Ok(())
}

/// Map IPv4-mapped IPv6 to V4 so metadata/LL rules apply uniformly.
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        other => other,
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Never dialable as a webhook target, regardless of `allow_private`.
fn is_always_denied(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast() {
                return true;
            }
            // Entire link-local 169.254.0.0/16 (covers AWS/GCP/Azure IMDS).
            if v4.is_link_local() {
                return true;
            }
            // Known non-LL cloud metadata endpoints.
            let o = v4.octets();
            // Alibaba metadata
            if o == [100, 100, 100, 200] {
                return true;
            }
            false
        }
        IpAddr::V6(v6) => {
            if v6.is_unspecified() || v6.is_multicast() {
                return true;
            }
            let s = v6.segments();
            // Link-local fe80::/10
            if (s[0] & 0xffc0) == 0xfe80 {
                return true;
            }
            // AWS IPv6 IMDS fd00:ec2::/32 (fd00:0ec2:…)
            if s[0] == 0xfd00 && s[1] == 0x0ec2 {
                return true;
            }
            // Site-local deprecated fec0::/10
            if (s[0] & 0xffc0) == 0xfec0 {
                return true;
            }
            false
        }
    }
}

/// RFC1918 (v4) or unique-local (v6) — the intended `allow_private` surface.
fn is_lan_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            (s[0] & 0xfe00) == 0xfc00 && !is_always_denied(IpAddr::V6(v6))
        }
    }
}

/// True for addresses we refuse when `allow_private` is false (non-public
/// except loopback, which is handled separately).
fn is_blocked_non_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(v4: Ipv4Addr) -> bool {
    if is_always_denied(IpAddr::V4(v4)) {
        return true;
    }
    if v4.is_private() {
        return true;
    }
    let o = v4.octets();
    // CGNAT 100.64.0.0/10
    if o[0] == 100 && (o[1] & 0xc0) == 64 {
        return true;
    }
    // Benchmarking 198.18.0.0/15
    if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
        return true;
    }
    // Documentation 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24
    if (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
    {
        return true;
    }
    // 0.0.0.0/8
    if o[0] == 0 {
        return true;
    }
    // Class E / reserved 240.0.0.0/4
    if o[0] >= 240 {
        return true;
    }
    false
}

fn is_blocked_v6(v6: Ipv6Addr) -> bool {
    if is_always_denied(IpAddr::V6(v6)) {
        return true;
    }
    // Unique-local fc00::/7
    let s = v6.segments();
    if (s[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // IPv4-mapped: apply v4 rules (canonical_ip already applied in check_ip,
    // but keep for direct callers of is_blocked_v6).
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_blocked_v4(v4);
    }
    // 6to4 2002::/16 — embed IPv4 in bits 16..48
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new(
            (s[1] >> 8) as u8,
            (s[1] & 0xff) as u8,
            (s[2] >> 8) as u8,
            (s[2] & 0xff) as u8,
        );
        return is_blocked_v4(v4);
    }
    false
}

/// URL parts the webhook policy needs, parsed with `reqwest::Url` — the
/// exact parser the dial client uses.
struct WebhookUrlParts {
    /// Normalized URL string (host lower-cased, default port applied) —
    /// what `client.post(&url)` re-parses, so the pin key matches the
    /// connect host.
    normalized: String,
    /// Host label (IPv6 brackets stripped) for policy + `resolve_to_addrs`.
    domain: String,
    /// Effective port (scheme default when omitted).
    port: u16,
}

/// Parse `raw` with `reqwest::Url`, enforcing http(s) + a non-empty host.
/// Using the same parser as the connect client closes parser-differential
/// SSRF: `http://10.0.0.1/@public` has host `10.0.0.1` (the `@public` is
/// in the path), so the policy — and the `resolve_to_addrs` pin — can never
/// be keyed on a host the client won't actually dial.
fn parse_webhook_url(raw: &str) -> Result<WebhookUrlParts, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("webhook url is empty".into());
    }
    let url = reqwest::Url::parse(raw).map_err(|e| format!("`{raw}` is not a valid URL: {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("`{raw}` is not an http(s) URL (scheme `{other}`)")),
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| format!("`{raw}` has no host"))?;
    // `host_str()` keeps brackets on IPv6 literals; strip for the IpAddr
    // parse and the `resolve_to_addrs` label (hostnames have no brackets).
    let domain = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("`{raw}` has no port"))?;
    Ok(WebhookUrlParts {
        normalized: url.to_string(),
        domain,
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_loopback_http() {
        assert!(validate_webhook_url("http://127.0.0.1:9000/kb-hook", false).is_ok());
        assert!(validate_webhook_url("http://[::1]:9000/h", false).is_ok());
        let d = prepare_webhook_dial("http://127.0.0.1:9000/h", false).unwrap();
        assert_eq!(d.domain, "127.0.0.1");
        assert_eq!(d.port, 9000);
        assert_eq!(d.addrs.len(), 1);
    }

    #[test]
    fn denies_metadata_and_rfc1918_by_default() {
        for u in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/hook",
            "http://192.168.1.1/hook",
            "http://172.16.5.5/hook",
            "http://100.64.0.1/hook",
            "http://240.0.0.1/hook",
        ] {
            let err = validate_webhook_url(u, false).unwrap_err();
            assert!(
                err.contains("non-public") || err.contains("blocked") || err.contains("metadata"),
                "url={u} err={err}"
            );
        }
    }

    #[test]
    fn allow_private_unlocks_lan_but_not_link_local_or_metadata() {
        assert!(validate_webhook_url("http://10.0.0.1/hook", true).is_ok());
        assert!(validate_webhook_url("http://192.168.0.5/h", true).is_ok());
        // Classic IMDS
        let err = validate_webhook_url("http://169.254.169.254/", true).unwrap_err();
        assert!(err.contains("blocked") || err.contains("metadata"), "{err}");
        // Other link-local
        let err = validate_webhook_url("http://169.254.0.23/", true).unwrap_err();
        assert!(err.contains("blocked"), "{err}");
        // IPv4-mapped IMDS
        let err = validate_webhook_url("http://[::ffff:169.254.169.254]/", true).unwrap_err();
        assert!(err.contains("blocked"), "{err}");
        // AWS IPv6 IMDS
        let err = validate_webhook_url("http://[fd00:ec2::254]/", true).unwrap_err();
        assert!(err.contains("blocked"), "{err}");
        // Alibaba
        let err = validate_webhook_url("http://100.100.100.200/", true).unwrap_err();
        assert!(err.contains("blocked"), "{err}");
    }

    #[test]
    fn rejects_non_http_scheme() {
        let err = validate_webhook_url("ftp://example.com/x", false).unwrap_err();
        assert!(err.contains("http(s)"), "{err}");
    }

    #[test]
    fn allows_public_literal() {
        assert!(validate_webhook_url("https://1.1.1.1/hook", false).is_ok());
        let d = prepare_webhook_dial("https://1.1.1.1/hook", false).unwrap();
        assert_eq!(d.port, 443);
    }

    #[test]
    fn at_sign_in_path_is_not_userinfo_ssrf() {
        // The real host is `10.0.0.1` — `@1.1.1.1` is in the PATH. A
        // hand-rolled `@` split would read `1.1.1.1` (public) and, worse,
        // key the dial pin on it while the client connects to `10.0.0.1`.
        // `reqwest::Url` agrees with the client, so this is blocked.
        let err = validate_webhook_url("http://10.0.0.1/@1.1.1.1", false).unwrap_err();
        assert!(
            err.contains("non-public") || err.contains("blocked"),
            "{err}"
        );
        assert!(prepare_webhook_dial("http://10.0.0.1/@1.1.1.1", false).is_err());
    }

    #[test]
    fn dial_url_and_domain_agree_with_reqwest_so_the_pin_binds() {
        // For a public host the dial plan's `url` must re-parse (via the
        // client's own `reqwest::Url`) to exactly `domain` — otherwise
        // `resolve_to_addrs(&domain, …)` would key on a host reqwest never
        // looks up and the connect would silently re-resolve.
        let d = prepare_webhook_dial("http://1.1.1.1/@10.0.0.1", false).unwrap();
        assert_eq!(d.domain, "1.1.1.1");
        assert_eq!(
            reqwest::Url::parse(&d.url).unwrap().host_str(),
            Some(d.domain.as_str())
        );
    }

    #[test]
    fn validate_defers_hostname_dns_but_gates_ip_literals() {
        // Sync validate never resolves DNS: a bare hostname passes (the
        // dial-time path resolves + pins). IP-literal policy still applies.
        assert!(validate_webhook_url("http://receiver.example/hook", false).is_ok());
        let err = validate_webhook_url("http://10.0.0.1/hook", false).unwrap_err();
        assert!(
            err.contains("non-public") || err.contains("blocked"),
            "{err}"
        );
    }

    #[test]
    fn default_ports_from_scheme() {
        let d = prepare_webhook_dial("http://127.0.0.1/h", false).unwrap();
        assert_eq!(d.port, 80);
        let d = prepare_webhook_dial("https://127.0.0.1/h", false).unwrap();
        assert_eq!(d.port, 443);
    }
}
