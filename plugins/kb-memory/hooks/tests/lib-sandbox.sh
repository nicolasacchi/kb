#!/usr/bin/env bash
# Sourced by the hook tests, never executed.
#
# Incident this exists for: a hook test that ran the real `kb` with the
# developer's HOME replayed and deleted their parked capture spool. A landed
# capture makes kb-hook-lib.sh `hook_adapter_land` run
# `kb sessions capture --replay-spool`, the replay deletes every item it lands,
# and the test's EXIT trap then deletes the landing dir.
#
# This helper is containment, not detection: it points every directory kb or a
# hook derives state from at a throwaway tree. The Rust lint
# crates/kb-cli/tests/cli_offline/test_sandbox_lint.rs is what catches a script
# that forgets to call it. PATH and KB_BIN_DIR are left alone on purpose.

# kb_test_sandbox <root> - point every directory kb or a hook derives state
# from at <root>/.kb-test-sandbox and drop the variables that would override
# them. Call it right after the script's TMPROOT + trap, before anything can
# reach a kb. Returns 1 (caller exits) if it cannot.
kb_test_sandbox() {
  local root="${1:-}" d
  [ -n "$root" ] || { echo "kb_test_sandbox: no root dir given" >&2; return 1; }
  root="$root/.kb-test-sandbox"
  for d in home xdg-cache xdg-config xdg-data xdg-state; do
    mkdir -p "$root/$d" || return 1
  done
  export HOME="$root/home" \
    XDG_CACHE_HOME="$root/xdg-cache" XDG_CONFIG_HOME="$root/xdg-config" \
    XDG_DATA_HOME="$root/xdg-data" XDG_STATE_HOME="$root/xdg-state"
  unset KB_HOME KB_CACHE_DIR KB_STATE_DIR KB_CONFIG_DIR \
    KB_CAPTURE_SPOOL KB_CAPTURE_LOCKS KB_SESSIONS_DIR KB_CAPTURE_SEGMENTS \
    CLAUDE_ENV_FILE
}
