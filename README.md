# spareline

Spare phone lines on spare phones, with an inbox for each line.

spareline turns old Android phones plugged into a small Linux host, such as a Raspberry Pi, into phone lines you can read from anywhere. Every text each SIM receives lands in that number's inbox in a web app and an HTTP API, and you can see and control each phone's screen from the browser. Nothing is installed on the phones; the host talks to them over adb.

It shows every phone the host's adb server sees, live: model, Android version, battery, temperature, per-SIM service and signal, whether mobile data is off, IMEIs, ICCIDs, and each SIM's number. Each number carries a free-text note, such as what the line is for. Every incoming text is copied from the phones into the database within seconds and is listed per number and in one log across numbers, live. **Screens** shows each phone's live screen with remote control (see below).

There is no login. Anyone who can reach the server can use the site and the API, so the server binds `127.0.0.1` and should be reached only through a private network, such as `tailscale serve` on a tailnet. When Tailscale Serve passes `Tailscale-User-Login`, inventory edits record it.

## Use it

- Web: `/` on the server (after deploy, see below).
- API docs: `/api/docs` (OpenAPI JSON at `/api/docs/json`).
- Health: `/healthz` returns `{ ok, adbReachable, phonesOnline, phonesOffline, alerts, version }`. `ok` is true when the adb server is reachable; `alerts` counts open alerts.

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

The server runs [scrcpy-server](https://github.com/Genymobile/scrcpy) 3.3.3 (`vendor/`) on a phone only while someone watches it, and stops it 30 seconds after the last viewer leaves. scrcpy keeps the phone awake during a session and restores `stay_on_while_plugged_in` afterwards. `GET /api/v1/screens` lists running sessions. Each opened screen is recorded in `control_sessions` with the Tailscale login and counts of inputs and typed texts, never the text itself.

### Phone safety

The phones must never use mobile data or Wi-Fi. One policy (`src/monitor/policy.ts`) drives both the drift check and **Set up phone**:

- **Drift check**, when a phone comes online and every `POLICY_CHECK_INTERVAL_S`: one batched, read-only `adb shell` (about 0.6-0.75 s on a NUU B30) reads every rule and the phone's network stats. Radio and data drift (mobile data per SIM, roaming, Wi-Fi and its scanning and wake-up, Bluetooth and its scanning, location) is turned off on sight and logged as a `drift` event with the value before and after. The radio log buffer is also set on sight: the B30 ignores Developer options' "Logger buffer sizes" at boot and starts with 256 KiB buffers, so after every reboot the check runs `logcat -b main,system,radio,crash -G 8M` (logged quietly as `log_buffer_set`, no alert). Anything else raises an alert. This is all a phone gets automatically, including a new one. A failed check is recorded and raises an alert.
- **Set up phone** (the button on the card of a phone that is not set up yet, after a confirmation, or `POST /api/v1/phones/{serial}/provision`) applies every rule adb can set: radios and data off, Data Saver on, Google Messages as the default SMS app, screen settings, and removing the apps on the `REMOVE` list. Removing an app deletes its data; `cmd package install-existing` brings the app back without it. It then re-reads and verifies; a phone that already complies gets no commands. One setting cannot be set over adb and is only checked; the Phones page says what to tap: **MMS messages** with data off (per SIM). Messages is deliberately not exempted from Data Saver; see the note in `policy.ts`.
- **Data watch**: per-app mobile bytes on metered (internet) networks, leaving out the phone process (uid 1001, which carries MMS, IMS, and video calls), above `DATA_THRESHOLD_BYTES` per SIM in 24 h turn data off again, log `data_threshold`, and turn the phone card red. That MMS bytes land on uid 1001 is inferred from AOSP.
- **SIM hotplug**: each online phone streams its radio log, filtered to SIM state lines, and polls `gsm.sim.state` every 10 s. Any change turns mobile data off at once (`svc data disable`, `mobile_data 0`), then the app re-reads `dumpsys isub`; a new SIM gets its own `mobile_data<subId> 0`, a verified check, a `sim.secured` event, and an inventory read that records the placement and number. If turning data off fails, a critical alert stays until a check finds data off. On connect and at set-up, the data keys of the next four subscription ids are also written as 0 ahead of time (Android numbers new SIMs in sequence; a guess, and harmless if unused).
- **Alerts**: a phone offline for more than 5 minutes, a SIM out of service for more than 30 minutes (cleared after three in-service samples), a battery above 45 °C (cleared below 43 °C), a failing rule or check, a radio that would not turn off, mobile data above the threshold, and a failed data-off after a SIM change or after data went over the threshold (`sim_unsecured`; it stays, even while the phone is offline, until a later check finds data off). `GET /api/v1/status` lists them with each phone's rule results. Health is sampled every `HEALTH_INTERVAL_S` and stored as 5-minute aggregates (`health_5m`), kept 90 days.

On the Phones page, a phone that passes every rule shows **Safety OK**; otherwise its card lists only the rules it fails, each with its current value and, for the settings adb cannot change, what to tap. A check that failed is listed as **Last check failed** with the reason, and a phone none of whose checks has read the rules yet shows **Safety not checked yet**. While there are alerts the cards do not already show (a phone offline or out of service, a hot battery, a SIM not secured, mobile data over the threshold, a radio still on after the automatic fix), a banner above the phones lists them, red when any is critical; otherwise there is none. **Set up phone** appears only while a phone fails a rule that only set-up fixes (the removed apps, the default SMS app, the screen settings, Data Saver, or automatic updates); a phone that passes those rules shows no button. It comes back if one of those settings drifts, for example when an update reinstalls a removed app.

Every command the monitor runs on a phone passes a guard that refuses anything that could turn a radio or data on. The two POST routes accept only requests another site cannot make: `Content-Type: application/json` (an empty `{}` body is fine) or `X-Spareline: 1`, and never `Sec-Fetch-Site: cross-site`.

`bun scripts/policy-check.ts SERIAL` prints every rule's state for one phone without changing anything.

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
| `DATA_THRESHOLD_BYTES` | `1000000` | Internet mobile data per SIM per 24 h that turns data off again and raises an alert |

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
| `src/monitor/` | Phone policy, drift check, Provision, data watch, SIM hotplug, health aggregates, alerts |
| `src/inventory/` | Phones, SIMs, placements, and numbers (with their notes) in SQLite |
| `src/sms/` | Text ingest (isms trigger, provider queries, reconcile, MMS parts), attribution to numbers, message queries |
| `src/db/` | Connection, numbered migrations, online backup |
| `src/api/` | Elysia routes and the zod schemas that define the API |
| `src/screens/` | scrcpy sessions per phone, the screen WebSocket relay, and its control allow-list |
| `web/` | React SPA |
| `vendor/` | scrcpy-server 3.3.3 jar, its license, and its sha256 |
| `deploy/podman/` | Quadlet, systemd units, and install/update/backup scripts |
