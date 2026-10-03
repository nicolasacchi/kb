# Canary 01 -- invariant #4 / SEC-02: `host_guard` on the `/capture` route.
# `/capture` is a corpus WRITE outside the /api nest, so the /api-tree guard
# does not reach it; the dedicated `route_layer(host_guard)` is the only thing
# refusing a DNS-rebound POST. The review (O7) found that deleting it failed
# nothing; tests/host_guard_router.rs now drives the REAL router.
# shellcheck shell=bash
CANARY_DESC="host_guard route_layer removed from /capture"
CANARY_FILES=(crates/kb-server/src/router.rs)
CANARY_NEXTEST=(-p kb-server -E 'binary(host_guard_router) & test(rebound_host_refused_on_capture_through_real_router)')
CANARY_MUST_FAIL=(rebound_host_refused_on_capture_through_real_router)
canary_apply() {
  python3 ci/canaries/edit_once.py crates/kb-server/src/router.rs \
    '        .route_layer(from_fn_with_state(state.origin.clone(), host_guard));' \
    '        ; // CANARY 01: host_guard removed'
}
