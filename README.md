# spareline

[![CI](https://github.com/265866/spareline/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/265866/spareline/actions/workflows/ci.yml?query=branch%3Amain)
[![Latest release](https://img.shields.io/github/v/release/265866/spareline)](https://github.com/265866/spareline/releases/latest)

Spare phone lines on spare phones, with an inbox for each line.

## Run it

```sh
docker run --rm ghcr.io/265866/spareline:latest
```

[Install](docs/install.md) has the details.

## Documentation

- [Install](docs/install.md): every way to install and update it.
- [Verify a release](docs/verifying-releases.md): check that a download came from this repository.
- [Design](docs/design.md): the goals, the architecture, and the decisions behind them.
- [Stability](docs/stability.md): what a release can change, and what to read before you upgrade.
- [Code style](docs/style.md): which style rules a tool checks, and the review checklist for the rest.
- [Contributing](CONTRIBUTING.md): set up, run the checks, and send a change.
- [Maintain](docs/maintaining.md): the owner's repository settings, releases and trellis updates.
- [Security policy](SECURITY.md): how to report a vulnerability.
- [Changelog](CHANGELOG.md): what changed in each release.

## License

Released under either the [MIT License](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at
your option.
The image holds the licenses of the crates it is built with in
`/usr/share/doc/spareline/THIRD-PARTY-LICENSES.html`.
