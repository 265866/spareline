#!/usr/bin/env bash
# shellcheck disable=SC2034 # Variables here are used by the scripts that source this file.
# Shared by install.sh, update.sh, backup.sh, restore.sh, and verify.sh.
set -Eeuo pipefail
umask 077

# SPARELINE_APP points the scripts at another directory, for testing them.
APP="$(realpath -m "${SPARELINE_APP:-$HOME/services/spareline}")"
BACKUPS="$APP/backups"
UPSTREAM=ghcr.io/265866/spareline:main
CURRENT=localhost/spareline:current
PREVIOUS=localhost/spareline:previous
CONTAINER=spareline
SERVICE=spareline.service
ADB_SERVICE=spareline-adb.service
TIMERS=(spareline-update.timer spareline-backup.timer)
# Registry credentials, needed only for a private image. The persistent path:
# podman's default under $XDG_RUNTIME_DIR is lost at reboot.
AUTHFILE="$HOME/.config/containers/auth.json"
DB=spareline.db

MAINTENANCE_HOUR=
# Empty uses the host's time zone.
MAINTENANCE_TZ=
SMOKE_MIN_PHONES=1
# deploy.env may override these and UPSTREAM (for example to test a branch image).
# shellcheck source=/dev/null
if [[ -f "$APP/deploy.env" ]]; then source "$APP/deploy.env"; fi
[[ -z "$MAINTENANCE_HOUR" || "$MAINTENANCE_HOUR" =~ ^([01]?[0-9]|2[0-3])$ ]] ||
  { printf 'MAINTENANCE_HOUR must be an hour from 0 to 23, not "%s"\n' "$MAINTENANCE_HOUR" >&2; exit 1; }

fail() { printf '%s\n' "$*" >&2; return 1; }
run() { timeout --foreground --kill-after=15s 15m "$@"; }
# date in MAINTENANCE_TZ; an empty TZ would mean UTC, not the host's zone.
maintenance_date() {
  if [[ -n "$MAINTENANCE_TZ" ]]; then TZ="$MAINTENANCE_TZ" date "$@"; else date "$@"; fi
}
pull_upstream() {
  local auth=()
  if [[ -r "$AUTHFILE" ]]; then auth=(--authfile "$AUTHFILE"); fi
  run podman pull --quiet "${auth[@]}" "$UPSTREAM" >/dev/null
}
log() {
  if [[ -d "$APP/logs" ]]; then
    printf '%s %s\n' "$(date --iso-8601=seconds)" "$*" | tee -a "$APP/logs/operations.log"
  else
    printf '%s\n' "$*" >&2
  fi
}
report_failure() {
  local status=$?
  log "FAILED: ${0##*/} line $1 (exit $status)"
  exit "$status"
}
trap 'report_failure "$LINENO"' ERR

[[ "$(realpath "$(dirname "${BASH_SOURCE[0]}")")" == "$APP" ]] || fail "Install these scripts in $APP before running them."
[[ ! -L "$APP" && ! -L "$BACKUPS" ]] || fail 'The app and backup directories must not be symlinks.'
mkdir -p "$APP/logs" "$APP/data" "$BACKUPS"
chmod 700 "$APP" "$APP/logs" "$APP/data" "$BACKUPS"

lock() { exec 9>"$BACKUPS/.lock"; flock -w 60 9 || fail 'Another backup, update, or restore is running.'; }
image_id() { run podman image inspect --format '{{.Id}}' "$1"; }
running_image() {
  run podman container inspect --format '{{.Image}}' "$CONTAINER" 2>/dev/null || image_id "$CURRENT"
}

# One-off container from an image, without network, as the app's user.
container_run() {
  run podman run --rm --pull=never --network=none --read-only --cap-drop=all \
    --security-opt=no-new-privileges --userns=keep-id:uid=10001,gid=10001 \
    --user=10001:10001 "$@"
}

# Online snapshot of the live database with the image's own backup command
# (VACUUM INTO + integrity_check). Prints nothing when there is no database yet.
snapshot_db() {
  local image=$1 destination=$2
  [[ -s "$APP/data/$DB" ]] || return 0
  container_run -v "$APP/data:/source" -v "$(dirname "$destination"):/backup" "$image" \
    bun src/db/backup.ts "/source/$DB" "/backup/$(basename "$destination")"
  chmod 600 "$destination"
}

check_db() {
  local image=$1 file=$2
  [[ -s "$file" ]] || fail "Missing snapshot: $file"
  container_run -v "$(dirname "$file"):/backup:ro" "$image" bun src/db/backup.ts --check "/backup/$(basename "$file")"
}

# Read-only device check of a new image against the live adb server: it must
# reach adb and read every online phone, at least $2 of them (src/smoke.ts).
device_smoke() {
  local image=$1 min=${2:-$SMOKE_MIN_PHONES}
  run podman run --rm --pull=never --network=pasta:-T,5037 --read-only --cap-drop=all \
    --security-opt=no-new-privileges --userns=keep-id:uid=10001,gid=10001 --user=10001:10001 \
    -e ADB_SERVER=127.0.0.1:5037 -e SMOKE_MIN_PHONES="$min" "$image" bun run smoke
}

# Clears a failed state (and its start limit) before a deliberate start. A unit
# that is not loaded has no state to clear.
reset_failed() { systemctl --user reset-failed "$SERVICE" 2>/dev/null || true; }

link_units() {
  local unit
  mkdir -p "$HOME/.config/containers/systemd" "$HOME/.config/systemd/user"
  ln -sfn "$APP/quadlet/spareline.container" "$HOME/.config/containers/systemd/spareline.container"
  for unit in "$APP/systemd/"*; do ln -sfn "$unit" "$HOME/.config/systemd/user/$(basename "$unit")"; done
  run systemctl --user daemon-reload
}

wait_healthy() {
  local attempt
  for ((attempt = 0; attempt < 45; attempt++)); do
    if run podman healthcheck run "$CONTAINER" >/dev/null 2>&1; then return 0; fi
    sleep 2
  done
  fail 'spareline did not become healthy.'
}

# Removes the given image, by ID, when it is neither :current nor :previous,
# then dangling spareline images (pulled but never applied, such as :main
# images replaced before a maintenance window). Only images built from this
# repository match the label filter, and tagged images are never dangling.
remove_unused_images() {
  local retired=$1 keep
  keep=" $(image_id "$CURRENT") $(image_id "$PREVIOUS" 2>/dev/null || true) "
  if [[ -n "$retired" && "$keep" != *" $retired "* ]]; then
    run podman rmi "$retired" >/dev/null || log "Could not remove the retired image $retired."
  fi
  run podman image prune --force --filter label=org.opencontainers.image.source=https://github.com/265866/spareline >/dev/null ||
    log 'Could not prune dangling spareline images.'
}

# Keeps the newest $2 files matching $1 in the backup directory.
keep_newest() {
  local pattern=$1 keep=$2
  find "$BACKUPS" -maxdepth 1 -type f -name "$pattern" -printf '%f\n' | sort -r | tail -n +"$((keep + 1))" |
    while IFS= read -r name; do rm -f -- "$BACKUPS/$name"; done
}
