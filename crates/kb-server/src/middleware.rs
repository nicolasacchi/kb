//! HTTP-layer middleware: RFC 7807 problem+json error conversion + Origin
//! allowlist for write-CSRF defense + v0.4 bearer-token auth + per-token
//! rate limit. Topic 11 §C, docs/self-host.md.

use crate::state::{AuthConfig, OriginConfig};
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderValue, Method, Request, Response, StatusCode},
    middleware::Next,
    response::IntoResponse,
};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

/// Origin-allowlist middleware (CSRF defense per topic 11 §C). On
/// non-safe methods, if the `Origin` header is present, it must:
///   - match the configured `parent_origin` (default
///     `http://localhost:4000`), OR
///   - end in the configured `artifact_host_suffix` (default
///     `.artifacts.localhost`) — i.e. sandboxed iframe origin, OR
///   - match the request's own `Host:` header — i.e. same-origin
///     requests from the SPA the daemon itself served, OR
///   - be any `localhost` / `127.0.0.1` origin when KB_DEV_ORIGIN_ANY=1.
///
/// Absent Origin = passes (curl, CLI, in-process tests). GETs are not
/// Origin-checked.
pub async fn origin_allowlist(
    State(origin_cfg): State<Arc<OriginConfig>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    if !is_state_changing(req.method()) {
        return next.run(req).await;
    }
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        // Origin absent — browsers send it on every cross-origin POST,
        // so absence means a local process (curl, kb-cli, in-process
        // tests) or a non-browser client. Deep-review M6
        // considered requiring an `X-Requested-By` header here to
        // block local-process CSRF in personal-mode; rejected as the
        // wrong trade-off: the bearer token is the real defense on
        // non-loopback, and a malicious LOCAL process can already do
        // anything else on the user's machine. The cost was every
        // external curl / shell script / e2e test having to learn to
        // set the header. kb-cli DOES set `X-Requested-By: kb-cli`
        // (so a future strict-mode opt-in is one-flag away), but the
        // middleware doesn't yet require it.
        return next.run(req).await;
    };
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    if !origin_allowed(origin, host, &origin_cfg) {
        return forbidden(ERR_ORIGIN_REFUSED, format!("origin {origin} not allowed"));
    }
    next.run(req).await
}

fn is_state_changing(method: &Method) -> bool {
    // LOW (deep-review): the Origin-allowlist gate fires only on
    // state-changing methods. OPTIONS is intentionally EXCLUDED — a
    // preflight from a cross-origin browser fetch is the legitimate
    // OPTIONS traffic, and the response shouldn't depend on the
    // allowlist (CORS is its own dance). Any future handler that
    // mutates state on OPTIONS / HEAD / CONNECT / TRACE must add its
    // own Origin gating; flagging the asymmetry here so the next
    // person to introduce one notices.
    matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    )
}

fn origin_allowed(origin: &str, host: Option<&str>, cfg: &OriginConfig) -> bool {
    if origin == cfg.parent_origin {
        return true;
    }
    if let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    {
        // Same-origin: the SPA the daemon served back to the browser
        // includes Origin = scheme + Host, and Host = our listen addr.
        // If origin's host:port matches the request's Host header, this
        // is by definition not CSRF — accept.
        //
        // v0.7.1 P2 note, REVISED by SEC-02: `Host` is client-controlled,
        // so `Origin == Host` is not a cryptographic guarantee, and the
        // v0.7.1 reasoning ("a genuine cross-site request carries the
        // victim's origin, which won't equal an attacker-chosen Host")
        // is FALSE under DNS rebinding — the rebound page's origin IS
        // the attacker-chosen name, which is exactly what made this arm
        // a hole. The rebinding defence is now `host_guard`, layered
        // OUTSIDE this one on `/api`. What is left here is the
        // same-origin CONVENIENCE: the SPA the daemon itself served
        // POSTs with `Origin` = its own scheme+Host, whatever hostname
        // it was reached on, and must keep working regardless of which
        // one that is. Do not treat this arm as a security boundary.
        if let Some(host) = host {
            if rest == host {
                return true;
            }
        }
        let host_no_port = rest.split(':').next().unwrap_or(rest);
        // *.<artifact_host_suffix> subdomains (production iframe origin
        // per topic 06; configured via `[server] artifact_host_suffix`).
        let suffix = cfg.artifact_host_suffix.as_str();
        if host_no_port.ends_with(suffix) && host_no_port.len() > suffix.len() {
            return true;
        }
        // Dev convenience: when KB_DEV_ORIGIN_ANY=1 is set, accept any
        // localhost / 127.0.0.1 port. Lets the SPA's `npm run dev` server
        // (Vite picks a random port) talk to the daemon during development.
        // NOT enabled in production; documented in the v0.1 plan.
        if std::env::var("KB_DEV_ORIGIN_ANY").as_deref() == Ok("1")
            && (host_no_port == "localhost" || host_no_port == "127.0.0.1")
        {
            return true;
        }
    }
    false
}

/// SW3 — CORS allow-origin predicate for the read-only `/api` surface.
/// True only for loopback origins: `http(s)://localhost`, `127.0.0.1`,
/// or `[::1]`, any port. Lets a browser page served by ONE local daemon
/// read GETs (including the `/api/events` stream) from OTHER local
/// daemons — the multi-daemon fleet case — without opening the API to
/// drive-by reads from arbitrary websites (a non-loopback origin gets
/// no `Access-Control-Allow-Origin` and the browser blocks the read).
/// Response-header-only: it never bypasses the bearer-token auth or the
/// trusted-hop loopback gates (invariants #3/#4), and the write-CSRF
/// Origin allowlist above is untouched (CORS here is GET-only).
pub fn is_loopback_web_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    // Split off the port — bracketed IPv6 first ([::1]:4001), then the
    // plain host:port form.
    let host = if let Some(r) = rest.strip_prefix('[') {
        match r.split_once(']') {
            Some((h, port)) if port.is_empty() || port.starts_with(':') => {
                return h == "::1";
            }
            _ => return false,
        }
    } else {
        rest.split(':').next().unwrap_or(rest)
    };
    host == "localhost" || host == "127.0.0.1"
}

/// `urn:kb:errors:origin-refused` — the Origin-allowlist (CSRF) refusal.
pub const ERR_ORIGIN_REFUSED: &str = "urn:kb:errors:origin-refused";
/// `urn:kb:errors:host-refused` — the DNS-rebinding `Host` refusal.
///
/// Deliberately a DIFFERENT urn from the Origin one: the SPA and the CLI
/// must be able to tell "your `Host` is not on this daemon's allowlist,
/// add `[server] hostnames`" (a one-line config fix) from "your `Origin`
/// is not allowed" (a CORS-shaped problem). Collapsing them would make
/// the self-diagnosing 403 body ambiguous.
pub const ERR_HOST_REFUSED: &str = "urn:kb:errors:host-refused";

