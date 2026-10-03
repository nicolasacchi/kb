# Canary 02 -- invariant #3: a request with no `ConnectInfo` peer fails CLOSED
# (not loopback). Treating a missing peer as loopback would hand every
# loopback-only route and the auth bypass to a request whose origin is unknown.
# shellcheck shell=bash
CANARY_DESC="request_is_loopback treats a missing ConnectInfo as loopback"
CANARY_FILES=(crates/kb-server/src/middleware.rs)
CANARY_NEXTEST=(-p kb-server --lib -E 'test(request_is_loopback_no_connect_info_fails_closed)')
CANARY_MUST_FAIL=(request_is_loopback_no_connect_info_fails_closed)
canary_apply() {
  python3 ci/canaries/edit_once.py crates/kb-server/src/middleware.rs \
    '    is_loopback_origin(peer, req.headers(), trusted_proxies)
}' \
    '    if peer.is_none() {
        return true; // CANARY 02: missing ConnectInfo treated as loopback
    }
    is_loopback_origin(peer, req.headers(), trusted_proxies)
}'
}
