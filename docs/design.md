# Design

How spareline works and why. Update this page in the same change as the code it
describes.

## Goals

spareline turns old Android phones plugged into a small Linux host into phone lines you can read
from anywhere. Every text each SIM receives lands in that number's inbox, in a web app and an HTTP
API, and each phone's screen can be watched and controlled from the browser. Nothing is installed
on the phones: the host talks to them through its adb server.

This project is a rewrite in Rust of a Bun and TypeScript service. It must replace that service in
place: the same database files, HTTP API, command-line commands, configuration and deploy kit keep
working unchanged, so switching a host needs no data conversion and no change to anything that
depends on it.

The host is small, such as a Raspberry Pi with 2 GB of RAM, and the container is capped at 700 MB.

## Architecture

The binary depends on every crate, and no crate depends on the binary.

| Crate | Owns | May use |
| --- | --- | --- |
| `spareline` | The command line and the environment; starts the service or runs one command. | all four below |
| `spareline_server` | The running service: HTTP API, sign-in guard, Server-Sent Events, the phone manager, text ingest, the monitor and the screen sessions. | adb, core, db |
| `spareline_db` | The SQLite database: migrations, stores, accounts, backups. Synchronous; the server calls it briefly from async code. | core |
| `spareline_adb` | The adb server's wire protocol. One TCP connection per call. | nothing |
| `spareline_core` | Pure logic without I/O: timestamps, parsers of phone output, policy rules, alerts. | nothing |

`spareline_adb` knows nothing about spareline, so its tests replay byte transcripts recorded from a
real adb server and need no phone. `spareline_core` does no I/O, so every parser and rule is tested
against recorded phone output.

## Decisions

- **No new migrations.** The database keeps exactly the three migrations the Bun service wrote.
  The builds before this one know only `001` to `003`, so a host can roll back to one of them and
  keep the current database. A fourth migration would turn every rollback into a restore from a
  snapshot taken before the update, losing what was stored since.
- **Times as text in one form.** Every stored time is the 24-character form JavaScript's
  `toISOString` writes, such as `2026-10-28T08:41:51.104Z`, because queries compare and order the
  strings. `spareline_core::Timestamp` is the only way the code writes one.
