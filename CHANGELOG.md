# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.3](https://github.com/265866/spareline/compare/v0.2.2...v0.2.3) - 2026-11-23

### Other

- *(core)* use reserved example values in two tests ([#42](https://github.com/265866/spareline/pull/42))

## [0.2.2](https://github.com/265866/spareline/compare/v0.2.1...v0.2.2) - 2026-11-22

### Fixed

- *(deploy)* copy the systemd units so disabling a timer keeps it installed ([#37](https://github.com/265866/spareline/pull/37))

## [0.2.1](https://github.com/265866/spareline/compare/v0.2.0...v0.2.1) - 2026-11-22

### Fixed

- *(deploy)* keep a retired image that still has a name, and check each unit on its own ([#32](https://github.com/265866/spareline/pull/32))

### Other

- *(deploy)* move the deploy kit into the spareline crate ([#35](https://github.com/265866/spareline/pull/35))
- *(deploy)* name the kit's paths from the kit itself ([#34](https://github.com/265866/spareline/pull/34))
- *(deploy)* silence the deploy.env source finding with a reason ([#30](https://github.com/265866/spareline/pull/30))

## [0.2.0](https://github.com/265866/spareline/releases/tag/v0.2.0) - 2026-11-22

### Added

- *(server)* serve the HTTP API and run the service ([#22](https://github.com/265866/spareline/pull/22))
- *(server)* relay the phones' screens over WebSockets ([#21](https://github.com/265866/spareline/pull/21))
- *(server)* check, provision and watch the phones' policy ([#20](https://github.com/265866/spareline/pull/20))
- *(server)* track the phones and ingest their texts ([#19](https://github.com/265866/spareline/pull/19))
- *(server)* share the database, the buses, the clock and the event log between tasks ([#18](https://github.com/265866/spareline/pull/18))
- *(image)* add the policy profiles and the scrcpy notice the image ships ([#25](https://github.com/265866/spareline/pull/25))
- *(cli)* add the users, keys, backup and policy-check commands ([#16](https://github.com/265866/spareline/pull/16))
- *(core)* port the monitor's and the screens' decision logic ([#14](https://github.com/265866/spareline/pull/14))
- *(server)* add the HTTP service's building blocks ([#13](https://github.com/265866/spareline/pull/13))
- *(core)* parse what phones print ([#12](https://github.com/265866/spareline/pull/12))
- *(db)* open and keep the Bun service's SQLite database ([#11](https://github.com/265866/spareline/pull/11))
- *(cli)* add the smoke and healthcheck commands the deploy kit runs ([#9](https://github.com/265866/spareline/pull/9))
- *(web)* move the React UI over from the Bun service ([#8](https://github.com/265866/spareline/pull/8))
- *(core)* add the timestamp form the database and the API use ([#5](https://github.com/265866/spareline/pull/5))
- *(adb)* add a client for the adb server's smart-socket protocol ([#4](https://github.com/265866/spareline/pull/4))

### Fixed

- *(db)* word backup failures as the live build does ([#23](https://github.com/265866/spareline/pull/23))

### Other

- *(web)* set the TypeScript options the React UI needs ([#7](https://github.com/265866/spareline/pull/7))
- *(web)* add Bun's type definitions ([#6](https://github.com/265866/spareline/pull/6))
- generate from trellis
