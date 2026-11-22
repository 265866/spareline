#!/usr/bin/env bash
# Pulls UPSTREAM (common.sh, deploy.env) and, when it is a new image, applies it:
# device smoke in the new image, stop, database snapshot, retag :current,
# start, wait for healthy. Any failure after the stop restores the previous
# image and database. --now ignores the maintenance window.
# shellcheck source=common.sh
source "$(dirname "$(realpath "$0")")/common.sh"
[[ $# -eq 0 || ( $# -eq 1 && $1 == --now ) ]] || fail 'Usage: update.sh [--now]'
lock
old_image="$(running_image)"
pull_upstream
# From here on the pulled image is addressed by its content-addressed ID, never
# by the moving tag, which a later push could change mid-update.
new_image="$(image_id "$UPSTREAM")"
digest="$(run podman image inspect --format '{{.Digest}}' "$new_image")"
if [[ "$new_image" == "$old_image" ]]; then log "Image already current ($digest)."; exit 0; fi
if [[ ${1:-} != --now && -n "$MAINTENANCE_HOUR" ]] && (( 10#$(maintenance_date +%H) != 10#$MAINTENANCE_HOUR )); then
  log "Update downloaded; waiting for the $MAINTENANCE_HOUR:00 maintenance window: $digest"
  exit 0
fi

# Device gate: the new image must reach the host adb server and read the
# phones before anything changes. The running service is untouched.
if ! device_smoke "$new_image"; then
  log "New image $digest failed the device smoke; staying on $old_image."
  exit 1
fi

resume_old() {
  local status=$?
  (( status != 0 )) || status=1
  trap - ERR TERM INT
  log "Update preparation failed; restarting the previous image (exit $status)."
  run systemctl --user start "$SERVICE" || log 'The previous service could not restart; manual recovery is required.'
  exit "$status"
}
trap resume_old ERR TERM INT
# Stop writes so the snapshot holds the old version's last transaction.
run systemctl --user stop "$SERVICE"
snapshot="$BACKUPS/preupdate-$(date -u +%Y%m%dT%H%M%SZ).db"
snapshot_db "$old_image" "$snapshot"

# Bash runs no ERR trap inside a trap handler, so every rollback step is
# checked explicitly.
restore_previous() {
  run systemctl --user stop "$SERVICE" || return 1
  if [[ -s "$snapshot" ]]; then
    rm -f -- "$APP/data/$DB-wal" "$APP/data/$DB-shm" || return 1
    install -m 600 "$snapshot" "$APP/data/$DB" || return 1
  fi
  run podman tag "$old_image" "$CURRENT" || return 1
  reset_failed
  run systemctl --user start "$SERVICE" || return 1
  wait_healthy
}
rollback() {
  local status=$?
  (( status != 0 )) || status=1
  trap - ERR TERM INT
  log "Update failed; restoring the previous image and database (exit $status)."
  if restore_previous; then
    log "Rolled back to $old_image."
  else
    run systemctl --user stop "$SERVICE" || true
    log "ROLLBACK FAILED; spareline is stopped and needs manual recovery (snapshot: $snapshot)."
  fi
  exit "$status"
}
trap rollback ERR TERM INT
# The image :previous points to now is dropped once the update succeeds.
retired="$(image_id "$PREVIOUS" 2>/dev/null || true)"
run podman tag "$old_image" "$PREVIOUS"
run podman tag "$new_image" "$CURRENT"
reset_failed
run systemctl --user start "$SERVICE"
wait_healthy
trap 'report_failure "$LINENO"' ERR
trap - TERM INT
keep_newest 'preupdate-*.db' 3
remove_unused_images "$retired"
log "Updated: $old_image -> $new_image ($digest)"
