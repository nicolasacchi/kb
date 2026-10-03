# Canary 03 -- invariant #2: `memory_recalls` is registered in the orphan
# sweep. O3 found it absent from all three artifact-id lifecycle registries
# (a delete leaked the rows). It is in CASCADE_STEPS too, so the
# `every_artifact_id_keyed_table_is_in_a_lifecycle_registry` test (which
# accepts EITHER registry) does NOT see this removal; the golden list and the
# behavioural sweep test do, and are the pins named here.
# shellcheck shell=bash
CANARY_DESC="memory_recalls dropped from SWEEP_TABLES"
CANARY_FILES=(crates/kb-core/src/storage/sqlite.rs)
CANARY_NEXTEST=(-p kb-core --lib -E 'test(cascade_cleanup_tables_are_pinned) | test(sweep_reclaims_orphan_capture_recalls_but_spares_live_serves)')
CANARY_MUST_FAIL=(cascade_cleanup_tables_are_pinned sweep_reclaims_orphan_capture_recalls_but_spares_live_serves)
canary_apply() {
  python3 ci/canaries/edit_once.py crates/kb-core/src/storage/sqlite.rs \
    '    (
        "memory_recalls",
        "artifact_id",
        Some("artifact_id NOT LIKE '"'"'served-%'"'"'"),
    ),
' \
    ''
}