fn forbidden(urn: &'static str, detail: String) -> Response<Body> {
    let body = serde_json::json!({
        "type": urn,
        "title": "Forbidden",
        "status": 403,
        "detail": detail,
    });
    let mut resp = (StatusCode::FORBIDDEN, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    // A security refusal must not be cached by anything in between.
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

// --- SEC-02 — the `Host` guard (DNS rebinding) ----------------------------
//
// WHY this exists, in the victim's own words: a page served from
// `http://evil.example:4000` whose DNS the attacker re-points at
// `127.0.0.1` makes the victim's browser connect from the box, so
// `is_loopback_origin` says "loopback", `request_is_admitted` hands it
// OPERATOR authority with no credential, and `origin_allowlist`'s
// `Origin == Host` arm is satisfied because under rebinding the page's
// origin IS the rebound name. Every /api verb, including
// `DELETE /api/kb`, is then reachable from a web page. `Host` is the
// only value in that request the attacker does not fully control, so it
// is the only place the attack can be stopped.
//
// The sibling implementation is `kb-code-server`'s
// `security::origin::origin_host_guard`. The ADMISSION TABLE here is
// deliberately kb's own (kb-server has `parent_origin` /
// `artifact_host_suffix` / an SPA fallback the sibling has no analogue
// for), but the PARSERS below are shared, `pub` facts about HTTP: a fix
// to the IPv6 or case handling must land in both daemons at once, or one
// of them is one-sided-bypassable.
//
// ## Where it is layered, and where it deliberately is NOT
//
// OUTERMOST on the `/api` nest (last `.layer()` in `build_router`, and
// a `route_layer` on `/capture`) — so it decides BEFORE `auth_bearer`
// and before the loopback bypass can hand out authority. Not on the
// top-level router: the artifact iframe is reached on
// `<id>.artifacts.localhost`, which is neither a loopback label nor a
// listable entry, so a top-level layer would break annotations. The SPA
// fallback is likewise unguarded on purpose — it serves public static
// bytes only, no corpus, and gating it buys nothing.
//
// ## The two places this fails open, and why that is the right trade
//
// `host_gate_applies` enforces `Host` for a loopback peer ALWAYS (no
// configuration required — the rebinding victim is by definition a
// loopback peer) and for a non-loopback peer only once `[server] hostnames`
// is non-empty. "Loopback peer" here means the RAW TCP peer
// (`peer_is_trusted`), deliberately NOT `request_is_loopback`: that one
// resolves through `X-Forwarded-For`, and a rebound page can set that
// header, so using it would let the page talk its way out of the gate.
// A
// non-loopback peer is by construction arriving through a reverse proxy
// that is already the authentication gate, and refusing its `Host`
// before the operator has had a chance to write `hostnames` would take
// a deployed daemon down on upgrade: a hardening unit that bricks the
// deployment it hardens is not a hardening unit. The boot warning in
// `serve_with_paths` names the key instead of silently doing nothing.
//
// The second is a request that carries NO NAME: no `Host` header and no
// request-URI authority, which is an HTTP/1.0 client or an in-process
// caller. That is refused by nothing, because there is nothing to
// enforce — and refusing it would break every `tower::oneshot` client
// in this repo's own suite, which is a cost with no security benefit
// (a browser always sends the name, so the rebinding victim is always
// checked). The name is looked up in `effective_host_name`, which
// consults the URI authority as well as `Host`: HTTP/2 makes `Host`
// OPTIONAL when `:authority` is present (RFC 9113 §8.3.1), so a proxy
// with an HTTP/2 upstream reaches `/api` with no `Host` header at all,
// and reading only the header left such a deploy with no `Host`
// enforcement even with `hostnames` configured.

/// A hostname (no scheme, possibly with a port, possibly bracketed IPv6)
/// split into its host label and optional port. `None` for an empty or
/// unparseable value. The label is lower-cased and, for IPv6, KEEPS its
/// brackets so it compares equal to the `[::1]` spelling operators write.
///
/// The label is also CANONICALISED by the private
/// `canonical_host_label` below: one trailing root dot is dropped, so
/// the absolute form an operator actually types
/// (`http://localhost.:4000`) is compared as the name it is. See that
/// function for why a trailing dot must not be a 403.
///
/// `pub` (SEC-02): `kb-code-server`'s `security::origin` imports this
/// rather than keeping a second copy — a parser that mishandles
/// `[::1]:4000` in one daemon and not the other is a silent one-sided
/// bypass. Never re-derive it.
pub fn split_host_port(value: &str) -> Option<(String, Option<&str>)> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Some(rest) = v.strip_prefix('[') {
        let (inner, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':');
        return Some((format!("[{}]", inner.to_ascii_lowercase()), port));
    }
    match v.rsplit_once(':') {
        // An unbracketed value with MORE than one colon is a bare IPv6
        // literal (`::1`), not host:port.
        Some((host, port)) if !host.contains(':') => {
            Some((canonical_host_label(host)?, Some(port)))
        }
        _ => Some((canonical_host_label(v)?, None)),
    }
}

/// Lower-case a raw `Host`/`hostnames` label and drop ONE trailing root
/// dot. `None` when nothing is left, which the callers turn into a
/// REFUSAL (an unparseable name is refused; that is the pre-existing
/// rule and this must not become a way in).
///
/// WHY the dot goes, and why only on the non-bracketed path: a trailing
/// dot is the DNS root label (RFC 6761 §6.3 "localhost."), so
/// `localhost.` and `localhost` are the SAME name and a browser sends
/// whichever the operator typed. Before SEC-02 there was no `Host`
/// check at all, so `kb` at `http://localhost.:4000` worked; the guard
/// turned that same-box browse into a 403 on every `/api` call, while
/// the SPA shell still loaded (the top-level fallback is deliberately
/// unguarded) — the failure mode is a page that renders and then fails
/// every fetch, with no boot warning to explain it, because the bind IS
/// loopback and `KB_ALLOW_NO_AUTH` is unset. Canonicalising in the
/// PARSER (not in `host_allowed`) means a config entry written as
/// `"kbc.example.com."` normalises identically to the request label,
/// so the two can never mean different things.
///
/// Dropping the dot cannot widen the allowlist in any way that matters:
/// it maps `attacker.example.` → `attacker.example`, which is still
/// refused, and no rebound name becomes a loopback name by gaining a
/// dot. The bracketed IPv6 branch above is left alone because a dot
/// after `]` is not a root label, it is malformed input.
fn canonical_host_label(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let name = lower.strip_suffix('.').unwrap_or(&lower);
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// The always-allowed loopback host labels, in every spelling a client
/// can send one.
///
/// The rule is "does the name denote a loopback address on this box",
/// asked of `std`'s own definition rather than a hand-written list,
/// which is what admits the spellings a 4-element `matches!` missed:
///
///   * **`127.0.0.0/8`, not just `127.0.0.1`** — Linux, macOS, the BSDs
///     and Windows all route the whole /8 to `lo`, and `Ipv4Addr::
///     is_loopback` says so. `127.0.0.2` is a loopback peer and used to
///     work, so the guard 403'd a same-box browse of it. It is NOT a
///     rebinding widening: a rebound page's `Host` is the ATTACKER's
///     name, never a loopback literal (the attacker has to win the DNS
///     answer, and `127.0.0.2` answers from `/etc/hosts` on the
///     victim's own box). Nothing that a browser can be tricked into
///     sending is added — one more loopback literal, with the same
///     trust level, is the same key this list always had.
///   * **`[::1]` / `::1`** — unchanged; `::1` is the only IPv6 loopback
///     address, so asking std adds nothing and takes nothing away.
///   * **`0.0.0.0` and `[::]` stay refused.** A wildcard is not a name
///     anyone can be reached on, and `is_loopback` is false for an
///     unspecified address, so the `addr_host_literal` rule (which
///     returns `None` for a wildcard bind) is mirrored here rather than
///     re-decided. Admitting them would hand a rebound `Host: 0.0.0.0`
///     a pass.
///   * **`localhost` only — NOT `*.localhost`.** Browsers force
///     `foo.localhost` to loopback (RFC 6761 §6.3) and glibc ≥ 2.35 /
///     systemd-resolved do the same, but that is a resolver
///     convention, not a DNS fact: on a box without it — musl, an older
///     libc, or a `search` domain — `foo.localhost` falls through to
///     the search domain and can be a public name the attacker owns.
///     A name whose resolution is resolver-dependent is exactly what
///     this allowlist must not admit, so the fix for an operator who
///     WANTS that name is the documented one: add it to
///     `[server] hostnames`. It also keeps the artifact-iframe host
///     (`<id>.artifacts.localhost`) refused at `/api` — see the
///     section comment on where this guard is layered.
///   * `localhost.` and other trailing-dot forms are handled by
///     `canonical_host_label` (below), not here.
///
/// NOTE this predicate is deliberately NOT the rule behind
/// [`is_loopback_web_origin`] (CORS) or the `Origin == Host` arm of
/// `origin_allowed`. CORS is a grant to a browser page, not a check on
/// a name the daemon was reached on, and it stays `localhost` /
/// `127.0.0.1` only. Do not "fix the inconsistency" by widening that
/// one from here.
///
/// `pub` (SEC-02): shared with `kb-code-server`'s `security::origin` —
/// see the note on [`split_host_port`].
pub fn is_loopback_host_label(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    // IPv6 labels arrive bracketed (`[::1]`); `split_host_port` keeps the
    // brackets so it compares equal to the spelling operators write, and
    // `IpAddr::from_str` needs the bare form.
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    match bare.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
}

/// Normalise a configured `[server] hostnames` entry to the same shape
/// [`host_allowed`] compares a request label in: lower-cased, port
/// stripped. Called once at boot by `OriginConfig::from_server_section`
/// so a config entry and a `Host:` value can never mean different
/// things (the sibling compares a port-stripped request label against
/// entries that were never stripped, so `["kbc.example.com:443"]` there
/// can never match — I do not inherit that).
pub fn normalize_host_label(entry: &str) -> Option<String> {
    split_host_port(entry).map(|(label, _port)| label)
}

/// The host literal of a `[server] addr` value, or `None` for a
/// wildcard. `addr = "192.168.1.5:4000"` → `Some("192.168.1.5")`;
/// `0.0.0.0` / `[::]` → `None`, because a wildcard is not a name anyone
/// can be reached on, and admitting it would hand a rebound
/// `Host: 0.0.0.0` a pass.
///
/// An `addr` with no port yields `None` too: it is not the bindable
/// `host:port` form `validate` requires, and guessing at it would mean
/// guessing which name the operator is reachable on.
pub fn addr_host_literal(addr: &str) -> Option<String> {
    let (label, port) = split_host_port(addr)?;
    port?;
    let bare = label.trim_matches(|c| c == '[' || c == ']');
    if bare.is_empty() {
        return None;
    }
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if ip.is_unspecified() {
            return None;
        }
    }
    Some(label)
}

/// `Host:` admission for ONE name. The name has already been split and
/// canonicalised by [`split_host_port`], so this compares like with
/// like: `localhost.`, `LOCALHOST:4000` and `127.0.0.2` are all
/// admitted (see [`is_loopback_host_label`] for why each of those is
/// loopback, and why `0.0.0.0` / `[::]` are not). An unparseable name
/// is refused.
///
/// `None` means "this request carries no name at all" and PASSES. The
/// name is looked up by [`effective_host_name`], which is where the
/// `:authority` fallback and the reasoning for the nameless case
/// live — change one and you must change the other.
pub fn host_allowed(host: Option<&str>, cfg: &OriginConfig) -> bool {
    let Some(host) = host else {
        return true;
    };
    let Some((label, _port)) = split_host_port(host) else {
        return false;
    };
    is_loopback_host_label(&label)
        || cfg.hostnames.contains(&label)
        || cfg.addr_host.as_deref() == Some(label.as_str())
}

/// The name a request was made to, or `None` if it carries none at all.
/// `Host` header first, then the request URI's AUTHORITY.
///
/// WHY the authority fallback exists, and why the old comment was
/// wrong: it claimed "HTTP/2 requests carry `:authority`, which hyper
/// folds into `Host` for us". hyper does not guarantee that. RFC 9113
/// §8.3.1 makes a `Host` header OPTIONAL when `:authority` is present,
/// and real proxies take the option — an HTTP/2 upstream (Traefik's
/// h2 backend, Caddy, nginx proxying to an h2 origin) reaches `/api`
/// with the name in `:authority` and NO `Host` header. This function's
/// predecessor read only the header, so `host_allowed(None)` decided
/// the request and `None` meant PASS: an operator who set
/// `hostnames = ["kb.example.com"]` on exactly that deploy got no `Host`
/// enforcement at all, with no error anywhere to say so. That is the
/// hole this closes. `Uri::authority` is where hyper reconstructs
/// `:scheme`/`:authority` into the request, and it is also populated
/// for an HTTP/1.1 proxy in absolute-form, so one lookup covers both.
///
/// Do NOT overstate what this buys. Browsers do not speak h2c, so the
/// rebinding page is unaffected either way: it arrives as HTTP/1.1 or
/// TLS h2 and always sends the attacker's chosen name, which
/// [`host_allowed`] checks. This is about PROXIES honouring a list the
/// operator configured.
///
/// A `Host` header that is present but not readable as text (invalid
/// UTF-8) reports as the EMPTY name, i.e. unparseable, i.e. REFUSED —
/// never as absent. The predecessor's `and_then(to_str().ok())`
/// collapsed "cannot read the name" into "no name", so a header the
/// client fully controls was itself a way past the guard.
///
/// `pub` (SEC-02): `kb-code-server`'s `security::origin::
/// origin_host_guard` should import this rather than re-deriving where
/// the name lives — that sibling reads `Host` only and carries the same
/// hole.
pub fn effective_host_name(req: &Request<Body>) -> Option<&str> {
    match req.headers().get(header::HOST) {
        Some(v) => Some(v.to_str().unwrap_or("")),
        None => req.uri().authority().map(|a| a.as_str()),
    }
}

/// When the `Host` check applies. Pure, so the fail-open boundary can
/// be unit-tested as a table without a socket — same decomposition as
/// [`request_is_admitted`].
pub fn host_gate_applies(peer_is_loopback: bool, hostnames_configured: bool) -> bool {
    peer_is_loopback || hostnames_configured
}

/// SEC-02 — the `Host` guard. See the section comment above for the
/// threat, the layering, and the two deliberate fail-opens (a
/// non-loopback peer before `hostnames` is configured, and a request
/// that carries no name at all).
pub async fn host_guard(
    State(origin_cfg): State<Arc<OriginConfig>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    // NOT `request_is_loopback`. That resolves through `X-Forwarded-For`
    // when the peer is a trusted hop — right for the AUTH decision (a real
    // proxy on the box may say who it forwards for), but fatal here:
    // `X-Forwarded-For` is not a forbidden request header, so a rebound page
    // sets it itself. One `X-Forwarded-For: 203.0.113.9` made
    // `request_is_loopback` false, which — with `hostnames` empty — made
    // `host_gate_applies` false and switched this guard off, reopening the
    // hole on the `KB_ALLOW_NO_AUTH=1` deployment. The gate must be decided
    // by something no header can move: the raw TCP peer.
    let peer_is_loopback = peer_is_trusted(
        req.extensions()
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip()),
        &origin_cfg.trusted_proxies,
    );
    if !host_gate_applies(peer_is_loopback, !origin_cfg.hostnames.is_empty()) {
        return next.run(req).await;
    }
    // The name is wherever HTTP put it: `Host`, or — for an HTTP/2
    // upstream, or an HTTP/1.1 proxy in absolute-form — the request
    // URI's authority. Reading only the header is what let a
    // proxied deploy configured with `hostnames = ["kb.example.com"]`
    // run with NO Host enforcement at all: `None` meant pass. See
    // `effective_host_name`.
    let host = effective_host_name(&req);
    if !host_allowed(host, &origin_cfg) {
        return forbidden(
            ERR_HOST_REFUSED,
            format!(
                "Host {:?} is not in this daemon's allowlist. Add it to `[server] hostnames`.",
                host.unwrap_or("")
            ),
        );
    }
    // NO NAME AT ALL, and it still passes — a deliberate decision, not
    // an oversight. Getting here means no `Host` header AND no URI
    // authority, which is an HTTP/1.0 client (RFC 9112 made `Host`
    // mandatory in 1.1) or an in-process `tower::oneshot` caller
    // (`Request::builder().uri("/api/kbs")`). It is not a rebinding
    // vector: rebinding needs a BROWSER, a browser always sends the
    // page's name, and that name is checked above — "no name" is not a
    // way to present a name that is not on the list, only a way to
    // present none. Refusing it would 403 every in-process client in
    // this repo's own suite for no security gain, and an HTTP/1.0
    // caller has no name to check, so there is nothing to enforce.
    next.run(req).await
}

