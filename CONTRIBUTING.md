# Contributing to spareline

Spare phone lines on spare phones, with an inbox for each line.

This project follows [trellis](https://github.com/265866/trellis): strict lints, a structure
checker, and pull requests that keep rules and code apart. [docs/style.md](docs/style.md) lists
every style rule, what checks it, and the review checklist for the rest; this file holds the
rules no check decides. Both apply to people and coding agents alike.

The owner's work, such as the repository settings and releases, is in
[docs/maintaining.md](docs/maintaining.md). The owner's recipes, such as `just setup-github`,
need admin rights on the repository or the release token, so `just ci` leaves them out.

## Layout

| Path | Holds |
| --- | --- |
| `crates/spareline` | The service: `main.rs` starts it and wires the parts together. No logic that a test would want to call. `spareline --version` prints the version line and exits 0 without starting the service, since CI's `Container image` job runs the image that way. `tests/command_line.rs` runs the built binary and checks it. |
| `crates/spareline_core` | The logic. A crate of its own keeps the binary's command-line and runtime dependencies out of it, lets `just docs` check the doc comments of both crates, and gives `internal_dependencies` its first edge: the binary depends on the logic, never the reverse. |
| `crates/spareline/web` | The web UI, a Bun app. trellis needs only its `check` and `build` scripts, and `build` to write the UI to `dist/`. The image puts that build in `/usr/share/spareline/web`, and `just run` builds it into `dist/`; both name the directory in `SPARELINE_WEB_DIR`, for the service to read when it serves the UI. |

Add a row for each crate you add, in the same pull request as the rule change that allows it.

Every crate is a workspace member one directory below `crates/`, as `crates/<name>`. That is a
rule, because which changes are rules depends on that place. `just policy` fails a member anywhere
else, a nested one included, and a package there that isn't a member. It also fails a crate
directory or `crates` that is a symbolic link, and a crate directory whose name isn't ASCII or
that Windows reads as another name: one that ends in a dot or a space, or holds a `:` or a `\`.
It fails a git submodule, or any other git repository, anywhere in the repository, since no check
sees its files, and two paths anywhere in the repository that macOS or Windows reads as one, such
as two crate directories whose names differ only in letter case. It fails any path with a part of the
form of a Windows short name too, such as `cargo~1.tom` or `demo_c~1`: at most eight characters
that end in `~` and digits, then at most three after a dot. Windows opens `Cargo.toml` under the
first, so a checkout of both there can hold one file, with the other's contents.

## Set up

Install [rustup](https://rustup.rs/), [cargo-binstall](https://github.com/cargo-bins/cargo-binstall),
[uv](https://docs.astral.sh/uv/), [Go](https://go.dev/dl/) for actionlint, and
[shellcheck](https://www.shellcheck.net/) 0.11.0, which actionlint runs on the workflows' scripts.
Install [Bun](https://bun.com/) 1.4.4, the version CI and the image build the web UI with.
Then install just, and use it to install the tools and toolchains the checks use:

```sh
cargo binstall just@1.59.0
just install-tools
just install-toolchains
```

`just install-tools` installs the check tools at the versions CI pins in `.github/workflows/`.
`just install-toolchains` installs the stable toolchain from `rust-toolchain.toml`, the nightly
rustfmt, and the nightly the custom lints build on.

`just web-install` fails when a `node_modules/.bin`, in `crates/spareline/web` or in a
directory above it, holds a `bun` or `bunx` command, and the image's build fails when the web UI's
own `node_modules/.bin` holds one. `bun run` looks there before the Bun you installed, so the web
UI's scripts would run on that Bun instead. In a directory above, delete that `node_modules`. In
the web UI's own, run `bun why bun`, which prints the packages that lead to `bun`. When a package
asks for `bun` only as a peer, an older Bun wrote that into `bun.lock`, so delete `bun.lock` and
`node_modules` and run `bun install` with the Bun you installed, which leaves that peer out. That
also moves every other package to the newest version its range allows, so read the lockfile's diff.
When a package depends on `bun` outright, run `bun remove` with the name of the web UI's own
dependency that leads to it, as `package.json` spells it. That name is `bun`, or its alias, when
the web UI depends on `bun` directly. When `bun why bun` finds no package, the command is usually
left from an earlier install, which Bun keeps, so delete `node_modules`. If the next install brings
the command back, a package other than `bun` provides it. Its entry in `bun.lock` lists the command
under `bin`, and `bun why` with that package's name shows the web UI's own dependency to
`bun remove`.

### Editors

Two options in `rustfmt.toml`, `imports_granularity` and `format_macro_matchers`, exist only in
nightly rustfmt, and stable rustfmt ignores them with a warning. For rust-analyzer, set
`rust-analyzer.rustfmt.extraArgs` to `["+nightly-2026-10-28"]`, so format-on-save matches
`just fmt-check`.

## Check a change

`just ci` runs every check CI runs. `just` alone lists them, so you can run one at a time:

| Recipe | Checks |
| --- | --- |
| `just fmt-check` | Rust and TOML formatting. `just fmt` fixes both. |
| `just typos` | Spelling. |
| `just actionlint`, `just zizmor` | The GitHub workflows, and their security. |
| `just shear` | Every declared dependency is used. |
| `just deny` | Advisories, licenses, banned crates and sources (`deny.toml`), and advisories for the web UI's packages (`just web-audit`). |
| `just links` | Every relative link and anchor in the Markdown files. |
| `just policy` | The structure and docs rules that [docs/style.md](docs/style.md#checked-by-tools) lists. |
| `just lint` | clippy on every target, warnings as errors. |
| `just dylint` | The custom lints from trellis, warnings as errors. |
| `just test` | The tests through nextest, then the doctests. |
| `just docs` | The docs, private items included, warnings as errors. |
| `just build-release` | The release build. |
| `just notices` | The license notices a release ships, in `target/THIRD-PARTY-LICENSES.html`, and the web UI's in `target/THIRD-PARTY-LICENSES-WEB.txt` (`just web-notices`). It fails on a license `about.toml` doesn't accept. |
| `just web-check`, `just web-build` | The web UI's `check` script, and its build. |
| `just rule-changes [base]` | Rules and code change in separate pull requests. |

Before you commit, run `just ci` and fix what fails. Never weaken a check to get it passing: no
lowered lint level, no new exception without a reason that holds up, no skipped or deleted test.
Then review the diff against the [review checklist](docs/style.md#checked-in-review) and the
[Code](#code) section below, and fix what holds up.

### Advisories

The Advisories workflow also runs `just deny` every week. A new advisory can fail a pull request
that changed nothing; fix it with `cargo update -p <crate>`, or for the web UI
`bun update <package>` in its directory, in its own pull request.

## Code

- **Errors.** A failure that a caller can recover from returns a `Result` whose error says what
  failed and with what input. Never panic on user input, I/O or anything over the network; a
  panic is for a bug or a broken invariant.
- **Fail closed.** Input the code doesn't recognize is an error, not a default.
- **Logs.** No secret, token or personal data in a log line or an error message.
- **Tests.** Put a test next to the code it tests, or in the crate's `tests/` directory when it
  uses only the crate's public API. A test is never skipped or retried: `#[ignore]` fails
  `just dylint`, and nextest has no retries. A test of timeouts, expiry, ordering or retries uses
  a clock it controls or explicit timestamps, never a sleep. Fixtures hold made-up data, never real
  names, emails, addresses, keys or tokens.

### Comments

Rust code has no `//` or `/* */` comments except `// SAFETY:` ones; the `code_comment` lint
enforces it, and [docs/style.md](docs/style.md#where-trellis-goes-further-than-the-guide) says why.
Put the why where it is checked or reviewed with the code: a doc comment on the item, the name of
a function or test, an assertion message, or the `reason` of an `#[expect(...)]`. Before you reuse
a workaround you find, even one an `expect` reason explains, check that the problem it works
around still exists. Configuration files (TOML, YAML, the justfile) keep their comments, since
they have nowhere else to say why.

### Silencing a lint

Put an `expect` with a reason on the smallest item the lint applies to:

- a clippy or rustc lint: `#[expect(clippy::lint_name, reason = "...")]`;
- a custom lint: `#[cfg_attr(dylint_lib = "trellis_lints", expect(lint_name, reason = "..."))]`.
  The `cfg_attr` keeps ordinary builds, which don't know the lint, from warning about it.
- private code that only its tests use until its first caller lands:
  `#[cfg_attr(not(test), expect(dead_code, reason = "..."))]`. A plain `expect(dead_code, ..)`
  goes unmet in the test build, where the tests use the code.

Nothing else silences a lint. [docs/style.md](docs/style.md#silencing-a-lint) has the rules for
the reason and the forms that fail, and the lints that take no exception at all.

## New crates

Each crate has one job. `just policy` checks every crate against the rules
[docs/style.md](docs/style.md#checked-by-tools) lists under `trellis-policy check`. When you add
one:

- **Place and manifest.** Put it in `crates/<name>`. Its `[lints]` table is exactly
  `workspace = true`, and it inherits `edition`, `rust-version`, `publish` and `license` from the
  workspace.
- **Settings.** Give it an entry in `internal_dependencies` in `[workspace.metadata.policy]`,
  which lists the workspace crates it may depend on directly, and list it in `changelog_include`
  in `release-plz.toml`, so its commits reach the release notes. A build script needs the crate in
  `build_script_crates`, and a direct dependency on a proc-macro crate needs that crate in
  `proc_macro_crates`. These settings are rules, so a new crate or a new edge between crates is a
  rule change. To rename a crate, follow "Rename a crate" in
  [docs/maintaining.md](docs/maintaining.md#rename-a-crate).

## Docs

The docs keep one shape. Write them to the
[docs review checklist](docs/style.md#docs-review-checklist); `just policy` checks the parts
[a tool can decide](docs/style.md#docs-checked-by-just-policy).

- **Settings.** `docs_files` in `[workspace.metadata.policy]` sets the files `just policy` expects,
  and `readme_sections` the README's `##` sections, in order. trellis starts the README with
  `Run it`, `Documentation` and `License`.
- **Where things go.** Design decisions go in [docs/design.md](docs/design.md), and compatibility
  promises in [docs/stability.md](docs/stability.md). release-plz writes `CHANGELOG.md`; don't edit
  it by hand except as the release steps in
  [docs/maintaining.md](docs/maintaining.md#each-release) say.

## Product boundaries

Settled decisions about what this project does and doesn't do go here, one bullet each, so no one
builds past them. Add one only when the owner decides it.

## Where a rule belongs

The repository is the next contributor's memory: whatever pattern is in it gets copied, a
workaround included. So a rule lives as far up this list as it can, strongest first:

1. **Structure.** Types, crate boundaries and APIs that make the wrong thing impossible to write.
2. **Checks.** A lint level, a cargo-deny rule, a structure rule in `[workspace.metadata.policy]`,
   or a test.
3. **This file.** Guidance for what no check can decide, such as product boundaries.
4. **Review.** The checklist in [docs/style.md](docs/style.md).

When a review finding comes up a second time, move it up. A new custom lint or structure check
belongs in [trellis](https://github.com/265866/trellis), so every project gets it; ask the owner
to add it there. A rule changes only when the owner decides it, in a pull request of its own;
[Send a change](#send-a-change) has the order for a new rule that flags existing code and for a
new proc macro.

## Platforms

`just ci` checks the platform you run it on, and CI checks every platform the project ships for:

- Linux on x86-64, as `x86_64-unknown-linux-gnu`
- Linux on ARM64, as `aarch64-unknown-linux-gnu`

Code for any other operating system fails `just dylint`, and so does code that only some
processors or C libraries build. A change to platform-specific code needs a run on that platform
before it's done.

## Send a change

Keep each pull request to one coherent change. Pull requests are squash-merged, so the title
becomes the commit on `main`. It must be a
[conventional commit](https://www.conventionalcommits.org/), such as `feat(cli): add a --json flag`
or `fix(config)!: reject unknown keys`. A `!` marks a breaking change.

A pull request changes either the rules or the code, never both. Roughly, code is what's under
`crates/` and every `.rs` file; the README, changelog, licenses, `Cargo.lock`, the root `Cargo.toml`
dependency tables, and pages in `docs/` other than `docs/style.md` with their images are neither;
and anything else is a rule. Every `build.rs` is a rule too, except one below a crate's `src/`,
`tests/`, `benches/` or `examples/` when the crate's directory holds no other `Cargo.toml`, in any
letter case, before or after the change. The file a crate's `build` key names is a rule as well, as
is every `.gitignore`, `.ignore`, `bunfig.toml`, `.npmrc`, TypeScript settings file (a `.json` file
whose name starts with `tsconfig`) and git submodule, wherever it sits, and every path Windows reads
as another path: one with a part that ends in a dot or a space, that holds a `:` or a `\`, or
that has the form of a short name.
`just policy` fails a TypeScript settings file whose `extends` or `references` names anything but
another one in the repository, by a path that starts with `./` or `../`, so every file a
`tsconfig.json` reaches is a rule. It also fails one that TypeScript could read another way than
`just policy` does, such as one with a lone carriage return. The root `.gitattributes` may set
only `text`, `-text`, `text=auto` and `eol=lf`, with no attribute macro, so a checkout holds the
bytes the diff showed, apart from line endings; `just policy` fails any other attribute, such as
`working-tree-encoding`, `ident`, `filter` or `binary`. A crate whose directory moves whole,
renamed with no file in it changed, is neither rule nor code, so a crate's directory can move in one
pull request beside the `path` of each dependency on it. A file in the crate that reads its own
location, such as a build script that reads `CARGO_MANIFEST_DIR`, can then do something else, as a
code change can change what a listed build script emits. The web UI's `package.json`, its scripts and its sources are code, outside
the rule split, so a code pull request can change what its `check` script runs, including which
settings file `tsc` reads and the flags it gets.
So an untracked file that fits none of these, such as an `.envrc`, counts as a rule in
`just rule-changes`: commit it in a rules pull request, or list it in `.git/info/exclude`. When a
new rule would flag existing code, either fix the code in one pull request and add the rule in the
next, or add the rule together with an `expect` and a real reason at each place it flags. A
proc-macro dependency is code: list the crate in `proc_macro_crates` in one pull request, then add
the dependency and its first use in the next. Run `just rule-changes` to see how your changes
sort: it counts the rule and code files, and lists both when a change mixes them.

## License

spareline is licensed under either the [Apache License 2.0](LICENSE-APACHE) or the
[MIT License](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in spareline by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or conditions.
