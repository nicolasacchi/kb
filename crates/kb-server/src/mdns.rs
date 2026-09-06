//! v0.4 C1 — opt-in mDNS daemon advertise.
//!
//! When `kb.toml [server] mdns = true`, the daemon broadcasts
//! `_kb._tcp.local.` with a `v=<crate-version>` TXT record so peers on
//! the same LAN can auto-discover. The TUI's daemon picker (D overlay)
//! can supplement its `~/.config/kb/daemons.toml` list with anything
//! it sees over mDNS (consumer side ships in v0.5).
//!
//! Off by default — single-host operators don't want to broadcast
//! their existence, and CI runners don't reliably support multicast.

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::net::IpAddr;

const SERVICE_TYPE: &str = "_kb._tcp.local.";

/// Register the daemon's service info on the multicast bus. The
/// returned `ServiceDaemon` MUST be kept alive for the duration of
/// the server task — drop = deregister + multicast goodbye.
pub fn advertise(daemon_name: &str, listen_addr: std::net::SocketAddr) -> Result<ServiceDaemon> {
    let mdns = ServiceDaemon::new().context("mdns_sd::ServiceDaemon::new")?;
    let host = format!("{daemon_name}.local.");
    let ip: IpAddr = if listen_addr.ip().is_unspecified() {
        // 0.0.0.0 → advertise this host's first non-loopback IP if we
        // can find one; fall back to localhost (still works for
        // same-host discovery).
        local_non_loopback().unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
    } else {
        listen_addr.ip()
    };
    let txt = [("v", env!("CARGO_PKG_VERSION"))];
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        daemon_name,
        &host,
        ip,
        listen_addr.port(),
        &txt[..],
    )
    .context("mdns ServiceInfo::new")?;
    mdns.register(info).context("mdns register")?;
    tracing::info!(
        service_type = SERVICE_TYPE,
        host = %host,
        ip = %ip,
        port = listen_addr.port(),
        "mdns advertise"
    );
    Ok(mdns)
}

fn local_non_loopback() -> Option<IpAddr> {
    // Stdlib doesn't expose interface enumeration; we don't pull in a
    // crate just for the v0.4 mDNS path. When the daemon binds 0.0.0.0,
    // we let mdns-sd's per-interface broadcasting do the right thing
    // and just supply localhost as the "primary" record.
    None
}