// --- v0.4 A2 — bearer-token auth + v0.34 Y1 identity ----------------------

/// Per-request identity (v0.34 Y1 / kb-users/1). ATTRIBUTION only —
/// authorization remains one trust tier (invariant #4). Inserted into
/// request extensions by [`auth_bearer`] on every admitted request;
/// handlers take `Extension<Identity>` and never re-read identity headers.
#[derive(Clone, Debug)]
pub struct Identity {
    pub user: String,
    pub source: IdentitySource,
}

/// How the identity ladder resolved this request's user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentitySource {
    /// Trusted-hop identity header (e.g. `Remote-User`).
    Header,
    /// Multi-user token registry match (`Authorization` or `X-Kb-Token`).
    Token,
    /// Legacy shared daemon token (`<config>/token`).
    Legacy,
    /// Loopback admit with no credentials (or `KB_ALLOW_NO_AUTH` fallback).
    Loopback,
}

impl IdentitySource {
    /// Wire / `identity_source` field value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Token => "token",
            Self::Legacy => "legacy",
            Self::Loopback => "loopback",
        }
    }
}

/// Header name for the edge-lane per-user token carrier. Production
/// traefik `kb-inject-bearer` overwrites `Authorization` with the shared
/// daemon token, so agents/scripts carry their registry secret here.
pub const X_KB_TOKEN: &str = "x-kb-token";

/// Bearer-token auth middleware. Applied to `/api/*` only; the artifact
/// subdomain handler stays open (it serves sandboxed bytes intended for
/// embedding, with the v0.3 outbound scrubbing layer running separately).
///
/// Loopback bypass: the check is skipped only when the *genuine* client
/// — resolved through any trusted reverse-proxy hops — is a loopback
/// address. See [`request_is_loopback`] for the exact rule; the short
/// version is that the immediate TCP peer must be a trusted hop
/// (loopback, or a `[server] trusted_proxies` entry) before
/// `X-Forwarded-For` is consulted at all, and the header is walked
/// right-to-left so a spoofed leftmost `X-Forwarded-For: 127.0.0.1`
/// cannot manufacture a loopback origin.
///
/// v0.34 Y1 — ADMISSION and IDENTITY are split: admission stays
/// byte-identical (loopback always admits; 401 semantics unchanged), but
/// identity resolution runs on EVERY admitted request and always inspects
/// `X-Kb-Token` + `Authorization` + the trusted identity header. A valid
/// registry secret also satisfies non-loopback admission.
///
/// `ConnectInfo<SocketAddr>` is read from request extensions rather than
/// declared as an extractor — that keeps the middleware compatible with
/// test plumbing that doesn't wire `into_make_service_with_connect_info`
/// (those paths are treated as loopback, mirroring the dev fast-path).
///
/// Production deployment guide: docs/self-host.md.
pub async fn auth_bearer(
    State(auth): State<Arc<AuthConfig>>,
    mut req: Request<Body>,
    next: Next,
) -> Response<Body> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let is_loopback = is_loopback_origin(peer, req.headers(), &auth.trusted_proxies);
    let peer_trusted = peer_is_trusted(peer, &auth.trusted_proxies);

    // --- ADMISSION (byte-identical 401 surface; registry is additive) ---
    if !request_is_admitted(&auth, is_loopback, req.headers(), allow_no_auth()) {
        return unauthorized("authentication required");
    }

    // --- IDENTITY (every admitted request; pure ladder) ---
    let identity = resolve_identity(&auth, peer_trusted, req.headers());
    req.extensions_mut().insert(identity);
    next.run(req).await
}

/// True when the immediate TCP peer is a trusted hop (loopback or in
/// `trusted_proxies`). Same gate as XFF / loopback determination — reused
/// for the identity-header step (fail-closed: untrusted peers never have
/// their header read).
pub fn peer_is_trusted(peer: Option<IpAddr>, trusted_proxies: &[IpAddr]) -> bool {
    match peer {
        Some(ip) => ip.is_loopback() || trusted_proxies.contains(&ip),
        // Missing ConnectInfo fails CLOSED for trust (invariant #3/#4),
        // matching is_loopback_origin — not a trusted hop.
        None => false,
    }
}

/// Admission verdict. Pure so unit tests can pin the truth table without
/// spinning a router. Loopback always admits; a configured legacy token
/// or matching registry secret admits non-loopback; `KB_ALLOW_NO_AUTH=1`
/// admits when neither credential surface is configured (or as override
/// when none matches — see ladder notes). An invalid `X-Kb-Token` never
/// causes a 401 by itself.
fn request_is_admitted(
    auth: &AuthConfig,
    is_loopback: bool,
    headers: &axum::http::HeaderMap,
    allow_no_auth: bool,
) -> bool {
    if is_loopback {
        return true;
    }
    // Registry secret (Authorization bearer OR X-Kb-Token) is a bearer
    // credential by another name — satisfies non-loopback admission.
    if registry_match(auth, headers).is_some() {
        return true;
    }
    // Legacy shared token on Authorization.
    if let Some(expected) = auth.token.as_deref() {
        if let Some(provided) = bearer_token(headers) {
            if timing_safe_eq(provided, expected) {
                return true;
            }
        }
        // Wrong/missing Authorization with a configured token → 401.
        // (X-Kb-Token already tried above via registry_match.)
        return false;
    }
    // No legacy token. A registry (if any) already failed to match above.
    // Honor the explicit override REGARDLESS of registry emptiness: the
    // registry exists for ATTRIBUTION — adding one to an Authelia-gated
    // deploy (upstream-proxy-is-the-gate, KB_ALLOW_NO_AUTH=1) must never
    // tighten admission for browser users carrying only Remote-User.
    // Default stays fail-closed (allow_no_auth is false unless set).
    allow_no_auth
}

/// Identity resolution ladder (first match wins; TOKEN BEATS HEADER —
/// explicit beats ambient). Pure; unit-tested. Every username passes
/// through [`kb_core::identity::normalize_username`].
///
/// 1. **Token** — registry match on `Authorization` then `X-Kb-Token`
/// 2. **Header** — trusted peer + configured header, valid username
/// 3. **Legacy** — bearer matches shared daemon token → operator
/// 4. **Loopback** — operator (also the `KB_ALLOW_NO_AUTH` fallback)
pub fn resolve_identity(
    auth: &AuthConfig,
    peer_trusted: bool,
    headers: &axum::http::HeaderMap,
) -> Identity {
    // 1. Registry token (Authorization first, then X-Kb-Token).
    if let Some(user) = registry_match(auth, headers) {
        return Identity {
            user,
            source: IdentitySource::Token,
        };
    }

    // 2. Trusted-hop identity header.
    if peer_trusted {
        if let Some(raw) = identity_header_value(headers, &auth.identity_header) {
            match kb_core::identity::normalize_username(raw) {
                Some(user) => {
                    return Identity {
                        user,
                        source: IdentitySource::Header,
                    };
                }
                None => {
                    tracing::warn!(
                        header = %auth.identity_header,
                        value = %raw,
                        "identity header value failed username validation; falling through ladder"
                    );
                }
            }
        }
    }

    // 3. Legacy shared token → operator.
    if let Some(expected) = auth.token.as_deref() {
        if let Some(provided) = bearer_token(headers) {
            if timing_safe_eq(provided, expected) {
                return Identity {
                    user: auth.operator.clone(),
                    source: IdentitySource::Legacy,
                };
            }
        }
    }

    // 4. Loopback / no-auth fallback → operator.
    Identity {
        user: auth.operator.clone(),
        source: IdentitySource::Loopback,
    }
}

/// Match a presented secret against the multi-user registry. Candidates
/// are taken from `Authorization: Bearer` then `X-Kb-Token` (that order).
/// Returns the matched username (already lowercase-valid).
fn registry_match(auth: &AuthConfig, headers: &axum::http::HeaderMap) -> Option<String> {
    if auth.tokens.is_empty() {
        return None;
    }
    // Authorization first, then X-Kb-Token — explicit primary carrier first.
    let candidates = [bearer_token(headers), x_kb_token(headers)];
    for candidate in candidates.into_iter().flatten() {
        if let Some(user) = match_registry_secret(auth, candidate) {
            return Some(user);
        }
    }
    None
}

