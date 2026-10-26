# Contributing

Thanks for helping. Bug reports, fixes, and support for more phone models are all welcome.

## Before you open a pull request

1. Install [Bun](https://bun.sh) 1.4.2 or later and run `bun install`.
2. Run the checks CI runs:

   ```sh
   bun run check && bun run typecheck && bun test && bun run build
   bun scripts/check-private.ts
   ```

   Shell scripts in `deploy/podman/` must also pass `shellcheck -x -P SCRIPTDIR -s bash deploy/podman/*.sh`.
3. Write commit subjects and the pull request title as [Conventional Commits](https://www.conventionalcommits.org/), such as `fix(sms): keep an MMS that arrives in two parts`. CI checks both.

[CLAUDE.md](CLAUDE.md) lists the project's rules; the ones that matter most:

- The app never turns a radio or data on. Probes stay read-only.
- Tests run against output recorded from real phones (`test/fixtures/`), not invented doubles.
- Real serials, IMEIs, ICCIDs, IMSIs, and phone numbers never go into the repository. Record fixtures with `scripts/capture-fixtures.ts` and `scripts/record-adb-fixtures.ts`, which replace them, and check every screenshot you attach.

## Supporting a new phone

Record its fixtures with `bun scripts/capture-fixtures.ts SERIAL test/fixtures/device/NAME`, check its `iphonesubinfo` codes in `src/devices/identifiers.ts`, and, if it needs its own settings, add a profile under `profiles/`.

## Security issues

Please report them privately, as [SECURITY.md](SECURITY.md) describes.
