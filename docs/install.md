# Container image

Read this page to run spareline on your server. It shows how to run the container image
and how to update it.

The image is `ghcr.io/265866/spareline`, for Linux on x86-64 and Linux on ARM64.
Docker and Podman pull the one for your machine.

Each release pushes three tags, such as these for 1.4.2:

| Tag | Follows |
| --- | --- |
| `v1.4.2`, the version | That release only |
| `1.4`, the minor version | The latest patch release of 1.4 |
| `latest` | The latest release |

## Run the image

To run spareline for good on the host the phones plug into, with hourly updates, daily backups
and a restart after a reboot, follow the README's [Quick start](../README.md#quick-start) instead,
which installs the Podman deploy kit. This section runs the image by hand.

spareline reads the phones through an adb server on the same host, which owns USB, and keeps its
database in `/app/data`. Start the adb server, make a folder for the data, then run the image on
the host's network, so it reaches the adb server, with `HOST=127.0.0.1` so it listens on the host
itself only. With rootless Podman:

```sh
adb start-server
mkdir -p ~/spareline/data
podman run -d --name spareline --restart unless-stopped \
  --network host --userns=keep-id --user "$(id -u):$(id -g)" \
  -e HOST=127.0.0.1 -e ADB_SERVER=127.0.0.1:5037 \
  -v ~/spareline/data:/app/data \
  ghcr.io/265866/spareline:<minor>
podman exec spareline spareline users add NAME
```

Replace `<minor>` with the latest release's minor version, such as `1.4`, so updates within it
arrive when you pull. The last command makes an account and prints its password once. Open
`http://127.0.0.1:8740` on the host, or forward port 8740 to your computer as the
[Quick start](../README.md#quick-start) shows. Check the image first as
[Verify a release](verifying-releases.md) shows.

With Docker, drop `--userns=keep-id`. Keep `--user "$(id -u):$(id -g)"` when the Docker daemon runs
as root, the usual setup, and use `--user 0:0` with rootless Docker, where root in the container is
your own user. Either way the container runs as the owner of `~/spareline/data`.

`adb start-server` lasts until the host restarts, and the container can't reach the phones until
it runs again. The settings spareline reads, such as a policy profile, are listed in
[Settings](../crates/spareline/deploy/podman/README.md#settings); pass each as another `-e`.

## Update

Pull the tag again and recreate the container. Your data stays in `~/spareline/data`:

```sh
podman pull ghcr.io/265866/spareline:<minor>
podman rm -f spareline
```

Then run the image again as above. To roll back, run the previous version's tag, such as `v1.4.1`.
