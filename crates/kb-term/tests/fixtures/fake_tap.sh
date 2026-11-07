#!/bin/sh
# fake_tap.sh --session <id> --run-dir <dir>
# A fake control-mode tap: records its attach timestamp, then attaches
# with piped stdin (drained by the runner's supervisor child).
session=
rundir=
while [ $# -gt 0 ]; do
  case "$1" in
    --session) session=$2; shift 2 ;;
    --run-dir) rundir=$2; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s' "$(date +%s%N)" > "$rundir/s/$session.tap_ts"
exec "$KB_TMUX_BIN" -S "$rundir/s/$session.sock" -C attach -f ignore-size -t kb
