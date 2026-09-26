#!/usr/bin/env bash
# Host cron entrypoint for kb state exports.
#
# Doctor treats <state>/exports/ as stale after 48h, so run at least daily.
# Does not start a daemon. Exits with `kb backup --all`'s status.
#
# Crontab:
#   0 3 * * * /home/nik/project/kb/scripts/kb-backup-cron.sh
#
set -euo pipefail
exec kb backup --all
