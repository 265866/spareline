#!/usr/bin/env bash
# Checks the kit's remove_unused_images with a stand-in podman: an image retired by an update is
# removed only when no name is left on it, so a name an operator added to keep a rollback copy
# keeps that image. Needs bash and coreutils, not Podman or systemd.
#   bash deploy/tests/remove-unused-images.sh
# shellcheck source-path=SCRIPTDIR
set -euo pipefail
kit=$(cd "$(dirname "$0")/../podman" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/bin" "$work/app" "$work/images"
cp -a "$kit/." "$work/app/"
cat >"$work/bin/podman" <<'EOF'
#!/usr/bin/env bash
# A stand-in for podman: images are files under $IMAGES named by ID, holding their names.
printf '%s\n' "$*" >>"$CALLS"
case "$1 $2" in
  "image inspect")
    format=$4 ref=$5
    if [[ -f "$IMAGES/$ref" ]]; then id=$ref; else id=$(grep -lx -- "$ref" "$IMAGES"/* 2>/dev/null | head -n 1 | xargs -r basename); fi
    [[ -n "$id" ]] || exit 125
    case "$format" in
      '{{.Id}}') echo "$id" ;;
      '{{range .RepoTags}}{{.}} {{end}}') tr '\n' ' ' <"$IMAGES/$id"; echo ;;
      *) exit 2 ;;
    esac ;;
  "rmi "*) rm -f "$IMAGES/$2" ;;
  "image prune") ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$work/bin/podman"

failures=0
check() {
  local scenario=$1 candidate=$2 want=$3
  : >"$work/calls"
  (
    export PATH="$work/bin:$PATH" IMAGES="$work/images" CALLS="$work/calls" SPARELINE_APP="$work/app"
    # shellcheck source=../podman/common.sh
    source "$work/app/common.sh"
    remove_unused_images "$candidate"
  ) >"$work/out" 2>&1
  if grep -qx "rmi $candidate" "$work/calls"; then got=removed; else got=kept; fi
  if [[ $got == "$want" ]]; then
    echo "ok: $scenario: $got"
  else
    echo "FAIL: $scenario: want $want, got $got"
    sed 's/^/  /' "$work/out"
    failures=$((failures + 1))
  fi
}

reset_images() {
  rm -f "$work/images"/*
  printf '%s\n' localhost/spareline:current >"$work/images/new0"
  printf '%s\n' localhost/spareline:previous >"$work/images/old0"
}

reset_images
printf '%s\n' localhost/spareline-rollback:previous >"$work/images/prev0"
check "a retired image that still has a rollback name" prev0 kept

reset_images
: >"$work/images/prev0"
check "a retired image with no name left" prev0 removed

reset_images
check "a retired image that is still :previous" old0 kept

[[ $failures -eq 0 ]]
