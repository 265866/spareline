# spareline

Spare phone lines on spare phones, with an inbox for each line.

spareline turns old Android phones plugged into a small Linux host, such as a Raspberry Pi, into phone lines you can read from anywhere. Every text each SIM receives lands in that number's inbox in a web app and an HTTP API, and you can see and control each phone's screen from the browser. Nothing is installed on the phones; the host talks to them over adb.

It shows every phone the host's adb server sees, live: model, Android version, battery, temperature, per-SIM service and signal, whether mobile data is off, IMEIs, ICCIDs, and each SIM's number. Each number carries a free-text note, such as what the line is for. Every incoming text is copied from the phones into the database within seconds and is listed per number and in one log across numbers, live. **Screens** shows each phone's live screen with remote control (see below).

Everything but the sign-in page needs an account or an API key. Accounts are made on the server (`bun src/cli.ts users add NAME`, see [Accounts](#accounts)); a signed-in browser keeps an HttpOnly session cookie for 30 days from its last use. The server binds `127.0.0.1` by default; put it behind a reverse proxy that adds HTTPS, such as `tailscale serve` or Caddy, before you reach it from other machines.

## Use it

- Web: `/` on the server (after deploy, see below).
- API docs: `/api/docs` (OpenAPI JSON at `/api/docs/json`).
- Health: `/healthz` returns `{ ok }` without signing in, for container and proxy health checks; `ok` is true when the adb server is reachable. Signed in, `/api/v1/health` adds `{ adbReachable, phonesOnline, phonesOffline, alerts, version }`; `alerts` counts open alerts.

### Texts

Each online phone is checked every 3 s with `dumpsys isms` (about 45 ms); when its inbound-SMS records change, the app reads new rows from the phone's SMS provider (then MMS). The whole inbox is compared with the database when a phone comes online and every 60 s, which catches MMS, texts that arrived while a phone was unplugged, and a phone seen for the first time (its existing inbox is imported). Nothing is installed on the phones, and re-reading a text never stores it twice. An MMS is stored once two reads in a row agree on it, because the phone writes it in steps. MMS pictures are kept as files under `DATA_DIR/attachments/`.

A text belongs to the number of the SIM that received it, looked up by phone and SIM slot at the time it arrived, so moving a SIM keeps its texts on its number. A text from a SIM the app has not identified yet shows as "unassigned SIM" until it is; it then moves to a new `seq` and is published again, so clients following that number see it.

| Endpoint | What |
| --- | --- |
| `GET /api/v1/numbers/{number}/messages?limit=50&before=<seq>` | A number's texts, newest first. `{number}` is E.164 (`+15125550101`), digits, or the number id. |
| `GET /api/v1/numbers/{number}/messages?after=<seq>&wait=30` | Oldest first after `seq`; waits up to `wait` seconds for the next one, then answers `{"messages": [], "cursor": <seq>}`. |
| `GET /api/v1/messages` | The same across all numbers, with a `q` search of the body, subject, sender, number (in any format, such as `(512) 555`), and the number's note. |
| `GET /api/v1/attachments/{id}` | An MMS attachment's bytes. |
| `GET /api/v1/events?types=message` | Server-Sent Events; `id` is the text's `seq`, and `Last-Event-ID` resumes. With `types=device,message`, device events and texts share the stream with `<deviceEventId>:<seq>` ids. Without `types` it carries device events only. |

`seq` is one increasing cursor across all numbers. Every new text is also published in-process as `message.ingested` (`SmsIngest`'s `messages` bus), the hook for forwarding later.

A phone that shows **Needs approval** is waiting for someone to tap **Allow** on "Allow USB debugging?" and tick **Always allow from this computer**. **No USB permission** means the host's udev rules or `plugdev` membership are wrong.

### Screens

**Screens** lists every phone with its state and numbers; nothing streams until you open one. An open phone shows its screen live at native resolution and up to 30 fps, with full control: click or drag to touch, scroll to swipe, type once the screen has focus (paste works), and use **Back**, **Home**, and **Recents**. The phone's own display stays dark while someone watches. Video needs WebCodecs, so use a current Chrome, Edge, or Safari over HTTPS or `localhost`.

The server runs [scrcpy-server](https://github.com/Genymobile/scrcpy) 3.3.3 (`vendor/`) on a phone only while someone watches it, and stops it 30 seconds after the last viewer leaves. scrcpy keeps the phone awake during a session and restores `stay_on_while_plugged_in` afterwards. `GET /api/v1/screens` lists running sessions. Each opened screen is recorded in `control_sessions` with the signed-in user and counts of inputs and typed texts, never the text itself.

### Phone health and rules

Every phone gets health monitoring and alerts: a phone offline for more than 5 minutes, a SIM out of service for more than 30 minutes (cleared after three in-service samples), and a battery above 45 °C (cleared below 43 °C). Health is sampled every `HEALTH_INTERVAL_S` and stored as 5-minute aggregates (`health_5m`), kept 90 days. A SIM change is noticed within seconds (below) and read into the inventory. `GET /api/v1/status` lists the alerts with each phone's live state.

Rules for the phones' settings are optional. Set `POLICY_PROFILE` to a profile file to turn them on; without one, spareline never changes a setting on a phone. Two examples ship in [`profiles/`](profiles/):

- [`no-data.json`](profiles/no-data.json): radios and mobile data stay off, Data Saver on, automatic system updates off, and MMS allowed with data off. A good start for lines that only receive texts.
- [`nuu-b30-no-data.json`](profiles/nuu-b30-no-data.json): the same, tuned for the NUU B30: Google Messages as the default SMS app, a dim screen that sleeps after 15 seconds, a larger radio log buffer (the B30 resets it at every boot), and the preinstalled apps it does not need removed.

A profile is a JSON object; every group is optional and does nothing when left out:

| Key | What it checks or sets |
| --- | --- |
| `radiosOff` | Mobile data and roaming (per SIM), Wi-Fi and its scanning and wake-up, Bluetooth and its scanning, and location stay off. Turned off again on sight. Also turns data off when a SIM changes, presets the next SIMs' data keys, and runs the data watch. `{ "dataThresholdBytes": 1000000 }` sets the data watch's limit (the default). |
| `dataSaver` | `true`: Data Saver on. |
| `smsApp` | The package that must be the default SMS app, such as `com.google.android.apps.messaging`. |
| `removeApps` | Packages removed for the phone's user by **Set up phone**. Removing an app deletes its data; `cmd package install-existing <package>` brings the app back without it. |
| `screen` | `autoBrightness` (true or false), `brightness` (0-255), `timeoutSeconds`, and `stayOnWhileCharging` (true or false). |
| `disableSystemUpdates` | `true`: automatic system updates off. |
| `radioLogBufferMiB` | The radio log buffer is at least this many MiB; set on sight with `logcat -G`, logged quietly as `log_buffer_set`. |
| `mmsWithDataOff` | `true`: **MMS messages** stays on with mobile data off, per SIM. adb cannot set it, so the Phones page says what to tap. |

The server exits at startup if the profile is missing or invalid, naming each problem. With a profile:

- **Drift check**, when a phone comes online and every `POLICY_CHECK_INTERVAL_S`: one batched, read-only `adb shell` (about 0.6-0.75 s on a NUU B30 with its profile) reads every rule and, with `radiosOff`, the phone's network stats. Radio and data drift is turned off on sight and logged as a `drift` event with the value before and after. Anything else raises an alert and waits for **Set up phone**. A failed check is recorded and raises an alert.
- **Set up phone** (the button on the card of a phone that fails a rule only set-up fixes, after a confirmation that lists what it will do, or `POST /api/v1/phones/{serial}/provision`) applies every rule adb can set, then re-reads and verifies; a phone that already complies gets no commands. Without a profile the route answers 409.
- **Data watch** (`radiosOff`): per-app mobile bytes on metered (internet) networks, leaving out the phone process (uid 1001, which carries MMS, IMS, and video calls), above `dataThresholdBytes` per SIM in 24 h turn data off again, log `data_threshold`, and turn the phone card red. That MMS bytes land on uid 1001 is inferred from AOSP.
- **SIM hotplug**: each online phone streams its radio log, filtered to SIM state lines, and polls `gsm.sim.state` every 10 s. On a change the app re-reads `dumpsys isub` and the inventory records the placement and number. With `radiosOff`, a change first turns mobile data off at once (`svc data disable`, `mobile_data 0`); a new SIM gets its own `mobile_data<subId> 0`, a verified check, and a `sim.secured` event. If turning data off fails, a critical alert (`sim_unsecured`) stays, even while the phone is offline, until a check finds data off. On connect and at set-up, the data keys of the next four subscription ids are also written as 0 ahead of time (Android numbers new SIMs in sequence; a guess, and harmless if unused).

On the Phones page, a phone that passes every rule shows **Policy OK**; otherwise its card lists only the rules it fails, each with its current value and, for the settings adb cannot change, what to tap. A check that failed is listed as **Last check failed** with the reason, and a phone none of whose checks has read the rules yet shows **Policy not checked yet**. Without a profile the cards show none of this. While there are alerts the cards do not already show, a banner above the phones lists them, red when any is critical; otherwise there is none.

Whatever the profile, every command the app runs on a phone passes a guard that refuses anything that could turn a radio or data on. The two POST routes accept only requests another site cannot make: `Content-Type: application/json` (an empty `{}` body is fine) or `X-Spareline: 1`, and never `Sec-Fetch-Site: cross-site`.

`bun scripts/policy-check.ts SERIAL PROFILE` prints every rule's state for one phone without changing anything.

### Accounts

Accounts are managed on the server, where the database is:

```sh
bun src/cli.ts users add NAME             # prints a generated password, once
bun src/cli.ts users list
bun src/cli.ts users reset-password NAME  # prints a new one and signs NAME out everywhere
bun src/cli.ts users remove NAME
```

In the container, prefix them with `podman exec spareline`. Anyone signed in can change their own password on the **Settings** page (at least 12 characters); that signs out their other browsers. Passwords are stored as argon2id hashes and sessions as SHA-256 hashes of their tokens. After 10 failed sign-ins for one name in 15 minutes, further attempts for that name are refused for a while. When a session ends (signing out, a password change, `reset-password`, or `remove`), its open event streams and screens close too, at once or, for changes made with the CLI, within 5 seconds. Inventory edits and screen sessions record who made them.

### API keys

Scripts use API keys instead of a password. Create one on the **Settings** page (or `bun src/cli.ts keys add USER NAME read|full`), copy it (it is shown once), and send it as a bearer token:

```sh
curl -H "Authorization: Bearer sl_..." https://HOST/api/v1/numbers/+15125550101/messages?limit=5
```

A **read** key may make GET requests only: texts, attachments, events, the inventory, and the status. A **full** key may also edit, set phones up, and open screens. No key can create keys or change a password, so a leaked key cannot lock you out. Revoking a key on the Settings page (or `bun src/cli.ts keys revoke ID`) stops it at once. Keys are stored as SHA-256 hashes, and edits made with one record the key's name.

Requests that change something must be JSON (or carry `X-Spareline: 1`), so a page on another site cannot make them in a signed-in browser; the session cookie is `SameSite=Strict`, and `Secure` when the request came over HTTPS (directly or with `X-Forwarded-Proto: https` from the proxy).

## Develop

Requires Bun 1.4.2 or later.

```sh
bun install
# Reach the adb server on the host the phones are plugged into (keep it running):
ssh -N -L 15037:127.0.0.1:5037 HOST
ADB_SERVER=127.0.0.1:15037 bun run dev    # http://127.0.0.1:8740
```

`bun run dev` builds the web UI into `dist/web`, rebuilds it when `web/` changes (reload the browser), and restarts the server when server code changes.

| Variable | Default | Meaning |
| --- | --- | --- |
| `ADB_SERVER` | `127.0.0.1:5037` | adb server `host:port` |
| `HOST`, `PORT` | `127.0.0.1`, `8740` | HTTP listen address |
| `DATA_DIR` | `data` | Directory of the SQLite database (`spareline.db`) |
| `SITE` | `local` | Name of this host, stored on each phone |
| `HEALTH_INTERVAL_S` | `60` | Seconds between health probes of each phone |
| `WEB_DIR` | `dist/web` | Built web UI |
| `POLICY_CHECK_INTERVAL_S` | `300` | Seconds between drift checks of each phone |
| `POLICY_PROFILE` | (none) | A policy profile file, such as `profiles/no-data.json`; without one the phones' settings are left alone (see Phone health and rules) |

The server exits at startup if any variable is invalid.

### Checks

```sh
bun run check       # Biome lint and format
bun run typecheck   # tsc --noEmit
bun test            # unit, protocol, database, and API tests
bun run build       # web UI into dist/web
```

Tests run against recordings from the real phone, not invented doubles:

- `test/fixtures/adb/` holds byte transcripts of the adb server (recorded with `scripts/record-adb-fixtures.ts`). The client tests replay them as recorded and one byte at a time.
- `test/fixtures/device/<phone>/` holds probe and SMS provider output (recorded with `scripts/capture-fixtures.ts`, which replaces IMEIs, ICCIDs, numbers, IMSI prefixes, cell ids, text senders, and text bodies with fakes and refuses to write if a real one remains).
- `test/fixtures/screen/` holds a scrcpy video stream from the home screen (recorded with `scripts/record-screen-fixture.ts`, which goes to the home screen first; check the screen holds nothing personal before committing).

Against a real phone, read-only apart from a temporary file:

```sh
SPARELINE_DEVICE_TESTS=1 ADB_SERVER=127.0.0.1:15037 bun test test/device.test.ts
ADB_SERVER=127.0.0.1:15037 bun run smoke
```

To support a new phone model, record its fixtures with `scripts/capture-fixtures.ts SERIAL test/fixtures/device/NAME` and check its `iphonesubinfo` codes in `src/devices/identifiers.ts`.

## Deploy

After CI passes on `main`, GitHub Actions builds `linux/arm64` natively and pushes `ghcr.io/265866/spareline:sha-<commit>`, and moves `:main` to it while that commit is still the head of `main`. On the host, a rootless Podman quadlet runs the image as `spareline.service`, and hourly timers pull and apply new images. [deploy/podman/README.md](deploy/podman/README.md) covers install, updates, backups, and restore.

## Layout

| Path | What |
| --- | --- |
| `src/adb/` | adb server client over TCP: device tracking, `shell,v2`, `exec`, stream services, sync push |
| `src/devices/` | `DeviceManager`, the sole owner of device state; probes and parsers |
| `src/monitor/` | Health aggregates, alerts, SIM hotplug, and the optional policy: profiles, drift check, Provision, data watch |
| `profiles/` | Example policy profiles |
| `src/inventory/` | Phones, SIMs, placements, and numbers (with their notes) in SQLite |
| `src/sms/` | Text ingest (isms trigger, provider queries, reconcile, MMS parts), attribution to numbers, message queries |
| `src/db/` | Connection, numbered migrations, online backup |
| `src/api/` | Elysia routes and the zod schemas that define the API |
| `src/screens/` | scrcpy sessions per phone, the screen WebSocket relay, and its control allow-list |
| `web/` | React SPA |
| `vendor/` | scrcpy-server 3.3.3 jar, its license, and its sha256 |
| `deploy/podman/` | Quadlet, systemd units, and install/update/backup scripts |
