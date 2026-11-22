# Verify a release

The [release workflow](../.github/workflows/release.yml) builds every release on GitHub Actions
from its tag and attests where it came from. Check that before you run it.

## Find the digest

Each release's notes on GitHub name the image by its digest, such as
`ghcr.io/265866/spareline@sha256:<digest>`. Verify that digest and run it.
A tag such as `latest` or `1.4` can move to another image after you check it; a digest can't.

## Check the signature

With [cosign](https://docs.sigstore.dev/cosign/system_config/installation/):

```sh
cosign verify ghcr.io/265866/spareline@sha256:<digest> \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity https://github.com/265866/spareline/.github/workflows/release.yml@refs/heads/main
```

It fails unless this repository's `release.yml`, running on `main`, signed that image.

## Check the attestation

With the [GitHub CLI](https://cli.github.com/), signed in with `gh auth login`:

```sh
gh attestation verify oci://ghcr.io/265866/spareline@sha256:<digest> \
  --repo 265866/spareline \
  --signer-workflow 265866/spareline/.github/workflows/release.yml \
  --source-ref refs/heads/main
```

The release workflow runs both checks on the digest before it tags the image with its version,
minor and `latest`, so every tag names an image that passed them.

## Run the digest

```sh
docker run --rm ghcr.io/265866/spareline@sha256:<digest>
```
