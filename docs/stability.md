# Stability

This page says what a release of spareline can change.

Releases are tagged `vX.Y.Z`. Before 1.0, the version numbers mean this:

- A minor release, such as `0.2.0` to `0.3.0`, can add features and break things. It can change
  the configuration the service reads or the interface it serves.
- A patch release, such as `0.2.0` to `0.2.1`, holds fixes and internal changes, such as
  dependency updates. It adds nothing and breaks nothing.

From 1.0, only a major release can break things.

Each release's notes, in [CHANGELOG.md](../CHANGELOG.md) and on the GitHub release, say what
changed and what you must do before you upgrade. Read them before every minor upgrade.

The minor image tag, such as `0.3`, follows that minor version's patch releases only, so pulling it
never moves you to the next minor release. [Install](install.md) lists every tag.

## Names that stay the same

Your scripts, links and the commands in [Install](install.md) and
[Verify a release](verifying-releases.md) depend on these names, so no release changes them:

- The `vX.Y.Z` tag form.
- The release workflow, `.github/workflows/release.yml`, and the `main` branch it runs on.
- The repository, `265866/spareline`.
- The image, `ghcr.io/265866/spareline`.
