#!/bin/sh
# stubborn.sh: a harness that ignores HUP, INT and TERM forever.
trap '' HUP INT TERM
while :; do sleep 1; done