fn match_registry_secret(auth: &AuthConfig, presented: &str) -> Option<String> {
    use kb_core::identity::TokenSecret;
    use sha2::{Digest, Sha256};
    for entry in &auth.tokens {
        let ok = match &entry.secret {
            TokenSecret::Plain(expected) => timing_safe_eq(presented, expected),
            TokenSecret::Sha256(hex_expected) => {
                let mut h = Sha256::new();
                h.update(presented.as_bytes());
                let dig = hex::encode(h.finalize());
                // Constant-time on the digest hex (not the presented secret).
                timing_safe_eq(&dig, hex_expected)
            }
        };
        if ok {
            return Some(entry.user.clone());
        }
    }
    None
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
}

fn x_kb_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(X_KB_TOKEN)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Read the configured identity header (case-insensitive name match).
fn identity_header_value<'a>(
    headers: &'a axum::http::HeaderMap,
    identity_header_lower: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name.as_str().eq_ignore_ascii_case(identity_header_lower))
        .and_then(|(_, v)| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Whether the operator has explicitly opted into running with NO kb-level
/// bearer auth on a non-loopback bind, via `KB_ALLOW_NO_AUTH=1`. This is the
/// "an upstream proxy (Authelia, oauth2-proxy, …) is the real authentication
/// gate" escape hatch: it lets a token-less daemon serve public binds, but
/// only as a loud, deliberate choice. Single source of truth so the request
/// path (`auth_bearer`) and the startup guard (`serve_with_paths`) agree on
/// the env-var name. Named to mirror the existing `KB_DEV_ORIGIN_ANY` knob.
///
/// `pub` (W1.2): `kb-code-server` imports this directly for its own startup
/// guard so the `KB_ALLOW_NO_AUTH=1` override is single-sourced with kb's —
/// never re-derive this check, import it.
pub fn allow_no_auth() -> bool {
    std::env::var("KB_ALLOW_NO_AUTH").as_deref() == Ok("1")
}

/// Startup-guard predicate (pure, so it's unit-testable without binding a
/// socket): should `serve_with_paths` REFUSE to start? True only when the
/// listener is non-loopback AND no bearer token is configured AND the
/// operator hasn't set the `KB_ALLOW_NO_AUTH=1` escape hatch. Keeps the
/// fail-closed rule in one place next to `auth_bearer`'s request-time twin.
///
/// `pub` (W1.2): `kb-code-server` calls this from its own bind-time guard
/// (mirroring `serve_with_paths` below) instead of re-implementing the
/// fail-closed rule — invariant #4 is security single-sourced, never copied.
pub fn refuse_public_bind_without_auth(
    bind_is_loopback: bool,
    has_token: bool,
    allow_no_auth: bool,
) -> bool {
    !bind_is_loopback && !has_token && !allow_no_auth
}

/// Resolve the genuine client IP from an `X-Forwarded-For` value by
/// walking it RIGHT-TO-LEFT, skipping entries that are loopback or in
/// `trusted_proxies` (the proxy hops we put there ourselves). The first
/// untrusted entry from the right is the real client; `None` means every
/// entry was a trusted hop, so the original client was loopback too.
/// Unparseable entries (obfuscated `unknown` tokens, garbage) are skipped
/// — they can't be an address we'd trust, and skipping keeps the walk
/// robust rather than letting one bad hop poison the chain.
///
/// This is the load-bearing anti-spoof step: a real reverse proxy
/// *appends* the client's address to the right of whatever the client
/// sent, so a forged leftmost `127.0.0.1` sits to the LEFT of the true
/// client IP and is never the value returned here.
///
/// `pub` (W1.2): `kb-code-server` imports this directly — see the note on
/// [`is_loopback_origin`].
pub fn xff_real_client(xff: &str, trusted_proxies: &[IpAddr]) -> Option<IpAddr> {
    for entry in xff.rsplit(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match entry.parse::<IpAddr>() {
            Ok(ip) if ip.is_loopback() || trusted_proxies.contains(&ip) => continue,
            Ok(ip) => return Some(ip),
            Err(_) => continue,
        }
    }
    None
}

/// Decide whether a request's genuine client is a loopback address —
/// i.e. whether the auth / rate-limit / outbound-scrub layers should
/// bypass. Single source of truth: every consumer must call this (or
/// the [`request_is_loopback`] wrapper that pulls inputs out of the
/// request) so a future change to the rule lands everywhere at once.
///
/// Rule:
///  1. `peer = None` (no `ConnectInfo` wired) → fails CLOSED. Treating
///     a missing peer as loopback (the pre-v0.7.1 P2 behaviour) made
///     the whole security layer silently fail-OPEN whenever a future
///     refactor or a test harness served the router bare.
///  2. The immediate TCP peer must be a *trusted hop* — loopback, or a
///     configured `trusted_proxies` IP — before `X-Forwarded-For` is
///     consulted at all. A direct non-loopback client is never loopback,
///     whatever headers it sends.
///  3. With no `X-Forwarded-For`, the trusted peer *is* the client.
///  4. Otherwise [`xff_real_client`] resolves the true client through the
///     proxy chain; only a loopback (or all-trusted) chain bypasses.
///
/// `pub` (W1.2): `kb-code-server` imports this — the sibling daemon reuses
/// kb's exact loopback-determination rule rather than re-deriving it, so a
/// future change to the anti-spoof logic lands in both daemons at once.
pub fn is_loopback_origin(
    peer: Option<IpAddr>,
    headers: &axum::http::HeaderMap,
    trusted_proxies: &[IpAddr],
) -> bool {
    let Some(peer) = peer else {
        return false;
    };
    let peer_trusted = peer.is_loopback() || trusted_proxies.contains(&peer);
    if !peer_trusted {
        return false;
    }
    let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) else {
        return peer.is_loopback();
    };
    xff_real_client(xff, trusted_proxies).is_none()
}

/// Convenience wrapper for middleware that has a full [`Request`] in hand.
/// New call sites should prefer [`is_loopback_origin`] with the inputs
/// pulled out explicitly (it's easier to test).
///
/// `pub` (W1.2): `kb-code-server` imports this — it's the invariant-#3
/// `ConnectInfo`-from-extensions peer extraction, single-sourced so kb-code
/// never re-derives the "missing peer fails closed" rule.
pub fn request_is_loopback(req: &Request<Body>, trusted_proxies: &[IpAddr]) -> bool {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    is_loopback_origin(peer, req.headers(), trusted_proxies)
}

/// Constant-time string comparison so the auth check doesn't leak token
/// length or prefix via timing. The `subtle` crate is overkill for one
/// call site — this is the standard 4-line hand-rolled version.
fn timing_safe_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

fn unauthorized(detail: &str) -> Response<Body> {
    let body = serde_json::json!({
        "type": "urn:kb:errors:unauthorized",
        "title": "Unauthorized",
        "status": 401,
        "detail": detail,
    });
    let mut resp = (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"kb\""),
    );
    resp
}

// --- v0.4 B1 — per-token rate limit ---------------------------------------

/// Fixed-window per-token bucket. v0.4 picks fixed-window over a true
/// token bucket for two reasons: (a) the policy is "60 req/min" not
/// "1 req/sec with bursts" — a fixed window is the natural shape, (b)
/// no extra dep (governor would pull tower-glue + rand). Per-route
/// configuration ships in v0.5; v0.4 bakes the policy into the layer
/// instance the router stacks.
#[derive(Debug)]
pub struct RateLimiter {
    capacity: u32,
    window: std::time::Duration,
    state: std::sync::Mutex<LimiterState>,
    /// v0.7.1 — reverse-proxy IPs trusted to set `X-Forwarded-For`,
    /// shared with `AuthConfig` / `OriginConfig`. The loopback bypass
    /// mirrors `auth_bearer` and must see the same trusted set.
    trusted_proxies: Arc<Vec<IpAddr>>,
}

/// v0.7.1 H6 — per-key windows plus a call counter for amortised
/// eviction. The key is the (client-controlled) bearer-token hash, so
/// without eviction `windows` would grow one entry per token ever seen
/// — a slow memory-exhaustion DoS on a daemon meant to be exposed.
#[derive(Debug, Default)]
struct LimiterState {
    windows: std::collections::HashMap<String, Window>,
    calls_since_sweep: u32,
}

/// Sweep fully-elapsed windows out of the map every this-many
/// `try_consume` calls. A full sweep is `O(map len)`; amortising it
/// keeps the per-call cost low while still bounding the map to roughly
/// one window's worth of active keys.
const SWEEP_INTERVAL: u32 = 256;

#[derive(Debug, Clone, Copy)]
struct Window {
    started_at: std::time::Instant,
    count: u32,
}

impl RateLimiter {
    pub fn new(
        capacity: u32,
        window: std::time::Duration,
        trusted_proxies: Arc<Vec<IpAddr>>,
    ) -> Self {
        Self {
            capacity,
            window,
            state: std::sync::Mutex::new(LimiterState::default()),
            trusted_proxies,
        }
    }

    /// Try to consume one slot for `key`. Returns `Ok(())` on success,
    /// `Err(retry_after_seconds)` when the window is full.
    fn try_consume(&self, key: &str) -> Result<(), u64> {
        let now = std::time::Instant::now();
        let window = self.window;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        // v0.7.1 H6 — amortised eviction. Every SWEEP_INTERVAL calls,
        // drop windows that have fully elapsed: one would be reset to
        // fresh on its next access anyway, so dropping it loses
        // nothing, and it bounds the map by active-key count rather
        // than by every token ever seen.
        state.calls_since_sweep += 1;
        if state.calls_since_sweep >= SWEEP_INTERVAL {
            state.calls_since_sweep = 0;
            state
                .windows
                .retain(|_, w| now.duration_since(w.started_at) < window);
        }

        // LOW (deep-review): avoid the per-request `String` allocation
        // that `windows.entry(key.to_string())` forced. Most calls hit
        // an existing entry — `get_mut` on a `&str` reuses the keyhash
        // without alloc. Only the (rare) first-call-per-key path
        // allocates, via `insert(key.to_string(), ...)`.
        let entry = if let Some(existing) = state.windows.get_mut(key) {
            existing
        } else {
            state.windows.insert(
                key.to_string(),
                Window {
                    started_at: now,
                    count: 0,
                },
            );
            state.windows.get_mut(key).expect("just inserted")
        };
        if now.duration_since(entry.started_at) >= window {
            *entry = Window {
                started_at: now,
                count: 0,
            };
        }
        if entry.count >= self.capacity {
            let elapsed = now.duration_since(entry.started_at);
            let remaining = window.saturating_sub(elapsed);
            return Err(remaining.as_secs().max(1));
        }
        entry.count += 1;
        Ok(())
    }
}

