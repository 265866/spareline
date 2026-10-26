# spareline

Android phones on USB to a small Linux host (developed on a Raspberry Pi 5: 2 GB RAM, 16 KB pages). One Bun process, one SQLite database. README.md covers setup and layout; deploy/podman/README.md covers running it on a host.

## Rules

- The phones must never use mobile data or Wi-Fi. Never run a command that enables a radio or data (`svc data enable`, `svc wifi enable`, settings that turn them on). Probes are read-only.
- `src/devices/manager.ts` is the only app code that opens per-phone adb sessions. Everything else uses its `Devices` interface. The exceptions are standalone tools that run outside the server: `src/smoke.ts` (the deploy gate) and `scripts/*` (fixture recording).
- Binder codes for `service call` live only in `src/devices/identifiers.ts`, keyed by SDK and verified by build fingerprint.
- No app auth: a private network is the boundary. Bind `127.0.0.1`; never expose the port publicly (no Tailscale Funnel).
- Real serials, IMEIs, ICCIDs, IMSIs, and phone numbers never go into the repo, fixtures, logs, or screenshots you share. Record fixtures with `scripts/capture-fixtures.ts` and `scripts/record-adb-fixtures.ts`, which replace them.
- Tests use recorded real output (`test/fixtures/`). A stand-in is allowed only for what cannot be produced on demand (a phone being plugged in, a SIM appearing), and says so in a comment.
- Schema changes are new numbered files in `src/db/migrations/`; never edit an applied one.
- The web UI imports only types from `src/` (`import type`).

## Before committing

```sh
bun run check && bun run typecheck && bun test && bun run build
```

Shell scripts in `deploy/podman/` must pass `shellcheck -x -P SCRIPTDIR`.

Commits and PR titles follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat(sms): ...`, `fix: ...`).
