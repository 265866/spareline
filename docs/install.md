# Install spareline

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

```sh
docker run -d --name spareline --restart unless-stopped \
  ghcr.io/265866/spareline:<minor>
```

Replace `<minor>` with the latest release's minor version, such as `1.4`, so updates within it
arrive when you pull.

Podman takes the same arguments. Check the image first as
[Verify a release](verifying-releases.md) shows.

## Update

Pull the tag again and recreate the container:

```sh
docker pull ghcr.io/265866/spareline:<minor>
docker rm -f spareline
```

Then run the image again as above. To roll back, run the previous version's tag, such as `v1.4.1`.
