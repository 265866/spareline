#!/usr/bin/env bash
# Runs spareline on a new temporary directory behind the demo's stand-in adb (adb.py), which
# serves three invented phones with invented SIMs, numbers and texts. spareline probes the phones
# and ingests the texts as it would real ones, and nothing leaves the machine.
#
#   bash crates/spareline/demo/run.sh             # serve on http://127.0.0.1:18790 until Ctrl-C
#   bash crates/spareline/demo/run.sh --capture   # rewrite docs/screenshots/*.png, then stop
#
# Options: --port N (18790), --adb-port N (15090), --no-build to use the last builds, and
# --deny a,b to have the capture refuse more strings. It needs cargo, bun and python3, and uv
# for --capture.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
port=18790 adb_port=15090 build=yes capture=no deny=
while (($#)); do
  case $1 in
    --port) port=$2; shift ;;
    --adb-port) adb_port=$2; shift ;;
    --no-build) build=no ;;
    --capture) capture=yes ;;
    --deny) deny=$2; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

if [[ $build == yes ]]; then
  (cd "$root" && cargo build --locked --release -p spareline)
  (cd "$root/crates/spareline/web" && bun install --frozen-lockfile && bun run build)
fi
binary=$root/target/release/spareline
dir=$(mktemp -d "${TMPDIR:-/tmp}/spareline-demo.XXXXXX")
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$dir"
}
trap cleanup EXIT
trap "exit 1" INT TERM

python3 -I "$here/adb.py" "$adb_port" 2>"$dir/adb.log" &
pids+=($!)

# A clean environment, so no setting of the caller's reaches the demo. env execs spareline, so
# the background job's PID is spareline's own and cleanup stops it.
spareline=(env -i PATH="$PATH" HOME="$dir" DATA_DIR="$dir/data" HOST=127.0.0.1 PORT="$port"
  ADB_SERVER="127.0.0.1:$adb_port" SITE=demo HEALTH_INTERVAL_S=5
  SPARELINE_WEB_DIR="$root/crates/spareline/web/dist" "$binary")
mkdir -p "$dir/data"
"${spareline[@]}" users add demo >"$dir/users.log"
sed -n 's/.*Password (shown once): //p' "$dir/users.log" >"$dir/password"
"${spareline[@]}" >"$dir/service.log" 2>&1 &
pids+=($!)

url=http://127.0.0.1:$port
for _ in $(seq 1 60); do curl -fsS -o /dev/null "$url/healthz" 2>/dev/null && break; sleep 0.5; done
jar=$dir/cookies
curl -fsS -o /dev/null -c "$jar" -H 'Content-Type: application/json' \
  --data "{\"name\":\"demo\",\"password\":\"$(cat "$dir/password")\"}" "$url/api/v1/auth/login"
api() { curl -fsS -o /dev/null -b "$jar" -X PATCH -H 'Content-Type: application/json' --data "$2" "$url$1"; }

# Counts what a list endpoint returns, such as "numbers" for /api/v1/numbers.
count() { curl -fsS -b "$jar" "$url$1" | python3 -I -c "import json,sys; print(len(json.load(sys.stdin)['$2']))"; }
# Once spareline has read the SIMs, add the notes a person would type. adb.py then hands over
# the texts, one every 3.5 s, about 50 s in all.
for _ in $(seq 1 60); do [[ $(count /api/v1/numbers numbers) -ge 3 ]] && break; sleep 0.5; done
api /api/v1/numbers/+15125550142 '{"note":"Bike sale listing"}'
api /api/v1/numbers/+15125550177 '{"note":"Shopping and appointments"}'
api /api/v1/numbers/+17375550118 '{"note":"Bakery newsletter sign-ups"}'
for _ in $(seq 1 180); do [[ $(count '/api/v1/messages?limit=100' messages) -ge 13 ]] && break; sleep 0.5; done
[[ $(count '/api/v1/messages?limit=100' messages) -ge 13 ]] || { echo "spareline didn't ingest every demo text" >&2; exit 1; }

if [[ $capture == yes ]]; then
  uv run --script "$here/capture.py" --url "$url" --password-file "$dir/password" \
    --out "$root/docs/screenshots" --deny "$deny"
else
  echo "spareline demo on $url, user demo, password $(cat "$dir/password"). Ctrl-C stops it."
  wait
fi
