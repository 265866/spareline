#!/usr/bin/env bash
# Daily: an online database snapshot (VACUUM INTO, integrity_check) and the adb
# key. Snapshots are kept for 7 days.
# shellcheck source=common.sh
source "$(dirname "$(realpath "$0")")/common.sh"
[[ $# -eq 0 ]] || fail 'Usage: backup.sh'
lock
image="$(running_image)"
snapshot="$BACKUPS/daily-$(date -u +%Y%m%dT%H%M%SZ).db"
snapshot_db "$image" "$snapshot"
if [[ -s "$snapshot" ]]; then check_db "$image" "$snapshot"; fi
# Losing the adb key means someone on site taps "Allow USB debugging" on every
# phone again. It is a secret, like everything in this directory.
if [[ -s "$HOME/.android/adbkey" ]]; then
  install -m 600 "$HOME/.android/adbkey" "$BACKUPS/adbkey"
  install -m 600 "$HOME/.android/adbkey.pub" "$BACKUPS/adbkey.pub"
fi
keep_newest 'daily-*.db' 7
log "Backup published: ${snapshot##*/}"
