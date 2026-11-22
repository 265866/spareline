# Run spareline with Podman

Read this page to run spareline on the Linux host the phones plug into, such as a Raspberry Pi 5,
and to keep it updated and backed up.

These files run spareline as two rootless user services:

- `spareline-adb.service`: the adb server (`adb -L tcp:localhost:5037 nodaemon server`). It owns
  USB.
- `spareline.service`: spareline, a rootless Podman quadlet from `localhost/spareline:current`.
  It gets no USB devices and reaches the adb server through `Network=pasta:-T,5037`, which
  forwards only port 5037 to the host's loopback. It publishes `127.0.0.1:8740`, runs read-only
  as uid 10001 with every capability dropped, and is capped at 700 MiB.

Everything lives in `~/services/spareline/`: these scripts, `quadlet/`, `systemd/`, `app.env`
(the service's settings), `data/` (the database), `logs/` and `backups/`.

In the commands below, `HOST` is the SSH name of the machine and `USER` is the account that runs
the services.

## Prerequisites

On the host:

- Podman 5 or later with `passt`, since the quadlet uses `Notify=healthy`, and `adb` from the
  distribution's `android-sdk-platform-tools` or `adb` package.
- Lingering for the account, so its services run without a login:
  `sudo loginctl enable-linger USER`.
- USB access for the account: membership in `plugdev` and the distribution's Android udev rules
  (`android-sdk-platform-tools-common` on Debian and Raspberry Pi OS).
- The memory cgroup controller. On Raspberry Pi OS, add `cgroup_enable=memory` to
  `/boot/firmware/cmdline.txt` and reboot.

The image is public, so pulling it needs no credentials. To run a private image instead, set
`UPSTREAM` in `deploy.env` and store a token with `read:packages` in the persistent auth file,
because Podman's default one under `$XDG_RUNTIME_DIR` is lost at reboot:

```sh
podman login --authfile ~/.config/containers/auth.json ghcr.io
```

## Install

1. From a checkout of this repository, copy this directory to the host, then run the installer
   there:

   ```sh
   ssh HOST 'mkdir -p ~/services/spareline'
   scp -r deploy/podman/. HOST:services/spareline/
   ssh HOST 'bash ~/services/spareline/install.sh && bash ~/services/spareline/verify.sh'
   ```

   `install.sh` is safe to rerun. It copies `app.env.example` to `app.env` if there is none,
   pulls the image if there is no `:current` yet, links the units, starts the adb server, checks
   that the image reaches it, starts spareline, waits for it to be healthy, and enables the
   timers. No phone needs to be plugged in yet. The image it pulls, `:latest`, exists only once
   this repository has published its first release.

2. Plug in the phones with USB debugging on. Each one asks "Allow USB debugging?" for this host's
   new adb key: tick **Always allow from this computer** and tap **Allow**. Updates check the
   phones too (see [Updates](#updates)), so keep at least `SMOKE_MIN_PHONES` of them online, or
   set it to 0 in `deploy.env`.

3. Add an account to sign in with. It prints a generated password once:

   ```sh
   ssh HOST 'podman exec spareline spareline users add NAME'
   ```

   The other account and key commands run the same way, after `podman exec spareline`:

   | Command | Does |
   | --- | --- |
   | `spareline users list` | Lists the users |
   | `spareline users reset-password NAME` | Prints a new generated password once and ends every session |
   | `spareline users remove NAME` | Removes a user with their sessions and keys |
   | `spareline keys add USER KEY_NAME read\|full` | Makes an API key for a user and prints it once. `read` allows `GET` requests only; `full` allows everything else except managing keys and passwords |
   | `spareline keys list` | Lists every key |
   | `spareline keys revoke ID` | Revokes a key, by the ID `keys list` prints |

4. spareline listens only on the host's loopback. Reach it through a reverse proxy that adds
   HTTPS, such as one of these:

   - [Tailscale Serve](https://tailscale.com/kb/1312/serve), to reach it from your tailnet:
     `sudo tailscale serve --bg --https=443 http://127.0.0.1:8740`. Without `sudo`, the account
     must be a Tailscale operator (`sudo tailscale set --operator=USER`).
   - Caddy, nginx or another proxy on the same host, pointed at `http://127.0.0.1:8740`.
   - An SSH tunnel for occasional use: `ssh -N -L 8740:127.0.0.1:8740 HOST`, then open
     `http://127.0.0.1:8740`.

## Settings

`app.env` holds spareline's environment variables. After you change it, restart the service with
`systemctl --user restart spareline.service`.

| Variable | Default | Meaning |
| --- | --- | --- |
| `SITE` | `local` | A name for this host, stored on each phone it sees. Set but empty, it stops the service from starting |
| `POLICY_PROFILE` | none | The phone rules to check and enforce, such as `/app/profiles/no-data.json`. Without one, spareline checks no settings on the phones |
| `POLICY_CHECK_INTERVAL_S` | `300` | Seconds between policy checks of each online phone, from 30 to 86400 |
| `HEALTH_INTERVAL_S` | `60` | Seconds between health readings of each online phone, from 5 to 3600 |

The image sets `HOST`, `PORT`, `DATA_DIR` and `SPARELINE_WEB_DIR`, and the quadlet sets
`ADB_SERVER`; leave them alone.

`deploy.env` changes how updates run. Copy `deploy.env.example` to `deploy.env` to set the image,
a maintenance window or the smoke threshold.

## Updates

`spareline-update.timer` runs `update.sh` hourly. `UPSTREAM` is `ghcr.io/265866/spareline:latest`
unless `deploy.env` sets it, so a host takes every release, the next major version included. Set
`UPSTREAM` to a minor version's tag, such as `ghcr.io/265866/spareline:0.2`, to take only its
patch releases; [Stability](../../docs/stability.md) says what each kind of release may change.
When `UPSTREAM` is a new image, `update.sh` does these steps:

1. It runs `spareline smoke` in the new image against the live adb server. The new image must
   reach adb and read every online phone, at least `SMOKE_MIN_PHONES` of them (1 by default). If
   it fails, nothing changes.
2. It stops the service and takes a database snapshot, `backups/preupdate-*.db`, with
   `spareline backup` from the running image.
3. It tags the old image `:previous` and the new one `:current`, starts the service, and waits
   for the health check.
4. On any failure in steps 2 and 3, it restores the previous image and the snapshot and starts
   the service again.
5. After a successful update, it removes the image that `:previous` pointed to before, and
   dangling spareline images, which were pulled but never applied. It never removes `:current`
   or `:previous`.

By default an update applies as soon as the hourly check finds it. To limit updates to a
maintenance window, set `MAINTENANCE_HOUR` in `deploy.env`.
`~/services/spareline/update.sh --now` ignores the window.

To update the kit itself, copy this directory to the host again, as in step 1 of
[Install](#install), and run `install.sh`. The copy replaces only the kit's own files, so `app.env`,
`deploy.env`, `data/`, `logs/` and `backups/` stay as they are.

## Backups

`spareline-backup.timer` runs `backup.sh` daily at 03:30 host time. It writes an online snapshot
with `spareline backup` (`VACUUM INTO`, then `PRAGMA integrity_check`) to `backups/daily-*.db`,
and a copy of `~/.android/adbkey`. It keeps seven snapshots. The adb key matters: without it,
someone must tap "Allow USB debugging" on every phone again. Treat `backups/` as secret, and copy
it off the host if the host's storage is the only copy.

The snapshots hold the database only. The MMS attachments are files under `data/attachments/`,
which no script copies, so copy that directory off the host along with `backups/`.

Updates keep their three newest `preupdate-*.db` snapshots. Nothing removes `prerestore-*.db`
snapshots, so delete them once you no longer need them.

To restore a snapshot, run `restore.sh`. It saves the current database as
`backups/prerestore-*.db` first:

```sh
~/services/spareline/restore.sh ~/services/spareline/backups/daily-YYYYMMDDTHHMMSSZ.db
```

## Troubleshooting

```sh
systemctl --user status spareline.service spareline-adb.service
journalctl --user -u spareline.service -u spareline-update.service
tail ~/services/spareline/logs/container.log ~/services/spareline/logs/operations.log
curl -s http://127.0.0.1:8740/healthz
adb devices -l
```

## Design notes

- The image has no shell. The scripts pass their arguments to the `spareline` binary, which is
  the image's entry point, and the quadlet's `HealthCmd` is a JSON array, which Podman runs
  without a shell. A plain `HealthCmd` would run under `/bin/sh -c`, fail every time, and roll
  back every update.
- `spareline healthcheck` passes on any `2xx` from `/healthz`. `/healthz` answers 200 with
  `{"ok":false}` while the adb server is down, so a stopped adb server doesn't roll back an
  update.
- Backups and update snapshots hold the database only, not an OCI image archive, to spare SD
  cards. The previous image stays in Podman storage as `localhost/spareline:previous` for
  rollback.
- A new image must pass the device smoke before it replaces the running one.
- The rollback steps are checked one by one. Bash runs no ERR trap inside a trap handler, so a
  nested ERR trap would never report a failed rollback.
