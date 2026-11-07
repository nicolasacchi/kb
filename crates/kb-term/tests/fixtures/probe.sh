#!/bin/sh
# probe.sh <output-file> <tmux-bin> <run-dir>
# Records: first-instruction timestamp (ns), $$, and the control-mode
# client flags of its own holder; then execs cat (a stand-in harness).
out=$1
tmux_bin=$2
rundir=$3
{
  date +%s%N
  printf '%s' "$$"
  printf '\n'
  "$tmux_bin" -S "$rundir/s/$KB_TERM_ID.sock" list-clients -F '#{client_control_mode}'
} > "$out"
exec cat
