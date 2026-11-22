# Maintain spareline

This page is for the repository's owner. It lists the work that needs the owner's rights, in the
order it comes up: setting up the repository, releasing, changing an answer, updating trellis and
renewing the release token.

## Before the first push

The release workflow runs on every push to `main`, so set the repository up before you push.
Commit the generated project on a branch named `main` (`git init --initial-branch main`), then:

1. Create the repository on GitHub as public, as you answered, without pushing:

   ```sh
   gh repo create 265866/spareline --public --source .
   ```

   [Change an answer](#change-an-answer) says what making it private changes.

2. Set the repository up. The recipe needs `gh` signed in as an admin of the repository, and the
   just version the project pins, since an older just can't read the `justfile`. On Windows, run
   it in Git Bash:

   ```sh
   cargo binstall just@1.59.0
   just setup-github
   ```

   It allows squash merging only, with the pull request's title and description as the commit, so
   the title becomes the commit on `main`. It creates the `release` label that release-plz puts on
   its pull request.

   It adds the Actions policy that allows `pull_request_target` for `rule-changes.yml`; without
   it, a public repository doesn't run the rule-change check, and no pull request gets the "Rule
   changes stand alone" status. It turns on private vulnerability reporting, which
   [SECURITY.md](../SECURITY.md) points to.

   Then it protects `main` with a ruleset: no deleting or force-pushing `main`, no change without
   a pull request, and three required checks from GitHub Actions: `PR title`, `CI passed` and the
   "Rule changes stand alone" status. A ruleset can require checks that haven't run yet, so `main`
   is protected from the first push. `CI passed` passes only when every other CI job passes, so
   the required checks stay the same when CI gains or loses a job. `CI passed` also needs
   `Container image`, which builds the image, as `just image` does, and runs it with `--version`,
   so the service must answer `--version` and exit 0.

   Running it again sets the same values and creates nothing twice. `just setup-github --dry-run`
   prints each `gh` call instead of running it.

3. Install the [Renovate app](https://github.com/apps/renovate) on the repository. Each week it
   opens one pull request with the crate updates other than major ones, and one for each major
   update. It does the same for the web UI's packages. The toolchain, tool, action and image pins,
   Bun's included, arrive with trellis updates.

4. Create a [fine-grained token](https://github.com/settings/personal-access-tokens/new) for
   this repository only, with read and write access to contents and to pull requests. A pull
   request that `GITHUB_TOKEN` opens starts no `pull_request_target` workflow, so without the
   token the release pull request never gets the "Rule changes stand alone" status and can't
   merge. Store it as the `RELEASE_PLZ_TOKEN` secret:

   ```sh
   just set-release-token
   ```

   It asks for the token without echoing it, and stores it only once the token passes
   `just check-release-token`. That check reads the repository with the token, which says whether
   the token's account may push, then asks GitHub to create a branch and a pull request from
   requests missing their required fields. A fine-grained token without the permission a request
   needs gets refused before GitHub reads the request, so the answers show the token's own
   permissions, and nothing is created. It also warns when the token expires within 14 days. The
   release workflow runs the same check before it updates the release pull request.

5. Check the settings. `just check-github` reports each one as ok or wrong, and fails if any is
   wrong. It checks that the `RELEASE_PLZ_TOKEN` secret exists, but GitHub never shows it a
   secret's value, so the token's access is left to the check in step 4. A missing `main` ruleset,
   one that isn't active and one that anything may bypass are each wrong.
   On Windows, run it in Git Bash too:

   ```sh
   just check-github
   ```

6. Push `main`:

   ```sh
   git push --set-upstream origin main
   ```

## At the first release

Once the first release has pushed the image, make the `spareline` package public in its
package settings on GitHub, so others can pull the image. GitHub can't make it private again.

## Each release

release-plz keeps a release pull request open with the next version and its `CHANGELOG.md` entry.
Merging it releases that version, with that entry as the release notes;
[release.yml](../.github/workflows/release.yml) does the rest. Before 1.0, a `feat` or a `!` bumps
the minor version and a `fix` the patch version. To ship:

1. To reword an entry, edit `CHANGELOG.md` on the release pull request's branch, as the last step
   before you merge: the next merge to `main` closes the release pull request and opens a new one
   without the edit. To make a release a minor one, change the version on that branch too: in the
   root `Cargo.toml`, in `Cargo.lock` and in the `CHANGELOG.md` heading.
   At the first release, release-plz leaves out each commit that changed only a crate that
   `changelog_include` in `release-plz.toml` lists: it adds those crates' commits only after a
   release tag. It leaves out each commit that changed only a deploy kit in the root `deploy/` at
   every release. On the release pull request's branch, add to the new section of `CHANGELOG.md`
   an entry for each commit that this prints and the section lacks; it also prints commits that
   changed the main crate too, which the section has:
   `git log --format=%s -- crates deploy ':!crates/spareline'`. Every type is left out, not
   only `feat` and `fix`. Put each under `### Added` for a `feat`, `### Fixed` for a `fix`, and
   `### Other` for any other type, such as a `test` or a `refactor`, and write it as release-plz
   writes the others: `feat(core): parse dates (#12)` becomes `- *(core)* parse dates (#12)`. Later
   releases list the crates' commits without the edit, but never the root kit's: add those to the
   release's section of `CHANGELOG.md` on each release pull request's branch by hand, from
   `git log --format=%s <previous release tag>..HEAD -- deploy`. The first release after you add a
   crate comes out as a patch when only that crate changed, and its notes can repeat older entries;
   delete those. Renaming the main crate stops the release workflow with an error that says what to
   do by hand.
2. Under each breaking change, from a commit marked `!` or with a `BREAKING CHANGE:` footer, write
   what users must do before they upgrade. Edit `CHANGELOG.md` on the release pull request's
   branch, as in step 1. [Stability](stability.md) promises users this note.
3. Merge the release pull request once its checks pass. Merge no other pull request until the
   release workflow's **Tag** job finishes. A pull request merged before then can get no release
   pull request until the next push to `main`.
4. If a later job fails, re-run the failed jobs. They pick up from the same tag.

### Keep releases verifiable

Changing a name that [Stability](stability.md#names-that-stay-the-same) lists breaks users'
scripts, links and the commands they copied from Install and Verify a release.

## Rename a crate

To move a crate to another directory, rename `crates/<old>` to `crates/<new>` with `git mv`,
change nothing inside it, and fix the `path` of each dependency on it. `just rule-changes` counts
the moved files as neither rules nor code, so this is a code pull request. Outside `crates/`, only
the crate's row under Layout in `CONTRIBUTING.md` names its directory. Fix that row in a rules
pull request after the move.

Moving `crates/spareline`, which holds the web UI, also changes the root files that name
`crates/spareline/web`: `justfile`, `Containerfile`, `.gitignore` and `.dockerignore`. They
are rules, so that move is a rules pull request, and the Layout rows can go in it.

GitHub's merge doesn't follow a moved directory, though git's does. When the move and another
open pull request that changes any file in the moved crate both branched before either landed,
GitHub refuses to merge whichever of the two lands second, and refuses its "Update branch" too,
with a merge conflict. Update that pull request's branch on your machine and push it:

```sh
git switch <branch>
git fetch origin
git merge origin/main
git push
```

Its diff then holds only its own change, at the crate's new path, and its checks run again.

To rename a crate's package, use three pull requests. A rules pull request adds the new name to
`internal_dependencies` as a key with the old name's list, and beside the old name in each list
of `internal_dependencies` and `restricted_crates` that holds it. It also adds the new name to
`changelog_include` in `release-plz.toml`, and to `build_script_crates` if the crate has a build
script. If the crate is itself a key of `restricted_crates`, the first rules pull request removes
that key, and the last adds it back under the new name. A code pull request then changes the
package's `name`, each dependency on it and each path that names it in code. A last rules pull
request removes the old name's key and the old name from each list. Move the directory and rename
the package in separate pull requests.

This doesn't cover the package `spareline`, which carries the project's name. Rule files
name it too: the root `Cargo.toml`, `dylint.toml`, the release configuration, the justfile, the
Containerfile and the workflows. So renaming it changes rules and code together. To rename it,
follow "Name and kind" under [Change an answer](#change-an-answer).

## Keep a package away from some crates

`restricted_crates` in `[workspace.metadata.policy]` of the root `Cargo.toml` maps a package to
the workspace crates that may depend on it, directly or through any other package. A crate that
must build without a UI toolkit then can't reach one through a new dependency. `just policy` fails
every other crate that reaches the package, with the path. It also fails a key that lists crates
but matches no package, so a misspelled key can't pass while it restricts nothing. An empty list
lets no crate reach the package, and may name one no crate depends on yet, to keep the first
dependency out. Each list names every crate that may reach its package, so where a UI crate
depends on GPUI beside `spareline`, list it too. The setting is a rule, so add or change it
in a rules pull request:

```toml
[workspace.metadata.policy.restricted_crates]
"git+https://github.com/zed-industries/zed" = ["spareline"]
"gpui" = ["spareline"]
"gpui-*" = ["spareline"]
```

A key takes one of three forms:

- **A package name**, such as `gpui`. It matches in any letter case, as crates.io does, and reads
  `-` and `_` as one, as Cargo does.
- **A name prefix** ending in `*`, such as `gpui-*`. It matches the same way, so `gpui-*` also
  covers `gpui_macros`.
- **A source**, starting with `git+`, `registry+` or `sparse+`. It matches every package from the
  same repository however the URL is written. The scheme, a `user@`, a port, a query such as
  `?branch=main`, a fragment, a trailing `/` or `.git`, a percent-escape of a letter, digit, `-`,
  `.`, `_` or `~`, and letter case, in the path too, don't matter. The kind does, so a `git+` key
  never matches a registry's packages. The repository must match whole, so
  `git+https://github.com/zed-industries/zed` doesn't match `zed-fork` or `zed2`. Cargo writes
  every crates.io package's source as `registry+https://github.com/rust-lang/crates.io-index`,
  whichever protocol fetched it.

A crate reaches a package through:

- its own normal, build and dev-dependencies, and the normal and build dependencies of each
  package they bring, workspace crates included. A dependency's own dev-dependencies don't count,
  since they aren't built for the crate.
- every platform's dependencies, such as `[target.'cfg(windows)'.dependencies]`, whichever system
  runs the check.
- features as the whole workspace turns them on. `cargo metadata` resolves one graph, with every
  feature of every workspace crate on and each package's features unified across the workspace.
  So a crate that uses a package without the feature that brings a restricted one still fails
  when another crate turns that feature on, and the check errs toward failing. Move the code
  behind that feature into a crate of its own, so the crate that must stay clear doesn't depend
  on it.

## Accept a license for one npm package

`accepted` in `about.toml` lists the licenses every crate and every npm package of the web UI may
use. When only one package needs another license, such as a font under OFL-1.1, accept it for
that package alone in `web-licenses.toml`, with the reason:

```toml
[[package]]
name = "@fontsource/inter"
licenses = ["OFL-1.1"]
reason = "A font the web UI ships; OFL-1.1 allows bundling it."
```

`name` is the package's npm name, with its scope, matched exactly, and the entry covers every
installed version of it. `just notices` judges each package's license by `accepted` and its
entry together, so `(OFL-1.1 OR GPL-3.0-only) AND MIT` passes with `OFL-1.1` here and `MIT` in
`accepted`. The file is a rule, so as for a crate's license, the entry lands first in a rules pull
request, and the package follows in a code pull request: `just notices` passes an entry for a
package the web UI doesn't install yet. Once the package is gone, `just stale-licenses`, which the
weekly Advisories workflow runs on `main`, fails on the entry until a rules pull request removes
it.

## Extend what trellis generates

Each of these is a place trellis leaves to the project, so you never edit a file it generates.
An update leaves them alone.

- **What the image adds.** The `image` answer, a YAML mapping, adds to the image without an edit
  to `Containerfile`: `env` sets environment defaults, `workdir` the directory the service starts
  in, `expose` the ports it listens on, and `files: true` copies `crates/spareline/image/`
  onto the image's root, such as data files at `crates/spareline/image/app/profiles/` or
  extra notices at `crates/spareline/image/usr/share/doc/spareline/`. Each `env`
  value is a string: quote one that YAML would read as a number, a boolean, a date or null, such as
  `"8080"`, while `0.0.0.0` reads as a string as written. The image stays distroless and runs as its
  `nonroot` user. Its files and working directory belong to root, readable by all and executable by
  none, so a directory the service writes needs a mount. They are data: none may be a link or an ELF
  program or library, and they go under `/usr/share` or a directory of their own, such as `/app`,
  never in one the base image keeps, such as `/usr/lib`, `/etc` or `/tmp`. The binary and trellis's
  license files win over a file at the same path. Those files are code, apart from ones that are
  rules by their names, such as a `.gitignore`. Land them in a code pull request first, since the
  image's build fails without them, then set the answer in a rules pull request:

  ```sh
  uvx copier update --defaults --vcs-ref=:current: \
    --data 'image={env: {HOST: 0.0.0.0, PORT: "8080"}, workdir: /app, expose: [8080], files: true}'
  ```

  The answer replaces the whole mapping, so repeat every key you keep. `just image` builds the
  image as the release does, for this machine's architecture only;
  [CONTRIBUTING.md](../CONTRIBUTING.md#the-container-image) says what it needs.

- **Deploy kit.** Keep the scripts and units that install and run it on a host in
  `crates/spareline/deploy/`. release-plz counts only the commits that change a file in a
  crate's directory, so a commit that changes only a kit there, such as
  `fix(deploy): restart the service after an install`, gets a release of its own and a line in
  its notes. The kit's files are code, and `just policy` runs shellcheck on its scripts; "Send a
  change" in [CONTRIBUTING.md](../CONTRIBUTING.md#send-a-change) says which files there stay
  rules. The image doesn't hold the kit.

  `just policy` checks a kit in the root `deploy/` the same way, but release-plz ignores it: a
  commit that changes only that kit releases nothing, and its fix ships with the next release of
  the crate. Add the fix's line to that release's notes by hand, in `CHANGELOG.md` on the release
  pull request's branch, as [Each release](#each-release) says. Moving the kit is optional, and a
  root kit keeps working. Leave it at the root when a host runs it from a clone of the repository,
  such as a unit that runs `deploy/pull.sh` in the checkout, since the host's first deploy after
  the move would look for the old path. To move the kit into the crate, open one pull request that
  moves it:

  ```sh
  git mv deploy crates/spareline/deploy
  ```

  `just rule-changes` leaves out the kit's code, the files that aren't rules by their names, when
  each keeps its path in the kit, its bytes and its mode. The kit's rule files, such as an
  `action.yml` or a `*.just` module, count as rules at both paths, as a workflow or justfile
  outside the kit that names its path does when it changes, such as a workflow's
  `uses: ./deploy` or a justfile's `import 'deploy/tasks.just'`. Change those paths in the same
  pull request, in the kit's rule files too, and the pull request is a rules change. Fix the
  links to the kit in the README and the pages under `docs/` in the same pull request too, such as
  `[deploy/](../deploy)`: `just links` fails the move otherwise, and those files are neither rules
  nor code, while `CONTRIBUTING.md` and `docs/style.md` are rules. A code file added, removed or
  changed in it makes it no longer a move, so the kit's code counts at both paths and mixes with
  its rules, and so does a code file outside the kit, such as a crate's source that names
  `deploy/`, which goes in a code pull request instead.

  The kit sits two directories deeper after the move, so a kit file that names the old place,
  such as `deploy/install.sh` in a unit, or that climbs out of the kit with `..`, such as
  `cd "$(dirname "$0")/.."`, an `EnvironmentFile=` path through `deploy/../.env` or a link to
  `../docs/` in the kit's own README, reaches somewhere else with the same bytes, and a Markdown
  link out of the kit fails `just links` at the move. Since changing a kit file in the move's pull
  request makes it no longer a move, make those references independent of the kit's place in a
  code pull request first: a script that needs the repository's root finds it with
  `git -C "$(dirname "$0")" rev-parse --show-toplevel` or takes it as an argument rather than
  counting `..`, a command runs from the kit's own directory, and a link out of the kit is a full
  URL. A host file that names a path in the checkout, such as that `EnvironmentFile=`, can't be
  made independent, which is a reason to keep the kit at the root. Crate code that reaches into
  the kit by a relative path, such as `include_str!("../../../deploy/app.service")`, stops
  building at the move, so drop that path in the same code pull request, with any other code
  outside the kit that names it. Then the move is renames alone, beside the links in the docs
  and the rule files that name the kit. It doesn't count as a move when:

  - the root keeps a `deploy` entry, or the kit's code also lands in a second crate;
  - the root or `crates/` holds an entry that a case-insensitive file system reads as the same
    name, or the crate's name isn't plain ASCII;
  - the crate already held a `deploy` entry before the move;
  - the kit holds a symbolic link, a submodule or a `Cargo.toml`, or a member's `build` key names
    a file in it;
  - git checks its code out differently at the new place, or a `.gitattributes` anywhere is
    spelled in other letters;
  - in a run on the working tree, either place, or a `.gitattributes` anywhere, has an
    uncommitted change.

## Change an answer

To change an answer, keep the trellis version and pass the new value, such as the license:

```sh
uvx copier update --defaults --vcs-ref=:current: --data 'license=MIT OR Apache-2.0'
```

or the processors the image runs on, `x86_64` and `aarch64` (ARM64):

```sh
uvx copier update --defaults --vcs-ref=:current: --data 'architectures=[x86_64]'
```

The change is a rule change, so it can't also change code. Once it merges, run `just check-github`
on `main`, and `just setup-github` if it reports a wrong setting. Changing the architectures needs
nothing on GitHub, since `CI passed` covers whatever platforms CI runs. Some answers also change
what GitHub needs:

- **Visibility.** A private repository costs more to run and lacks some of what a public one has.
  Protecting its `main` needs GitHub Pro or a paid organization plan, and its CI draws on the
  account's included Actions usage.
  [Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions) has
  GitHub's rates. Its image is neither attested nor signed, because without GitHub Enterprise Cloud,
  GitHub attests images only in public repositories, and a keyless signature would name the
  repository in Sigstore's public log. GitHub takes private vulnerability reports only in public
  repositories, so a private repository's `SECURITY.md` sends reports to issues.

  To make the repository private, first merge the change that
  `uvx copier update --defaults --vcs-ref=:current: --data visibility=private` makes, then make the
  repository private on GitHub and run `just check-github` on `main`. The Actions policy for the
  rule-change check then no longer matters. The update keeps building for ARM64, whose runners draw
  on a private repository's Actions usage. To stop, add `--data 'architectures=[x86_64]'` to it. The
  image's package stays public, because GitHub can't make a public package private again. Delete it
  in its package settings before the next release, which then publishes a new, private package.

An update writes nothing into `crates/` apart from a published crate's `LICENSE` files, which it
adds or deletes as publishing or the license changes. So the answers that the crates' own files
hold need more than the update, and two can't change this way at all:

- **Name and kind.** An update records a new `project_name` or `kind` and rewrites the files
  outside `crates/` for it, but the crates keep their names and scaffold, so `just policy` fails,
  or passes on crates the new answer doesn't describe. To rename the project or change what it
  ships, generate a new project and move the code into it.
- **Description.** The update changes `README.md`, `CONTRIBUTING.md` and the image's description
  label in `Containerfile`. Each crate's `description` in its `Cargo.toml` and its `//!` crate docs
  keep the old sentence; change them in a code pull request.
- **License.** Between `MIT` and `MIT OR Apache-2.0`, the update is all it takes. To change it
  to `none`, also remove `license.workspace = true` from every crate's `Cargo.toml`, in the
  update's pull request; Cargo can't load the workspace until each crate's line is gone.
  A crate's license line is a rule setting, so the pull request stays a rule change.
- **Web UI.** Turning `web_ui` off leaves `crates/spareline/web` in place, and
  `.gitignore` no longer ignores its `node_modules/` and `dist/`. Delete those two directories
  before you run the checks, or `just policy` fails on the links in `node_modules/.bin` and
  `just rule-changes` counts every file in them. The app's `tsconfig.json`, and any
  `.gitignore`, `bunfig.toml` or `.npmrc` in it, are rules: delete them in the update's pull
  request, and the rest of the app in a code pull request after it.

The other answers change only files the update writes.

## Update trellis

Renovate opens a pull request in its weekly run after trellis tags a new version. Before you merge
it, read that version's section in trellis's
[CHANGELOG.md](https://github.com/265866/trellis/blob/main/CHANGELOG.md): its "Action required in
projects" part says what to do before or after the update merges. To update by hand:

```sh
uvx copier update --defaults
```

An update never touches `crates/`, `Cargo.lock`, `CHANGELOG.md` or `docs/design.md`, which are
yours after the project is generated. It is a rule change, so it can't also change code. Copier
merges the new template into your edits to the files it renders, such as a crate's row in
`CONTRIBUTING.md`, its settings in `Cargo.toml` and `release-plz.toml`, and a page's line in
`README.md`. Where trellis changed a line you changed, or one next to it, Copier leaves conflict
markers; resolve them before you commit. Once the update merges, run `just check-github` on
`main`. A newer trellis can set more than the one you set up with, so if it reports a wrong
setting, run `just setup-github`.

An update that moves the base images' digests in the `Containerfile`, such as for a security fix,
publishes no new image by itself. The release workflow builds the image only when it tags a
release, and release-plz opens a release pull request only when a file of one of the crates
changes; the `Containerfile` is in none of them. The new base images ship with the next release.

## Renew the release token

A fine-grained token expires on the date you set when you created it. In its last 14 days, the
release workflow's **Update the release PR** job warns on every run. Before it expires, create a
new one with the same access and replace the secret:

```sh
just set-release-token
```

Until you do, the **Update the release PR** job fails at its token check, which names what the
token lacks, and the release pull request stops updating.
