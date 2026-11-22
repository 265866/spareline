#!/usr/bin/env bash
# Replaces the live database with a snapshot from backups/ and restarts.
# shellcheck source=common.sh
source "$(dirname "$(realpath "$0")")/common.sh"
[[ $# -eq 1 ]] || fail 'Usage: restore.sh ~/services/spareline/backups/SNAPSHOT.db'
lock
snapshot="$(realpath "$1")"
[[ "$snapshot" == "$BACKUPS/"*.db && -f "$snapshot" && ! -L "$snapshot" ]] || fail 'Restore takes a .db snapshot inside the backup directory.'
image="$(image_id "$CURRENT")"
check_db "$image" "$snapshot"
# Keep what is being replaced.
snapshot_db "$image" "$BACKUPS/prerestore-$(date -u +%Y%m%dT%H%M%SZ).db"
run systemctl --user stop "$SERVICE"
rm -f -- "$APP/data/$DB-wal" "$APP/data/$DB-shm"
install -m 600 "$snapshot" "$APP/data/$DB"
reset_failed
run systemctl --user start "$SERVICE"
wait_healthy
log "Restored the database from ${snapshot##*/}"