/// Per-token rate-limit middleware. Stacked on hot endpoints (search,
/// atlas recompute, review POST). Loopback bypass mirrors `auth_bearer`.
///
/// v0.34 Y1 — bucket key is the RESOLVED user when identity source is
/// `Header` or `Token` (two users through the same edge hop get
/// independent buckets). Legacy/Loopback keep the existing token-hash /
/// `<no-token>` key so single-user deploys are unchanged. Prefers
/// `Extension<Identity>` (auth_bearer runs outer of this layer on /api);
/// falls back to the pure ladder when the extension is absent.
pub async fn rate_limit(
    State(limiter): State<Arc<RateLimiter>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    if request_is_loopback(&req, &limiter.trusted_proxies) {
        return next.run(req).await;
    }
    let key = rate_limit_key(&req, &limiter.trusted_proxies);
    match limiter.try_consume(&key) {
        Ok(()) => next.run(req).await,
        Err(retry_after) => rate_limited(retry_after),
    }
}

/// Derive the rate-limit bucket key for a non-loopback request.
fn rate_limit_key(req: &Request<Body>, trusted_proxies: &[IpAddr]) -> String {
    // Prefer the identity auth_bearer already resolved (layer order:
    // auth_bearer is outer of rate_limit on the /api tree).
    if let Some(id) = req.extensions().get::<Identity>() {
        return rate_limit_key_for_identity(id, req.headers());
    }
    // Fallback: no Identity extension (shouldn't happen on /api). Use
    // token-hash / no-token — same as pre-Y single-user behaviour.
    let _ = trusted_proxies;
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(token_key)
        .unwrap_or_else(|| "<no-token>".to_string())
}

fn rate_limit_key_for_identity(id: &Identity, headers: &axum::http::HeaderMap) -> String {
    match id.source {
        IdentitySource::Header | IdentitySource::Token => {
            format!("user:{}", id.user)
        }
        IdentitySource::Legacy | IdentitySource::Loopback => headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(token_key)
            .unwrap_or_else(|| "<no-token>".to_string()),
    }
}

fn token_key(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(&h.finalize()[..16])
}

fn rate_limited(retry_after_seconds: u64) -> Response<Body> {
    let body = serde_json::json!({
        "type": "urn:kb:errors:rate-limited",
        "title": "Too Many Requests",
        "status": 429,
        "detail": format!("retry after {retry_after_seconds}s"),
    });
    let mut resp = (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    if let Ok(value) = HeaderValue::from_str(&retry_after_seconds.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, value);
    }
    resp
}

/// N7+P3+P4: request-counter + per-route latency middleware. Bumps
/// `RequestMetrics.total`, then dispatches the request, then records
/// elapsed time + per-route counter against the matching `RouteKind`
/// slot. Lock-free: one path classification + 3 atomic adds per
/// request (1 for total, 1 for route count, 1 for the latency bucket).
pub async fn count_requests(
    State(metrics): State<std::sync::Arc<crate::state::RequestMetrics>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    use std::sync::atomic::Ordering;
    metrics.total.fetch_add(1, Ordering::Relaxed);
    let path = req.uri().path();
    let kind = crate::state::classify_route(path);
    // TM-track — per-kb attribution. Capture the kb name (owned, before `req`
    // is consumed) only when detailed metrics are on, so the off-path pays
    // nothing beyond the flag load.
    let kb: Option<String> = if metrics.detailed_enabled() {
        crate::state::kb_from_path(path).map(str::to_string)
    } else {
        None
    };
    let started = std::time::Instant::now();
    let resp = next.run(req).await;
    let ms = started.elapsed().as_millis() as u64;
    metrics.by_route[kind as usize].observe(ms);
    if let Some(kb) = kb {
        metrics.observe_kb(&kb, ms);
    }
    resp
}

