#!/usr/bin/env bash
# Smoke-test the pinned tmux 3.7c for the kb-term integration lane.
#
# Takes KB_TMUX_BIN (exported into the environment by scripts/ci/build-tmux.sh
# via GITHUB_ENV). Writes JUnit XML to ${1:-tmux-smoke-junit.xml} with
# exactly five <testcase>s in a <testsuite> wrapper: a case that fails gets a
# <failure message=...> child carrying a truncated tail of the case's captured
# output, and the script exits non-zero at the end — never aborting at the
# first failure, so one red case does not hide the rest.
#
# All state lives in a mktemp -d; every tmux invocation uses the private
# socket under it and `-f /dev/null` (so a developer's tmux.conf can never
# change the contract). `kill-server` runs in an EXIT trap.
# Every tmux invocation runs under `timeout 10`, so a single hung command
# fails its case instead of burning the whole job timeout.
set -uo pipefail

OUT_XML="${1:-tmux-smoke-junit.xml}"
: "${KB_TMUX_BIN:?KB_TMUX_BIN is not set (build-tmux.sh must run first)}"

D="$(mktemp -d)"
SOCK="${D}/s.sock"
FAILED=0
FAILURES=0
NAMES=()
RESULTS=()
LOGS=()

cleanup() {
  timeout 10 "${KB_TMUX_BIN}" -S "${SOCK}" -f /dev/null kill-server >/dev/null 2>&1 || true
  rm -rf "${D}"
}
trap cleanup EXIT

tm() {
  timeout 10 "${KB_TMUX_BIN}" -S "${SOCK}" -f /dev/null "$@"
}

record() {
  # $1 = case name; the rest is the case command. The case's stdout/stderr
  # goes to a per-case log; a failing case prints its tail to stderr (the
  # job log) and the tail is also embedded in the JUnit <failure> body.
  local name="$1"; shift
  local log="${D}/case-${name}.log"
  if "$@" >"${log}" 2>&1; then
    RESULTS+=("pass")
  else
    RESULTS+=("fail")
    FAILED=1
    FAILURES=$((FAILURES + 1))
    echo "tmux-smoke: case ${name} failed; tail of its output:" >&2
    tail -n 20 "${log}" >&2 || true
  fi
  LOGS+=("${log}")
  NAMES+=("${name}")
}

case_version() {
  [ "$(timeout 10 "${KB_TMUX_BIN}" -V 2>/dev/null)" = "tmux 3.7c" ]
}

ensure_session() {
  # $1 = session name, $2 = command, $3 = x, $4 = y. A new-session against a
  # missing socket also starts the server, so this doubles as server start.
  tm has-session -t "$1" 2>/dev/null || tm new-session -d -s "$1" -x "$3" -y "$4" "$2"
  tm list-sessions 2>/dev/null | grep -F -- "${1}:" >/dev/null
}

case_holder() {
  ensure_session holder "sleep 300" 120 40
  [ -S "${SOCK}" ] && tm set -g exit-empty off
}

case_conf() {
  ensure_session holder "sleep 300" 120 40 \
    && tm set -g prefix None \
    && tm set -g mouse off \
    && tm set -g window-size manual \
    && tm set -s extended-keys always \
    && tm set -g extended-keys-format csi-u \
    && tm set -g history-limit 50000 \
    && tm set -g status off \
    && tm set -g remain-on-exit on \
    && tm set -g allow-passthrough off \
    && tm resize-window -x 100 -y 30 \
    && [ "$(tm display-message -p '#{window_width}x#{window_height}')" = "100x30" ]
}

case_control() {
  ensure_session tap "sh" 120 40
  pane="$(tm list-panes -t tap -F '#{pane_id}' | head -n 1)"
  fifo="${D}/ctl-in"
  ctl_out="${D}/ctl-out"
  mkfifo "${fifo}"
  : > "${ctl_out}"
  exec 3<>"${fifo}"
  "${KB_TMUX_BIN}" -S "${SOCK}" -f /dev/null -C attach -f ignore-size \
    <"${fifo}" >"${ctl_out}" 2>&1 &
  ctl_pid=$!
  sleep 0.2
  printf 'refresh-client -f pause-after=20\n' >&3
  suffix="$(date +%s%N)-$$"
  marker="KBTERM_MARK_${suffix}"
  printf 'send-keys -t %s "printf '"'"'%%s%%s\\n'"'"' KBTERM_MARK_ %s" Enter\n' \
    "${pane}" "${suffix}" >&3
  # Poll up to 10s; the marker string itself never appears in the echoed
  # command (its two halves are separate shell words), so seeing it proves
  # the pane output reached the tap.
  deadline=$(( $(date +%s) + 10 ))
  ok=1
  while [ "$(date +%s)" -le "${deadline}" ]; do
    if grep -E -e "^%(output|extended-output) ${pane} " "${ctl_out}" 2>/dev/null \
       | grep -F "${marker}" >/dev/null; then
      ok=0
      break
    fi
    sleep 0.3
  done
  if grep -q "%error" "${ctl_out}" 2>/dev/null; then
    ok=1
  fi
  kill "${ctl_pid}" 2>/dev/null || true
  wait "${ctl_pid}" 2>/dev/null || true
  exec 3>&-
  [ "${ok}" = "0" ] || return 1
}

case_kill() {
  tm kill-server >/dev/null 2>&1 || true
  deadline=$(( $(date +%s) + 5 ))
  while [ "$(date +%s)" -le "${deadline}" ]; do
    if [ ! -S "${SOCK}" ] || ! tm list-sessions >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.3
  done
  return 1
}

record version-is-3.7c case_version
record private-socket-holder-starts case_holder
record kb-term-conf-options-accepted case_conf
record control-mode-tap-sees-output case_control
record kill-server-removes-socket case_kill

xml_escape() {
  # Strip control chars invalid in XML 1.0, then escape the markup chars,
  # so the output can sit inside a <failure> body and still parse.
  printf '%s' "$1" | tr -d '[\001-\010\013-\037\177]' \
    | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' \
      -e 's/"/\&quot;/g' -e "s/'/\&apos;/g"
}

{
  printf '<?xml version="1.0" encoding="UTF-8"?>\n'
  printf '<testsuites><testsuite name="tmux-smoke" tests="5" failures="%d">\n' "${FAILURES}"
  for i in 0 1 2 3 4; do
    if [ "${RESULTS[${i}]}" = "pass" ]; then
      printf '  <testcase classname="tmux-smoke" name="%s"/>\n' "${NAMES[${i}]}"
    else
      tail_text="$(tail -n 8 "${LOGS[${i}]}" 2>/dev/null || true)"
      printf '  <testcase classname="tmux-smoke" name="%s"><failure message="%s failed">%s</failure></testcase>\n' \
        "${NAMES[${i}]}" "${NAMES[${i}]}" "$(xml_escape "${tail_text}")"
    fi
  done
  printf '</testsuite></testsuites>\n'
} > "${OUT_XML}"

if [ "${FAILED}" != "0" ]; then
  echo "tmux-smoke: failures present (see ${OUT_XML})" >&2
  exit 1
fi
echo "tmux-smoke: all five cases pass"
