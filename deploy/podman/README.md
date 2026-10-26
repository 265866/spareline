# Running spareline with Podman

These files run spareline on a Linux host that the phones plug into, such as a Raspberry Pi 5, as two rootless user services:

- `spareline-adb.service`: the adb server (`adb -L tcp:localhost:5037 nodaemon server`). It owns USB.
- `spareline.service`: the app, a rootless Podman quadlet from `localhost/spareline:current`. It gets no USB devices and reaches the adb server through `Network=pasta:-T,5037`, which forwards only port 5037 to the host's loopback. It publishes `127.0.0.1:8740`, runs read-only as uid 10001 with every capability dropped, and is capped at 700 MB.

Everything lives in `~/services/spareline/`: these scripts, `quadlet/`, `systemd/`, `app.env` (the app's settings), `data/` (the database), `logs/`, and `backups/`.

In the commands below, `HOST` is the SSH name of the machine and `USER` is the account that runs the services.

## Prerequisites

On the host:

- Podman 5 or later with `passt` (the quadlet uses `Notify=healthy`), and `adb` from the distribution's `android-sdk-platform-tools` (or `adb`) package.
- Lingering for the account, so its services run without a login: `sudo loginctl enable-linger USER`.
- USB access for the account: membership in `plugdev` and the distribution's Android udev rules (`android-sdk-platform-tools-common` on Debian and Raspberry Pi OS).
- The memory cgroup controller. On Raspberry Pi OS, add `cgroup_enable=memory` to `/boot/firmware/cmdline.txt` and reboot.

The image is public, so pulling it needs no credentials. To run a private image instead, set `UPSTREAM` in `deploy.env` and store a token with `read:packages` in the persistent auth file, because the default path under `$XDG_RUNTIME_DIR` is lost at reboot:

```sh
podman login --authfile ~/.config/containers/auth.json ghcr.io
```

## Install

From a checkout, copy this directory to the host, then run the installer there:

```sh
ssh HOST 'mkdir -p ~/services/spareline'
scp -r deploy/podman/. HOST:services/spareline/
ssh HOST 'bash ~/services/spareline/install.sh && bash ~/services/spareline/verify.sh'
```

`install.sh` is safe to rerun. It copies `app.env.example` to `app.env` if there is none, pulls the image if there is no `:current` yet, links the units, starts the adb server, checks that the image reaches it, starts the app, waits for it to be healthy, and enables the timers. No phone needs to be plugged in yet.

Then plug in the phones with USB debugging on. Each one asks "Allow USB debugging?" for this host's new adb key: tick **Always allow from this computer** and tap **Allow**. Updates check the phones too (see below), so keep at least `SMOKE_MIN_PHONES` of them online, or set it to 0 in `deploy.env`.

Then create an account to sign in with. It prints a generated password once:

```sh
ssh HOST 'podman exec spareline bun src/cli.ts users add NAME'
```

The app listens only on the host's loopback. Reach it through a reverse proxy that adds HTTPS, for example:

- [Tailscale Serve](https://tailscale.com/kb/1312/serve), to reach it from your tailnet: `sudo tailscale serve --bg --https=443 http://127.0.0.1:8740`. Without `sudo`, the account must be a Tailscale operator (`sudo tailscale set --operator=USER`).
- Caddy, nginx, or another proxy on the same host, pointed at `http://127.0.0.1:8740`.
- An SSH tunnel for occasional use: `ssh -N -L 8740:127.0.0.1:8740 HOST`, then open `http://127.0.0.1:8740`.

## Settings

`app.env` holds the app's environment variables (see the main README). After changing it, restart the app with `systemctl --user restart spareline.service`.

`deploy.env` changes how updates run. Copy `deploy.env.example` to `deploy.env` to set the image, a maintenance window, or the smoke threshold.

## Updates

`spareline-update.timer` runs `update.sh` hourly. When `UPSTREAM` (by default `ghcr.io/265866/spareline:main`) is a new image, it:

1. Runs `bun run smoke` inside the new image against the live adb server. The new image must reach adb and read every online phone (at least `SMOKE_MIN_PHONES`, default 1). If it fails, nothing changes.
2. Stops the service and takes a database snapshot (`backups/preupdate-*.db`).
3. Tags the old image `:previous` and the new one `:current`, starts the service, and waits for the health check.
4. On any failure in steps 2 and 3, it restores the previous image and the snapshot and starts the service again.
5. After a successful update, it removes the image that `:previous` pointed to before, and dangling spareline images (pulled but never applied). `:current` and `:previous` are never removed.

By default an update applies as soon as the hourly check finds it. To limit updates to a maintenance window, set `MAINTENANCE_HOUR` in `deploy.env`. `./update.sh --now` ignores the window.

## Backups

`spareline-backup.timer` runs `backup.sh` daily at 03:30 host time. It writes an online snapshot (`VACUUM INTO`, then `PRAGMA integrity_check`) to `backups/daily-*.db` and a copy of `~/.android/adbkey`. Seven snapshots are kept. The adb key matters: without it, every phone needs someone to tap "Allow USB debugging" again. Treat `backups/` as secret, and copy it off the host if the host's storage is the only copy.

To restore a snapshot (the current database is saved as `backups/prerestore-*.db` first):

```sh
~/services/spareline/restore.sh ~/services/spareline/backups/daily-YYYYMMDDTHHMMSSZ.db
```

## Design notes

- Backups and update snapshots hold the database only, not an OCI image archive, to spare SD cards. The previous image stays in Podman storage as `localhost/spareline:previous` for rollback.
- A new image must pass the device smoke before it replaces the running one.
- Rollback steps are checked one by one. Bash runs no ERR trap inside a trap handler, so a nested ERR trap would never report a failed rollback.

## Troubleshooting

```sh
systemctl --user status spareline.service spareline-adb.service
journalctl --user -u spareline.service -u spareline-update.service
tail ~/services/spareline/logs/container.log ~/services/spareline/logs/operations.log
curl -s http://127.0.0.1:8740/healthz
adb devices -l
```
