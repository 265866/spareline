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
| `crates/spareline` | The binary: `main.rs` reads the command line and the environment, then starts the service or runs one of its commands, such as `smoke` or `users add`. No logic that a test would want to call. `spareline --version` prints the version line and exits 0 without starting the service, since CI's `Container image` job runs the image that way. `tests/command_line.rs` runs the built binary and checks it. |
| `crates/spareline/image` | The files the image adds, at their paths under its root: `crates/spareline/image/app/data.json` lands at `/app/data.json`. `image` in `.copier-answers.yml` turns them on, and [docs/maintaining.md](docs/maintaining.md#extend-what-trellis-generates) says what else it sets. |
| `crates/spareline/web` | The web UI, a Bun app. trellis needs only its `check` and `build` scripts, and `build` to write the UI to `dist/`. The image puts that build in `/usr/share/spareline/web`, and `just run` builds it into `dist/`; both name the directory in `SPARELINE_WEB_DIR`, for the service to read when it serves the UI. |
| `crates/spareline_core` | Types and pure logic the other crates share, with no I/O: the timestamp form every stored time uses, the parsers of what phones print, the policy rules and the alerts. A crate of its own keeps runtime dependencies out of it and gives `internal_dependencies` its first edge: the binary depends on the logic, never the reverse. `just docs` checks the doc comments of every crate. |
| `crates/spareline_adb` | A client for the adb server's smart-socket protocol: device lists and tracking, shell commands, file pushes, and byte streams to sockets on a phone. It knows nothing about spareline; every connection goes to the adb server, never to a phone directly. |
| `crates/spareline_db` | The SQLite database: the migrations, which open the databases the Bun service this project replaces wrote, unchanged, and the stores for texts, inventory, health, accounts and sign-in sessions. [docs/stability.md](docs/stability.md) says what stays compatible. |
| `crates/spareline_server` | The HTTP service the binary starts: the API and its Server-Sent Events, sign-in, the phone manager that polls adb, text ingest, health monitoring and policy, and the screen sessions. |

Add a row for each crate you add, in the same pull request as the rule change that allows it.

Every crate is a workspace member one directory below `crates/`, as `crates/<name>`. That is a
rule, because which changes are rules depends on that place. `just policy` fails a member anywhere
else, a nested one included, and a package there that isn't a member. It also fails a crate
directory or `crates` that is a symbolic link, and a crate directory whose name isn't ASCII or
that Windows reads as another name: one that ends in a dot or a space, or holds a `:` or a `\`.
It fails a git submodule, or any other git repository, anywhere in the repository, since no check
sees its files, and two paths anywhere in the repository that macOS or Windows reads as one, such as
two crate directories whose names differ only in letter case. It fails any path with a part of the
form of a Windows short name too, such as `cargo~1.tom` or `demo_c~1`: at most eight characters that
end in `~` and digits, then at most three after a dot. Windows opens `Cargo.toml` under the first,
so a checkout of both there can hold one file, with the other's contents.

## Set up

Install these first:

- [rustup](https://rustup.rs/), [cargo-binstall](https://github.com/cargo-bins/cargo-binstall),
  [uv](https://docs.astral.sh/uv/), and [Go](https://go.dev/dl/) for actionlint.
- [shellcheck](https://www.shellcheck.net/) 0.11.0, which actionlint runs on the workflows'
  scripts and `just policy` on a deploy kit's.
- [Bun](https://bun.com/) 1.4.4, the version CI and the image build the web UI with.
- Docker, for `just image`, which builds the container image with `docker build`.

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
directory above it, holds a `bun` or `bunx` command. `bun run` looks there before the Bun you
installed, so the web UI's scripts would run on that Bun instead. In a directory above, delete that
`node_modules`. In the web UI's own, run `bun why bun`, which prints the packages that lead to
`bun`. When a package asks for `bun` only as a peer, an older Bun wrote that into `bun.lock`, so
delete `bun.lock` and `node_modules` and run `bun install --save-text-lockfile` with the Bun you
installed, which leaves that peer out and writes `bun.lock` even when your Bun settings ask for the
binary lockfile. That also moves every other package to the newest version its range allows, so read
the lockfile's diff. When a package depends on `bun` outright, run `bun remove` with the name of the
web UI's own dependency that leads to it, as `package.json` spells it. That name is `bun`, or its
alias, when the web UI depends on `bun` directly. When `bun why bun` finds no package, the command
is usually left from an earlier install, which Bun keeps, so delete `node_modules`. If the next
install brings the command back, a package other than `bun` provides it. Its entry in `bun.lock`
lists the command under `bin`, and `bun why` with that package's name shows the web UI's own
dependency to `bun remove`. The image's build also fails when the web UI's own
`node_modules/.bin` holds such a command.

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
| `just notices` | The license notices a release ships, in `target/THIRD-PARTY-LICENSES.html`, and the web UI's in `target/THIRD-PARTY-LICENSES-WEB.txt` (`just web-notices`). It fails on a license `about.toml` doesn't accept, unless `web-licenses.toml` accepts it for that npm package. |
| `just web-check`, `just web-build` | The web UI's `check` script, and its build. |
| `just image` | The container image, for this machine's architecture. `just ci` leaves it out; [The container image](#the-container-image) says why. |
| `just rule-changes [base]` | Rules and code change in separate pull requests. |

Before you commit, run `just ci` and fix what fails. Never weaken a check to get it passing: no
lowered lint level, no new exception without a reason that holds up, no skipped or deleted test.
Then review the diff against the [review checklist](docs/style.md#checked-in-review) and the
[Code](#code) section below, and fix what holds up.

### Advisories

The Advisories workflow also runs `just deny` every week. A new advisory can fail a pull request
that changed nothing; fix it with `cargo update -p <crate>`, or for the web UI
`bun update <package>` in its directory, in its own pull request. It also runs
`just stale-licenses`, which fails on an entry in `about.toml` for a crate the dependency graph
doesn't hold, and on an entry in `web-licenses.toml` for an npm package the web UI
doesn't install. Unless the code pull request that adds that crate or package is still open,
remove the entry, and a crate's exception in `deny.toml`, in a rules pull request.

### The container image

`just image` builds the image from `Containerfile` with `docker build`, so it needs Docker, and
`just ci` leaves it out. It builds for this machine's architecture only. CI's `Container image`
job and the release build every architecture the image ships for with Docker Buildx, each on
that architecture's base image, and CI's `Run the image` jobs start each on its own architecture.
Don't trust a local `podman build --platform` for another architecture: `Containerfile` pins each
base image by the digest of its index of architectures, and Podman can take the copy it already
pulled for this machine's architecture, so the image holds the wrong architecture's layers.

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
  `workspace = true`. It inherits `version`, `edition`, `rust-version`, `publish` and `license`
  from the workspace. The crates share one version, which release-plz bumps in the root
  `Cargo.toml` alone; trellis doesn't support a crate versioned apart from the others.
- **Settings.** Give it an entry in `internal_dependencies` in `[workspace.metadata.policy]`,
  which lists the workspace crates it may depend on directly, and list it in `changelog_include`
  in `release-plz.toml`, so its commits reach the release notes. A build script needs the crate in
  `build_script_crates`, and a direct dependency on a proc-macro crate needs that crate in
  `proc_macro_crates`. These settings are rules, so a new crate or a new edge between crates is a
  rule change. To rename a crate, follow "Rename a crate" in
  [docs/maintaining.md](docs/maintaining.md#rename-a-crate).
- **Dependencies on other crates.** Name a workspace crate by its `path` alone, as
  `<crate> = { path = "../<crate>" }`, with no `version`: cargo needs one only to publish.
  release-plz rewrites that `version` when a release moves past what it allows, so the release
  pull request would change the crate's `Cargo.toml`, which is code, beside the version in the
  root `Cargo.toml`, a rule, and fail "Rule changes stand alone". `cargo add --path` writes the
  `version`, and `just policy` fails until you remove it.

## Docs

The docs keep one shape. Write them to the
[docs review checklist](docs/style.md#docs-review-checklist); `just policy` checks the parts
[a tool can decide](docs/style.md#docs-checked-by-just-policy).

- **Settings.** `docs_files` in `[workspace.metadata.policy]` sets the files `just policy` expects,
  and `readme_sections` the README's `##` sections, in order. trellis starts the README with
  `Run it`, `Documentation` and `License`.
- **Where things go.** Design decisions go in [docs/design.md](docs/design.md), and compatibility
  promises in [docs/stability.md](docs/stability.md). release-plz writes `CHANGELOG.md`; don't edit
  it by hand except as the release steps in [docs/maintaining.md](docs/maintaining.md#each-release)
  say.

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
as another path: one with a part that ends in a dot or a space, that holds a `:` or a `\`, or that
has the form of a short name. `just policy` fails a TypeScript settings file whose `extends` or
`references` names anything but another one in the repository, by a path that starts with `./` or
`../`, so every file a `tsconfig.json` reaches is a rule. It also fails one that TypeScript could
read another way than `just policy` does, such as one with a lone carriage return. The root
`.gitattributes` may set only `text`, `-text`, `text=auto` and `eol=lf`, with no attribute macro, so
a checkout holds the bytes the diff showed, apart from line endings; `just policy` fails any other
attribute, such as `working-tree-encoding`, `ident`, `filter` or `binary`. A crate whose directory
moves whole, renamed with no file in it changed, is neither rule nor code, so a crate's directory
can move in one pull request beside the `path` of each dependency on it. A file in the crate that
reads its own location, such as a build script that reads `CARGO_MANIFEST_DIR`, can then do
something else, as a code change can change what a listed build script emits. An untracked file that
fits none of these, such as an `.envrc`, counts as a rule in `just rule-changes`: commit it in a
rules pull request, or list it in `.git/info/exclude`.

A root deploy kit that moves into `crates/spareline/deploy/` with every code file unchanged
leaves its code out as a moved crate does, while its files that are rules by their names count at
both paths, so the move lands in a rules pull request beside the paths that name the kit, as
[Maintaining](docs/maintaining.md#extend-what-trellis-generates) says. A code file in the kit that
names `deploy/` or climbs out of it with `..` reaches somewhere else after the move.

The web UI's `package.json`, its scripts and its sources are code, outside the rule split, so a
code pull request can change what its `check` script runs, including which settings file `tsc`
reads and the flags it gets. Its `tsconfig.json` is a rule, and a `types` entry there fails `tsc`
until its `@types` package is installed. So a new `types` entry takes three pull requests: add the
package to `devDependencies` in a code pull request, the entry in a rules pull request, then the
code that uses it in another code pull request.

When a new rule would flag existing code, either fix the code in one pull request and add the rule
in the next, or add the rule together with an `expect` and a real reason at each place it flags. A
proc-macro dependency is code: list the crate in `proc_macro_crates` in one pull request, then add
the dependency and its first use in the next. Run `just rule-changes` to see how your changes
sort: it counts the rule and code files, and lists both when a change mixes them.

A dependency under a license that `deny.toml` and `about.toml` don't accept lands in two pull
requests. First, in a rules pull request, accept the license for that crate alone, with the
reason in a comment: an exception in `deny.toml`, such as `[[licenses.exceptions]]` with
`crate = "webpki-roots"` and `allow = ["CDLA-Permissive-2.0"]`, and an entry in `about.toml`, such
as `[webpki-roots]` with `accepted = ["CDLA-Permissive-2.0"]`. `just deny` and `just notices` pass
with an entry for a crate the dependency graph doesn't hold yet. Then add the dependency in a code
pull request. Accepting the license for every crate, in `allow` or `accepted`, lets any later
dependency use it too, so do that only when the owner decides it.

A dependency that brings in a crate `deny.toml` bans, such as `paste`, fails `just deny` however
deep in the dependency graph the banned crate sits. Letting it in is the owner's decision, and it
lands in two pull requests too. First, in a rules pull request, add the crate that depends on the
banned one directly to the ban's `wrappers`, with the reason in a comment, such as
`wrappers = ["some-crate"]` under `crate = "paste"`. Only that crate may then depend on it, and
`just deny` passes, with a warning, while the dependency graph doesn't hold the wrapper yet. Then
add the dependency in a code pull request.

A framework that lets in `ctor` or `inventory` this way may also have its macros place
constructors, code that runs before `main()`, in this project's own code, which
`code_before_main` reports. GPUI's `actions!` and `#[derive(Action)]` do. Allow them in the same
rules pull request as the `wrappers` entry: list each macro in `constructor_macros` in
`dylint.toml`, by the path the report names, the crate that defines the macro and its name, such as
`gpui::actions` and `gpui_macros::Action`. A listed macro may place any constructor, so list only
the macros the framework needs. A constructor that one of this project's own macros writes stays
reported, even inside a listed macro.

An npm package of the web UI under a license `about.toml` doesn't accept lands the same way.
First, in a rules pull request, accept the license for that package alone with an entry in
`web-licenses.toml`: a `[[package]]` table with `name`, the package's npm name with its scope,
such as `"@fontsource/inter"`, `licenses`, such as `["OFL-1.1"]`, and `reason`. The entry covers
every installed version of the package, and `just notices` passes with an entry for a package
the web UI doesn't install yet. Then add the package in a code pull request. Once the package is
gone, the weekly `just stale-licenses` fails until a rules pull request removes the entry.

A deploy kit, the scripts, container files and systemd units in `crates/spareline/deploy/`
that install and run the project on a host, is code too, like the rest of `crates/`, and so is a kit
in the root `deploy/`, though release-plz releases nothing for a change to that one alone, as
[Maintaining](docs/maintaining.md#extend-what-trellis-generates) says.

A kit change lands with the code change it needs, and review reads it as code. `just policy` runs
shellcheck on each of its scripts, the files that end in `.sh`, `.bash`, `.dash`, `.ksh` or `.bats`,
start with a `#!` line that names `sh`, `bash`, `dash`, `ksh`, `ash` or `bats`, or set their shell
with a `# shellcheck shell=<shell>` line, and on the files they source, in a copy of the kit alone,
so a file outside the kit counts as missing, the crate's other files and the other kit's included. A
library a script sources through a path shellcheck can't follow, with SC1090 or SC1091 silenced, is
checked only if it is a script by itself, so give it a `.sh` name or a `shell=` line, and give every
other script a `#!` line. `just policy` reads no `.shellcheckrc` and no `SHELLCHECK_OPTS`, and
reports no finding in a host file a script sources, such as `/etc/os-release`. Silence a finding
with `# shellcheck disable=SC<code> # <reason>` as close above it as shellcheck takes a directive.
shellcheck applies a directive to the whole command below it, so `just policy` runs it again with
every `disable` taken out and asks shellcheck whether a directive closer to each finding a directive
hid would silence it alone. If one would, such as above a line in the function or block the
directive sits over, or in the file a sourced line reads, the directive fails, though another
directive for the code sits between. Where shellcheck takes no closer one, such as after a `|`, `&&`
or `||`, inside `[[ ]]`, an array or a heredoc, above a `case` pattern or above the `done` that ends
a loop, it passes, so a directive above a `case` covers its pattern lines, one-line bodies included.
A file that sources a kit file by its absolute path in the checkout fails, since shellcheck would
read the checkout, not the copy. It also fails a directive after code on its line or spelled with an
unusual space, one that disables anything but codes, such as `all` or a range, or a parse check
other than SC1090 and SC1091, one without a reason, `shell=` in a file with a `#!` line, `source=`
or `source-path=` that leaves the kit, any key but `disable`, `enable`, `shell`, `source` and
`source-path`, and a symbolic link in the kit. A file in the kit that is a rule by its name stays a
rule: a `justfile` or `*.just` module, an `action.yml`, `.gitignore`, `.ignore`, `bunfig.toml`,
`.npmrc`, `build.rs`, TypeScript settings file, agent instruction file or tool configuration file,
and a path Windows reads as another.

## License

spareline is licensed under either the [Apache License 2.0](LICENSE-APACHE) or the
[MIT License](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in spareline by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or conditions.