/// Convert a `kb_core::Error` into an `application/problem+json` response
/// per RFC 7807 with the URN-form `type` field topic 11 §C specifies.
pub fn error_to_problem_json(err: &kb_core::Error) -> Response<Body> {
    let status =
        StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = serde_json::json!({
        "type": err.problem_type(),
        "title": status.canonical_reason().unwrap_or("Error"),
        "status": err.http_status(),
        "detail": err.to_string(),
    });
    let mut resp = (status, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_origin_predicate() {
        // Allowed: the three loopback hosts, any port, http or https.
        for ok in [
            "http://localhost",
            "http://localhost:4738",
            "https://localhost:9999",
            "http://127.0.0.1:4001",
            "http://[::1]:4000",
            "http://[::1]",
        ] {
            assert!(is_loopback_web_origin(ok), "{ok} should be allowed");
        }
        // Refused: anything that isn't unambiguously loopback.
        for bad in [
            "http://evil.example",
            "https://kb.example.com",
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            "http://192.168.1.5:4000",
            "http://[::2]:4000",
            "ftp://localhost",
            "null",
            "",
        ] {
            assert!(!is_loopback_web_origin(bad), "{bad} should be refused");
        }
    }

    fn dev() -> OriginConfig {
        OriginConfig::default()
    }

    fn prod() -> OriginConfig {
        OriginConfig {
            artifact_host_suffix: ".artifacts.example.com".to_string(),
            parent_origin: "https://kb.example.com".to_string(),
            ..OriginConfig::default()
        }
    }

    /// `[server] hostnames = [...]` resolved the way boot resolves it.
    fn with_hostnames(hostnames: &[&str], addr: &str) -> OriginConfig {
        OriginConfig {
            hostnames: Arc::new(
                hostnames
                    .iter()
                    .filter_map(|h| normalize_host_label(h))
                    .collect(),
            ),
            addr_host: addr_host_literal(addr),
            ..OriginConfig::default()
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// Build a request with a wired `ConnectInfo` peer + optional XFF,
    /// the way `into_make_service_with_connect_info` does at runtime.
    fn req_with(peer: &str, xff: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().extension(ConnectInfo(SocketAddr::new(ip(peer), 50000)));
        if let Some(xff) = xff {
            b = b.header("x-forwarded-for", xff);
        }
        b.body(Body::empty()).unwrap()
    }

    #[test]
    fn parent_origin_allowed() {
        assert!(origin_allowed("http://localhost:4000", None, &dev()));
    }

    #[test]
    fn artifact_subdomain_origin_allowed() {
        assert!(origin_allowed(
            "http://kitchen-sink.artifacts.localhost:4000",
            None,
            &dev()
        ));
        assert!(origin_allowed(
            "http://abc123.artifacts.localhost",
            None,
            &dev()
        ));
    }

    #[test]
    fn arbitrary_origin_blocked() {
        assert!(!origin_allowed("http://evil.com", None, &dev()));
        assert!(!origin_allowed("https://google.com", None, &dev()));
        assert!(!origin_allowed("http://localhost:5555", None, &dev())); // wrong port, no host
    }

    #[test]
    fn artifacts_localhost_root_blocked() {
        // No subdomain prefix = parent of subdomain tree, not allowed.
        assert!(!origin_allowed("http://artifacts.localhost", None, &dev()));
    }

    #[test]
    fn same_origin_allowed_via_host() {
        // SPA served from this daemon → Origin and Host align, accept.
        assert!(origin_allowed(
            "http://127.0.0.1:4737",
            Some("127.0.0.1:4737"),
            &dev()
        ));
        assert!(origin_allowed(
            "http://192.168.1.5:4737",
            Some("192.168.1.5:4737"),
            &dev()
        ));
    }

    #[test]
    fn cross_origin_blocked_even_with_host() {
        // Origin=evil but Host=us → not same-origin, blocked.
        assert!(!origin_allowed(
            "http://evil.com",
            Some("127.0.0.1:4737"),
            &dev()
        ));
    }

    // --- production OriginConfig (custom domain) -------------------------

    #[test]
    fn production_parent_and_artifact_origins_accepted() {
        assert!(origin_allowed("https://kb.example.com", None, &prod()));
        assert!(origin_allowed(
            "https://kitchen-sink.artifacts.example.com",
            None,
            &prod()
        ));
    }

    #[test]
    fn production_blocks_dev_localhost_origins() {
        // The default-dev .artifacts.localhost must NOT cross-leak into a
        // production-configured daemon.
        assert!(!origin_allowed(
            "http://abc.artifacts.localhost",
            None,
            &prod()
        ));
        assert!(!origin_allowed("http://localhost:4000", None, &prod()));
    }

    // --- v0.7.1 C1 — X-Forwarded-For anti-spoof ------------------------

    #[test]
    fn xff_real_client_empty_and_loopback_yield_none() {
        assert_eq!(xff_real_client("", &[]), None);
        assert_eq!(xff_real_client("127.0.0.1", &[]), None);
        assert_eq!(xff_real_client("::1", &[]), None);
        assert_eq!(xff_real_client(" 127.0.0.1 , ::1 ", &[]), None);
    }

    #[test]
    fn xff_real_client_returns_external_ip() {
        assert_eq!(xff_real_client("8.8.8.8", &[]), Some(ip("8.8.8.8")));
        // Trailing loopback hop is skipped; the external client is found.
        assert_eq!(
            xff_real_client("8.8.8.8, 127.0.0.1", &[]),
            Some(ip("8.8.8.8"))
        );
    }

    #[test]
    fn xff_real_client_ignores_spoofed_leftmost_loopback() {
        // The attack: client sends `X-Forwarded-For: 127.0.0.1`; a real
        // proxy APPENDS the true client IP to the right. Walking
        // right-to-left finds the appended IP, not the forged one.
        assert_eq!(
            xff_real_client("127.0.0.1, 8.8.8.8", &[]),
            Some(ip("8.8.8.8"))
        );
        assert_eq!(
            xff_real_client("::1, 203.0.113.7", &[]),
            Some(ip("203.0.113.7"))
        );
    }

    #[test]
    fn xff_real_client_skips_trusted_proxies_and_garbage() {
        let trusted = [ip("10.0.0.1")];
        // 10.0.0.1 is a trusted hop → skipped; 8.8.8.8 is the client.
        assert_eq!(
            xff_real_client("8.8.8.8, 10.0.0.1", &trusted),
            Some(ip("8.8.8.8"))
        );
        // All hops trusted/loopback → no genuine remote client.
        assert_eq!(xff_real_client("127.0.0.1, 10.0.0.1", &trusted), None);
        // Obfuscated/garbage entries are skipped, not fatal.
        assert_eq!(
            xff_real_client("8.8.8.8, unknown", &[]),
            Some(ip("8.8.8.8"))
        );
        assert_eq!(xff_real_client("garbage", &[]), None);
    }

    // invariant:3 fail-closed
    #[test]
    fn request_is_loopback_no_connect_info_fails_closed() {
        // v0.7.1 P2 — a request with no `ConnectInfo` peer can't be
        // proven loopback, so it does NOT bypass (fail closed). Every
        // `serve_*` entrypoint wires the connect-info extractor, so this
        // is unreachable at runtime — it's the defence-in-depth default.
        let req = Request::builder().body(Body::empty()).unwrap();
        assert!(!request_is_loopback(&req, &[]));
    }

    #[test]
    fn request_is_loopback_peer_only() {
        assert!(request_is_loopback(&req_with("127.0.0.1", None), &[]));
        assert!(request_is_loopback(&req_with("::1", None), &[]));
        assert!(!request_is_loopback(&req_with("8.8.8.8", None), &[]));
    }

    // invariant:4 xff-walk
    #[test]
    fn request_is_loopback_direct_remote_cannot_forge_xff() {
        // A direct (non-loopback, non-trusted) peer never has its
        // X-Forwarded-For consulted — the spoof is rejected outright.
        assert!(!request_is_loopback(
            &req_with("8.8.8.8", Some("127.0.0.1")),
            &[]
        ));
    }

    #[test]
    fn request_is_loopback_proxy_appended_spoof_is_rejected() {
        // The C1 regression: loopback proxy peer, client forged
        // `127.0.0.1`, the proxy appended the real client IP. Must
        // resolve to the real client → enforce (not loopback).
        assert!(!request_is_loopback(
            &req_with("127.0.0.1", Some("127.0.0.1, 8.8.8.8")),
            &[]
        ));
        assert!(!request_is_loopback(
            &req_with("127.0.0.1", Some("8.8.8.8")),
            &[]
        ));
    }

    #[test]
    fn request_is_loopback_genuine_loopback_chain_bypasses() {
        // Same-host proxy forwarding a genuinely-local client.
        assert!(request_is_loopback(
            &req_with("127.0.0.1", Some("127.0.0.1")),
            &[]
        ));
    }

    #[test]
    fn request_is_loopback_trusted_proxy_peer() {
        let trusted = vec![ip("172.17.0.1")];
        // A configured (non-loopback) proxy IP is a trusted hop: XFF is
        // consulted, and a real remote client still enforces.
        assert!(!request_is_loopback(
            &req_with("172.17.0.1", Some("8.8.8.8")),
            &trusted
        ));
        assert!(request_is_loopback(
            &req_with("172.17.0.1", Some("127.0.0.1")),
            &trusted
        ));
        // The same proxy IP, but NOT configured as trusted → enforce.
        assert!(!request_is_loopback(
            &req_with("172.17.0.1", Some("127.0.0.1")),
            &[]
        ));
    }

    // --- N7 — count_requests middleware --------------------------------

    use axum::body::to_bytes;
    use axum::routing::get;
    use axum::Router;

    /// Drive a tiny axum router with the `count_requests` layer and
    /// confirm the atomic counter increments per request.
    #[tokio::test]
    async fn count_requests_bumps_per_request() {
        use crate::state::RequestMetrics;
        use std::sync::atomic::Ordering;
        let metrics = Arc::new(RequestMetrics::default());
        let app: Router = Router::new()
            .route("/ping", get(|| async { "pong" }))
            .layer(axum::middleware::from_fn_with_state(
                metrics.clone(),
                count_requests,
            ));
        // Hammer the route a few times via tower's oneshot — no real
        // socket, no port binding. Each oneshot drives one request
        // through the layer stack to the route handler.
        use tower::ServiceExt;
        for _ in 0..5 {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], b"pong");
        }
        assert_eq!(
            metrics.total.load(Ordering::Relaxed),
            5,
            "the request counter should reflect every request reaching the layer"
        );
    }

    /// N7: 404s + 5xxs count too — the middleware doesn't filter on
    /// outcome. The metric reflects server load (work to dispatch +
    /// build the response), not just successful operations.
    #[tokio::test]
    async fn count_requests_counts_errors_and_404s() {
        use crate::state::RequestMetrics;
        use std::sync::atomic::Ordering;
        let metrics = Arc::new(RequestMetrics::default());
        let app: Router = Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route(
                "/boom",
                get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "kaboom") }),
            )
            .layer(axum::middleware::from_fn_with_state(
                metrics.clone(),
                count_requests,
            ));
        use tower::ServiceExt;
        let _ = app
            .clone()
            .oneshot(Request::builder().uri("/ok").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let _ = app
            .clone()
            .oneshot(Request::builder().uri("/boom").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metrics.total.load(Ordering::Relaxed), 3);
    }

    // --- v0.7.1 H6 — rate-limiter eviction -----------------------------

    #[test]
    fn rate_limiter_evicts_elapsed_windows() {
        // The per-key map must not grow unbounded — elapsed windows are
        // swept so it's bounded by one window's worth of active keys,
        // not by every (client-controlled) token hash ever seen.
        let limiter = RateLimiter::new(
            100,
            std::time::Duration::from_millis(5),
            Arc::new(Vec::new()),
        );
        // Fill the map with distinct keys — fewer than SWEEP_INTERVAL,
        // so no sweep has run yet.
        for i in 0..50 {
            let _ = limiter.try_consume(&format!("token-{i}"));
        }
        assert_eq!(limiter.state.lock().unwrap().windows.len(), 50);
        // Let every one of those windows fully elapse.
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Drive enough calls (one steady key) to trip the periodic sweep.
        for _ in 0..SWEEP_INTERVAL {
            let _ = limiter.try_consume("steady");
        }
        let len = limiter.state.lock().unwrap().windows.len();
        assert!(
            len <= 2,
            "expected the 50 elapsed windows swept, map still has {len}"
        );
    }

    // --- deep-review P1 → P0 — fail-closed auth on token-less public bind ---

    /// The startup-guard predicate truth table. Refuse to serve ONLY when the
    /// bind is non-loopback, no token is configured, and the operator hasn't
    /// set the override. Every other combination is allowed to boot.
    // invariant:4 fail-closed-bind
    #[test]
    fn refuse_public_bind_without_auth_truth_table() {
        // Non-loopback + no token + no override → REFUSE (the dangerous case).
        assert!(refuse_public_bind_without_auth(false, false, false));
        // …but a configured token clears it.
        assert!(!refuse_public_bind_without_auth(false, true, false));
        // …and the explicit override clears it (upstream proxy is the gate).
        assert!(!refuse_public_bind_without_auth(false, false, true));
        // Loopback never refuses (personal-mode default), token or not.
        assert!(!refuse_public_bind_without_auth(true, false, false));
        assert!(!refuse_public_bind_without_auth(true, true, false));
    }

    /// Build a GET `/api/x` request with a wired `ConnectInfo` peer.
    fn api_req(peer: &str) -> Request<Body> {
        Request::builder()
            .uri("/api/x")
            .extension(ConnectInfo(SocketAddr::new(ip(peer), 50000)))
            .body(Body::empty())
            .unwrap()
    }

    fn auth_app(auth: AuthConfig) -> axum::Router {
        use axum::middleware::from_fn_with_state;
        use axum::routing::get;
        axum::Router::new()
            .route("/api/x", get(|| async { "ok" }))
            .layer(from_fn_with_state(Arc::new(auth), auth_bearer))
    }

    /// The core regression: with NO token configured, a loopback request gets
    /// the personal-mode bypass (200), but a NON-loopback request now FAILS
    /// CLOSED (401) instead of the old silent auth-disable. (`KB_ALLOW_NO_AUTH`
    /// is not exercised here — env mutation would race the parallel suite; the
    /// override is covered by the `allow_no_auth ||` branch + the predicate
    /// truth table above.)
    #[tokio::test]
    async fn auth_bearer_no_token_loopback_ok_public_unauthorized() {
        use tower::ServiceExt;
        // token: None (personal-mode default).
        let app = auth_app(AuthConfig::default());
        let resp = app.clone().oneshot(api_req("127.0.0.1")).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "loopback with no token keeps the personal-mode bypass"
        );
        let resp = app.oneshot(api_req("8.8.8.8")).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "non-loopback with no token must fail closed, not silently disable auth"
        );
    }

    /// With a token configured the existing rule is unchanged: loopback
    /// bypasses; a non-loopback request needs the correct bearer token.
    #[tokio::test]
    async fn auth_bearer_with_token_enforces_on_public() {
        use tower::ServiceExt;
        let auth = AuthConfig {
            token: Some("s3cret".to_string()),
            trusted_proxies: Arc::new(Vec::new()),
            ..AuthConfig::default()
        };
        let app = auth_app(auth);
        // Loopback still bypasses even with a token set.
        let resp = app.clone().oneshot(api_req("127.0.0.1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Non-loopback, no Authorization header → 401.
        let resp = app.clone().oneshot(api_req("8.8.8.8")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // Non-loopback with the correct bearer → 200.
        let ok = Request::builder()
            .uri("/api/x")
            .header(header::AUTHORIZATION, "Bearer s3cret")
            .extension(ConnectInfo(SocketAddr::new(ip("8.8.8.8"), 50000)))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(ok).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Non-loopback with a wrong bearer → 401.
        let bad = Request::builder()
            .uri("/api/x")
            .header(header::AUTHORIZATION, "Bearer nope")
            .extension(ConnectInfo(SocketAddr::new(ip("8.8.8.8"), 50000)))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(bad).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // --- v0.34 Y1 identity ladder -----------------------------------------

    fn auth_with_registry() -> AuthConfig {
        use kb_core::identity::{TokenEntry, TokenSecret};
        AuthConfig {
            token: Some("legacy-shared".to_string()),
            trusted_proxies: Arc::new(Vec::new()),
            tokens: vec![
                TokenEntry {
                    user: "alice".into(),
                    secret: TokenSecret::Plain("alice-secret".into()),
                },
                TokenEntry {
                    user: "bob".into(),
                    secret: TokenSecret::Sha256({
                        use sha2::{Digest, Sha256};
                        let mut h = Sha256::new();
                        h.update(b"bob-secret");
                        hex::encode(h.finalize())
                    }),
                },
            ],
            operator: "operator".into(),
            identity_header: "remote-user".into(),
        }
    }

    fn headers_of(pairs: &[(&str, &str)]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    /// invariant:4 — identity ladder order: token beats header beats legacy.
    #[test]
    fn identity_ladder_token_beats_header_beats_legacy() {
        let auth = auth_with_registry();
        // Token (Authorization) wins over Remote-User.
        let h = headers_of(&[
            ("authorization", "Bearer alice-secret"),
            ("remote-user", "bob"),
        ]);
        let id = resolve_identity(&auth, true, &h);
        assert_eq!(id.user, "alice");
        assert_eq!(id.source, IdentitySource::Token);

        // X-Kb-Token also resolves as Token (and beats header).
        let h = headers_of(&[("x-kb-token", "alice-secret"), ("remote-user", "bob")]);
        let id = resolve_identity(&auth, true, &h);
        assert_eq!(id.user, "alice");
        assert_eq!(id.source, IdentitySource::Token);

        // Header when no registry secret.
        let h = headers_of(&[("remote-user", "Bob")]); // folds to bob
        let id = resolve_identity(&auth, true, &h);
        assert_eq!(id.user, "bob");
        assert_eq!(id.source, IdentitySource::Header);

        // Legacy shared token → operator.
        let h = headers_of(&[("authorization", "Bearer legacy-shared")]);
        let id = resolve_identity(&auth, false, &h);
        assert_eq!(id.user, "operator");
        assert_eq!(id.source, IdentitySource::Legacy);

        // Loopback fallback.
        let h = headers_of(&[]);
        let id = resolve_identity(&auth, true, &h);
        assert_eq!(id.user, "operator");
        assert_eq!(id.source, IdentitySource::Loopback);
    }

    /// invariant:4 — untrusted peer's identity header is NEVER read.
    #[test]
    fn identity_untrusted_peer_header_ignored() {
        let auth = auth_with_registry();
        let h = headers_of(&[("remote-user", "alice")]);
        let id = resolve_identity(&auth, false, &h);
        assert_eq!(id.user, "operator");
        assert_eq!(id.source, IdentitySource::Loopback);
    }

    /// invariant:4 — invalid username on header falls through.
    #[test]
    fn identity_invalid_header_username_falls_through() {
        let auth = auth_with_registry();
        let h = headers_of(&[("remote-user", "has space")]);
        let id = resolve_identity(&auth, true, &h);
        assert_eq!(id.user, "operator");
        assert_eq!(id.source, IdentitySource::Loopback);
    }

    /// invariant:4 — loopback + valid header is honored (peer trusted).
    #[test]
    fn identity_loopback_header_honored() {
        let auth = auth_with_registry();
        let h = headers_of(&[("remote-user", "alice")]);
        let id = resolve_identity(&auth, true, &h);
        assert_eq!(id.user, "alice");
        assert_eq!(id.source, IdentitySource::Header);
    }

    /// invariant:4 — sha256 registry form matches.
    #[test]
    fn identity_registry_sha256_matches() {
        let auth = auth_with_registry();
        let h = headers_of(&[("authorization", "Bearer bob-secret")]);
        let id = resolve_identity(&auth, false, &h);
        assert_eq!(id.user, "bob");
        assert_eq!(id.source, IdentitySource::Token);
    }

    /// invariant:4 — registry secret admits non-loopback; bad X-Kb-Token
    /// alone never 401s when Authorization is valid legacy.
    #[tokio::test]
    async fn auth_bearer_registry_admits_and_x_kb_token_ignored_when_bad() {
        use tower::ServiceExt;
        let auth = auth_with_registry();
        let app = auth_app(auth);
        // Registry secret on Authorization from public peer → 200.
        let ok = Request::builder()
            .uri("/api/x")
            .header(header::AUTHORIZATION, "Bearer alice-secret")
            .extension(ConnectInfo(SocketAddr::new(ip("8.8.8.8"), 50000)))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(ok).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Legacy Authorization + garbage X-Kb-Token still admits (X-Kb-Token
        // invalid is ignored for admission; identity uses Authorization first
        // so registry would win if it matched — here only legacy matches).
        let ok = Request::builder()
            .uri("/api/x")
            .header(header::AUTHORIZATION, "Bearer legacy-shared")
            .header("x-kb-token", "not-a-real-secret")
            .extension(ConnectInfo(SocketAddr::new(ip("8.8.8.8"), 50000)))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(ok).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // X-Kb-Token alone (valid registry) admits public.
        let ok = Request::builder()
            .uri("/api/x")
            .header("x-kb-token", "alice-secret")
            .extension(ConnectInfo(SocketAddr::new(ip("8.8.8.8"), 50000)))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(ok).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// invariant:4 — two Header users get independent rate-limit buckets.
    #[test]
    fn rate_limit_key_splits_by_resolved_user() {
        let alice = Identity {
            user: "alice".into(),
            source: IdentitySource::Header,
        };
        let bob = Identity {
            user: "bob".into(),
            source: IdentitySource::Header,
        };
        let h = headers_of(&[("authorization", "Bearer same-edge-token")]);
        assert_eq!(rate_limit_key_for_identity(&alice, &h), "user:alice");
        assert_eq!(rate_limit_key_for_identity(&bob, &h), "user:bob");
        assert_ne!(
            rate_limit_key_for_identity(&alice, &h),
            rate_limit_key_for_identity(&bob, &h)
        );
        // Legacy keeps token-hash key.
        let legacy = Identity {
            user: "operator".into(),
            source: IdentitySource::Legacy,
        };
        assert_eq!(
            rate_limit_key_for_identity(&legacy, &h),
            token_key("same-edge-token")
        );
    }

    /// Identity is inserted into extensions on every admitted request
    /// (including loopback).
    #[tokio::test]
    async fn auth_bearer_inserts_identity_extension() {
        use tower::ServiceExt;
        let auth = auth_with_registry();
        // Handler that echoes the identity user.
        let app = axum::Router::new()
            .route(
                "/api/x",
                axum::routing::get(
                    |axum::Extension(id): axum::Extension<Identity>| async move {
                        format!("{}:{}", id.user, id.source.as_str())
                    },
                ),
            )
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(auth),
                auth_bearer,
            ));
        let req = Request::builder()
            .uri("/api/x")
            .header("remote-user", "alice")
            .extension(ConnectInfo(SocketAddr::new(ip("127.0.0.1"), 50000)))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"alice:header");
    }

    /// Admission truth table incl. the Y1 review catch: with a tokens
    /// REGISTRY configured but no legacy token, KB_ALLOW_NO_AUTH must still
    /// admit a credential-less non-loopback request — the registry exists
    /// for attribution and must never TIGHTEN the upstream-proxy-is-the-
    /// gate posture. Default (no override) stays fail-closed.
    #[test]
    fn request_is_admitted_registry_never_tightens_allow_no_auth() {
        let no_creds = axum::http::HeaderMap::new();
        let mut with_registry = auth_with_registry();
        with_registry.token = None;
        // Registry + override → admitted (the fixed case).
        assert!(request_is_admitted(&with_registry, false, &no_creds, true));
        // Registry, no override → fail-closed.
        assert!(!request_is_admitted(
            &with_registry,
            false,
            &no_creds,
            false
        ));
        // Registry secret still admits without any override.
        let ok = headers_of(&[("x-kb-token", "alice-secret")]);
        assert!(request_is_admitted(&with_registry, false, &ok, false));
        // Legacy token configured → wrong/missing Authorization stays 401
        // even under the override (unchanged from main).
        let mut with_legacy = auth_with_registry();
        with_legacy.token = Some("legacy-tok".into());
        assert!(!request_is_admitted(&with_legacy, false, &no_creds, false));
        // Loopback always admits.
        assert!(request_is_admitted(&with_registry, true, &no_creds, false));
    }

    // --- SEC-02 — the Host guard (DNS rebinding) ------------------------
    //
    // The table tests pin the pure predicates; the `#[tokio::test]`s
    // drive a REAL router through `host_guard` + `origin_allowlist` with
    // a wired `ConnectInfo` loopback peer, which is exactly the shape of
    // the rebinding victim. Deleting the guard fails them.

    #[test]
    fn loopback_hosts_are_allowed_on_any_port_with_no_config() {
        let p = with_hostnames(&[], "127.0.0.1:4000");
        for h in [
            "localhost",
            "localhost:4747",
            "127.0.0.1",
            "127.0.0.1:4747",
            "[::1]",
            "[::1]:4747",
            "::1",
            "LOCALHOST:4747",
        ] {
            assert!(host_allowed(Some(h), &p), "{h} must be allowed");
        }
        // Absent Host passes — see `host_allowed`'s doc.
        assert!(host_allowed(None, &p));
    }

    #[test]
    fn a_rebound_hostname_is_refused() {
        let p = with_hostnames(&[], "127.0.0.1:4000");
        for h in ["attacker.com", "attacker.com:4747", "kb.example.com"] {
            assert!(!host_allowed(Some(h), &p), "{h} must be refused");
        }
        // An unparseable Host is refused too (empty label), while an
        // ABSENT one passes — the two are not the same input.
        assert!(!host_allowed(Some("   "), &p));
    }

    #[test]
    fn configured_hostnames_join_the_allowlist() {
        let p = with_hostnames(&["kbc.example.com"], "127.0.0.1:4000");
        assert!(host_allowed(Some("kbc.example.com"), &p));
        // Case-folded and port-stripped on BOTH sides.
        assert!(host_allowed(Some("KBC.EXAMPLE.COM:443"), &p));
        assert!(!host_allowed(Some("evil.example.com"), &p));
    }

    #[test]
    fn a_configured_entry_may_carry_a_port_and_it_is_stripped() {
        // The sibling compares a port-stripped request label against
        // entries that were never stripped, so `["kbc.example.com:443"]`
        // can never match there. I normalise at resolve time instead, so
        // it DOES match — pinned here so a future "simplification" back
        // to raw-entry comparison is a test failure, not a silent
        // turn-off of the guard in production.
        let p = with_hostnames(&["kbc.example.com:443"], "127.0.0.1:4000");
        assert!(host_allowed(Some("kbc.example.com"), &p));
        assert!(host_allowed(Some("kbc.example.com:8443"), &p));
    }

    #[test]
    fn split_host_port_handles_ipv6_and_ports() {
        assert_eq!(
            split_host_port("[::1]:4747"),
            Some(("[::1]".to_string(), Some("4747")))
        );
        assert_eq!(split_host_port("::1"), Some(("::1".to_string(), None)));
        assert_eq!(
            split_host_port("localhost:4747"),
            Some(("localhost".to_string(), Some("4747")))
        );
        assert_eq!(split_host_port(""), None);
    }

    #[test]
    fn the_addr_host_literal_is_allowed_and_a_wildcard_is_not() {
        let lan = with_hostnames(&[], "192.168.1.5:4000");
        assert!(host_allowed(Some("192.168.1.5:4000"), &lan));
        assert!(host_allowed(Some("192.168.1.5"), &lan));
        assert!(!host_allowed(Some("attacker.com"), &lan));
        // A wildcard bind is not a name anyone can be reached on.
        for wildcard in ["0.0.0.0:4000", "[::]:4000", ":::4000"] {
            let p = with_hostnames(&[], wildcard);
            assert!(!host_allowed(Some("0.0.0.0"), &p), "{wildcard}");
            assert!(!host_allowed(Some("::"), &p), "{wildcard}");
            assert!(!host_allowed(Some("[::]"), &p), "{wildcard}");
        }
    }

    #[test]
    fn the_artifact_iframe_host_is_never_allowlisted() {
        // Pins WHY the guard is layered on the `/api` nest and NOT on the
        // top-level router: the artifact iframe is served on
        // `<id>.artifacts.<suffix>`, an unbounded set nobody can list in
        // `[server] hostnames`. If this ever starts returning true, the
        // guard was widened past `/api` and annotations are broken.
        for p in [
            with_hostnames(&[], "127.0.0.1:4000"),
            with_hostnames(&["kb.example.com"], "127.0.0.1:4000"),
        ] {
            assert!(!host_allowed(
                Some("kitchen-sink.artifacts.localhost:4000"),
                &p
            ));
        }
    }

    #[test]
    fn the_gate_applies_to_loopback_always_and_to_others_only_once_configured() {
        // The rebinding victim IS a loopback peer → enforced with zero
        // configuration. The single deliberate fail-open, pinned.
        assert!(host_gate_applies(true, false));
        assert!(host_gate_applies(true, true));
        assert!(!host_gate_applies(false, false));
        assert!(host_gate_applies(false, true));
    }

    /// A miniature `/api` tree with the real middleware stack, driven
    /// through tower's `oneshot` — no socket, no port, no daemon.
    fn guarded_api(cfg: Arc<OriginConfig>) -> Router {
        Router::new()
            .route("/kbs", get(|| async { "kbs" }))
            .route("/kb", axum::routing::delete(|| async { "deleted" }))
            .route(
                "/kb/{kb}/proposals",
                axum::routing::post(|| async { "proposed" }),
            )
            .fallback(|| async { StatusCode::NOT_FOUND })
            // Same order as `build_router`: host_guard is the LAST
            // `.layer()`, hence outermost, so it runs before
            // origin_allowlist — exactly the production wiring.
            .layer(axum::middleware::from_fn_with_state(
                cfg.clone(),
                origin_allowlist,
            ))
            .layer(axum::middleware::from_fn_with_state(cfg, host_guard))
    }

    fn guarded_request(
        peer: &str,
        method: Method,
        uri: &str,
        host: Option<&str>,
        origin: Option<&str>,
    ) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .extension(ConnectInfo(SocketAddr::new(ip(peer), 50000)));
        if let Some(h) = host {
            b = b.header("host", h);
        }
        if let Some(o) = origin {
            b = b.header("origin", o);
        }
        b.body(Body::empty()).unwrap()
    }

    /// THE regression this whole guard exists for: a page at
    /// `http://attacker.example:4000` whose DNS is pointed at 127.0.0.1
    /// connects FROM the box, so the loopback bypass would hand it
    /// operator authority and `Origin == Host` would be satisfied. Every
    /// verb — read, write, delete — must be refused with the
    /// self-diagnosing urn, with NO configuration at all.
    #[tokio::test]
    async fn a_rebound_host_is_refused_on_a_loopback_peer_with_no_config() {
        use axum::body::to_bytes;
        use tower::ServiceExt;
        let cfg = Arc::new(with_hostnames(&[], "127.0.0.1:4000"));
        for (method, uri) in [
            (Method::GET, "/kbs"),
            (Method::POST, "/kb/x/proposals"),
            (Method::DELETE, "/kb"),
        ] {
            let resp = guarded_api(cfg.clone())
                .oneshot(guarded_request(
                    "127.0.0.1",
                    method.clone(),
                    uri,
                    Some("attacker.example:4000"),
                    Some("http://attacker.example:4000"),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {uri}");
            let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            let body = String::from_utf8(body.to_vec()).unwrap();
            assert!(body.contains(ERR_HOST_REFUSED), "{body}");
            // The operator can diagnose it from the response alone.
            assert!(body.contains("[server] hostnames"), "{body}");
        }
    }

    /// A forged `X-Forwarded-For` must not switch the guard OFF.
    ///
    /// The guard and `auth_bearer` both derive "is this a loopback peer"
    /// from `request_is_loopback`, which consults `X-Forwarded-For` when the
    /// peer is trusted. That is correct for the AUTH decision — a real proxy
    /// on the box is trusted to say who it forwards for — but it means a
    /// rebound page can set the header itself: `X-Forwarded-For` is not in
    /// the Fetch spec's forbidden-header list, so
    /// `fetch('/api/kbs', {headers:{'X-Forwarded-For':'203.0.113.9'}})`
    /// arrives looking like a non-loopback client. With `hostnames` empty
    /// that makes `host_gate_applies(false, false)` false, the guard is
    /// skipped, and the pre-fix vulnerability returns on exactly the
    /// `KB_ALLOW_NO_AUTH=1` deployment the carve-out already leaves open.
    ///
    /// So the gate must read the RAW TCP peer, which no header can move.
    #[tokio::test]
    async fn a_forged_x_forwarded_for_does_not_disable_the_guard() {
        use axum::body::to_bytes;
        use tower::ServiceExt;
        let cfg = Arc::new(with_hostnames(&[], "127.0.0.1:4000"));
        let mut req = guarded_request(
            "127.0.0.1",
            Method::GET,
            "/kbs",
            Some("attacker.example:4000"),
            Some("http://attacker.example:4000"),
        );
        // The whole attack: the page names a client that is not loopback.
        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        let resp = guarded_api(cfg).oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "a forged XFF must not switch the Host guard off"
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8(body.to_vec())
            .unwrap()
            .contains(ERR_HOST_REFUSED));
    }

    /// The non-regression that matters more than the hole: the default
    /// install must keep working. A loopback SPA POST — matching `Host`
    /// and same-origin `Origin` — reaches the handler.
    #[tokio::test]
    async fn a_legitimate_spa_post_still_works() {
        use axum::body::to_bytes;
        use tower::ServiceExt;
        let cfg = Arc::new(with_hostnames(&[], "127.0.0.1:4000"));
        let resp = guarded_api(cfg.clone())
            .oneshot(guarded_request(
                "127.0.0.1",
                Method::POST,
                "/kb/kb1/proposals",
                Some("localhost:4000"),
                Some("http://localhost:4000"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"proposed");
    }

    #[tokio::test]
    async fn a_plain_loopback_read_is_admitted_unchanged() {
        use axum::body::to_bytes;
        use tower::ServiceExt;
        let cfg = Arc::new(with_hostnames(&[], "127.0.0.1:4000"));
        for host in ["localhost:4000", "127.0.0.1:4000", "[::1]:4000"] {
            let resp = guarded_api(cfg.clone())
                .oneshot(guarded_request(
                    "127.0.0.1",
                    Method::GET,
                    "/kbs",
                    Some(host),
                    None,
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{host}");
            let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], b"kbs");
        }
    }

    /// A request with no name AT ALL — no `Host` header and no URI
    /// authority, which is an HTTP/1.0 client or an in-process
    /// `tower::oneshot` caller — is admitted, and that is deliberate: see
    /// the reasoning in `host_guard`. "No name" is not a way to present a
    /// name that is not on the list.
    ///
    /// The sibling case, where the name IS present but in the request
    /// URI's authority (HTTP/2 `:authority`, RFC 9113 §8.3.1) and the
    /// guard must still enforce, is `tests/host_guard.rs`.
    #[tokio::test]
    async fn a_request_without_a_host_header_is_admitted() {
        use tower::ServiceExt;
        let cfg = Arc::new(with_hostnames(&["kb.example.com"], "127.0.0.1:4000"));
        let resp = guarded_api(cfg)
            .oneshot(guarded_request(
                "127.0.0.1",
                Method::GET,
                "/kbs",
                None,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_configured_hostname_is_admitted() {
        use axum::body::to_bytes;
        use tower::ServiceExt;
        let cfg = Arc::new(with_hostnames(&["kb.example"], "127.0.0.1:4000"));
        let resp = guarded_api(cfg.clone())
            .oneshot(guarded_request(
                "127.0.0.1",
                Method::GET,
                "/kbs",
                Some("kb.example"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"kbs");
    }

    /// No route oracle: a legitimate caller asking for a path that does
    /// not exist still gets 404, not 403. A 403 on unmatched paths would
    /// let anyone with a rebound name probe which routes exist.
    #[tokio::test]
    async fn an_unmatched_api_path_is_still_404_for_a_legitimate_host() {
        use tower::ServiceExt;
        let cfg = Arc::new(with_hostnames(&[], "127.0.0.1:4000"));
        let resp = guarded_api(cfg.clone())
            .oneshot(guarded_request(
                "127.0.0.1",
                Method::GET,
                "/nope",
                Some("localhost:4000"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// The fail-open boundary, end to end: a NON-loopback peer with no
    /// `hostnames` is admitted on any Host (the reverse-proxy deployment
    /// that already has `auth_bearer` in front of it), and the very same
    /// peer is refused the moment `hostnames` is non-empty.
    #[tokio::test]
    async fn a_non_loopback_peer_is_only_host_checked_once_hostnames_is_set() {
        use tower::ServiceExt;
        let unconfigured = Arc::new(with_hostnames(&[], "172.17.0.1:4000"));
        let resp = guarded_api(unconfigured.clone())
            .oneshot(guarded_request(
                "172.17.0.1",
                Method::GET,
                "/kbs",
                Some("kb.example.com"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "the pinned fail-open");

        // Once `hostnames` is set the gate applies to EVERY peer, so an
        // UNLISTED host is refused from a non-loopback peer too — that is the
        // point of configuring it. The listed host is of course still
        // admitted; asserting otherwise would pin the allowlist to refuse the
        // very name it exists to permit.
        let configured = Arc::new(with_hostnames(&["kb.example.com"], "172.17.0.1:4000"));
        for (host, expected) in [
            ("kb.example.com", StatusCode::OK),
            ("other.example", StatusCode::FORBIDDEN),
        ] {
            let resp = guarded_api(configured.clone())
                .oneshot(guarded_request(
                    "172.17.0.1",
                    Method::GET,
                    "/kbs",
                    Some(host),
                    None,
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), expected, "{host}");
        }
    }
}
