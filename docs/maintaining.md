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
   the required checks stay the same when CI gains or loses a job.
   `CI passed` also needs `Container image`, which builds the image, as `just image` does, and runs
   it with `--version`, so the service must answer `--version` and exit 0.

   Running it again sets the same values and creates nothing twice. `just setup-github --dry-run`
   prints each `gh` call instead of running it.

3. Install the [Renovate app](https://github.com/apps/renovate) on the repository. Each week it
   opens one pull request with the crate updates other than major ones, and one for each major
   update. It does the same for the web UI's packages. The toolchain, tool, action
   and image pins, Bun's included, arrive with trellis updates.

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
   one that isn't active and one that anything may bypass
   are each wrong.
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
   release tag. On the release pull request's branch, add to the new section of `CHANGELOG.md` an
   entry for each commit that this prints and the section lacks; it also prints commits that
   changed the main crate too, which the section has:
   `git log --format=%s -- crates ':!crates/spareline'`. Every type is left out, not only
   `feat` and `fix`. Put each under `### Added` for a `feat`, `### Fixed` for a `fix`, and
   `### Other` for any other type, such as a `test` or a `refactor`, and write it as release-plz
   writes the others: `feat(core): parse dates (#12)` becomes `- *(core)* parse dates (#12)`.
   Later releases list those commits without the edit.
   The first release after you add a crate comes out as a patch when only that crate changed, and
   its notes can repeat older entries; delete those. Renaming the main crate stops the release
   workflow with an error that says what to do by hand.
2. Under each breaking change, from a commit marked `!` or with a `BREAKING CHANGE:` footer, write
   what users must do before they upgrade. Edit `CHANGELOG.md` on the release pull request's
   branch, as in step 1. [Stability](stability.md) promises users this note.
3. Merge the release pull request once its checks pass. Merge no other pull request until the
   release workflow's **Tag** job finishes. A pull
   request merged before then can get no release pull request until the next push to `main`.
4. If a later job fails, re-run the failed jobs. They pick up from the same tag.

### Keep releases verifiable

Changing a name that [Stability](stability.md#names-that-stay-the-same) lists breaks users'
scripts, links and the commands they copied from Install and Verify a release.

## Rename a crate

To move a crate to another directory, rename `crates/<old>` to `crates/<new>` with `git mv`,
change nothing inside it, and fix the `path` of each dependency on it. `just rule-changes` counts
the moved files as neither rules nor code, so this is a code pull request. Outside `crates/`, only
the crate's row under Layout in `CONTRIBUTING.md` names its directory. Fix that row in a rules
pull request after the move. Moving `crates/spareline`, which holds the web UI, also changes the root
files that name `crates/spareline/web`: `justfile`, `Containerfile`, `.gitignore` and
`.dockerignore`. They are rules, so that move is a rules pull request, and the Layout rows can go
in it.

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
that holds it. It also adds the new name to `changelog_include` in `release-plz.toml`, and to
`build_script_crates` if the crate has a build script. A code pull request then changes the
package's `name`, each dependency on it and each path that names it in code. A last rules pull
request removes the old name's key and the old name from each list. Move the directory and rename
the package in separate pull requests.

This doesn't cover the package `spareline`, which carries the project's name. Rule files
name it too: the root `Cargo.toml`, `dylint.toml`, the release configuration, the justfile, the Containerfile
and the workflows. So renaming it changes rules and code together. To rename it,
follow "Name and kind" under [Change an answer](#change-an-answer).

## Extend what trellis generates

Each of these is a place trellis leaves to the project, so you never edit a file it generates.
An update leaves them alone.

- **What the image adds.** The `image` answer, a YAML mapping, adds to the image without an edit
  to `Containerfile`: `env` sets environment defaults, each a quoted string, `workdir` the
  directory the service starts in, `expose` the ports it listens on, and `files: true` copies
  `crates/spareline/image/` onto the image's root, such as data files at
  `crates/spareline/image/app/profiles/` or extra notices at
  `crates/spareline/image/usr/share/doc/spareline/`. The image stays distroless
  and runs as its `nonroot` user. Its files and working directory belong to root, readable by all
  and executable by none, so a directory the service writes needs a mount. They are data: none
  may be a link or an ELF program or library, and they go under `/usr/share` or a directory of
  their own, such as `/app`, never in one the base image keeps, such as `/usr/lib`, `/etc` or
  `/tmp`. The binary and trellis's license files win over a file at the same path. Those files
  are code, apart from ones that are rules by their names, such as a `.gitignore`. Land them in a
  code pull request first, since the image's build fails without them, then set the answer in a
  rules pull request:

  ```sh
  uvx copier update --defaults --vcs-ref=:current: \
    --data 'image={env: {HOST: 0.0.0.0, PORT: "8080"}, workdir: /app, expose: [8080], files: true}'
  ```

  The answer replaces the whole mapping, so repeat every key you keep. `just image` builds the
  image as the release does.

- **Deploy kit.** Keep the scripts and units that install and run it on a host in `deploy/`.
  They are code, and `just policy` runs shellcheck on the scripts; "Send a change" in
  [CONTRIBUTING.md](../CONTRIBUTING.md#send-a-change) says which files there stay rules.

## Change an answer

To change an answer, keep the trellis version and pass the new value, such as the license:

```sh
uvx copier update --defaults --vcs-ref=:current: --data 'license=MIT OR Apache-2.0'
```

or the processors the image runs on, `x86_64` and `aarch64` (ARM64):

```sh
uvx copier update --defaults --vcs-ref=:current: --data 'architectures=[x86_64]'
```

The change is a rule change, so it can't also change code. Once it merges, run
`just check-github` on `main`, and `just setup-github` if it reports a wrong setting.
Changing the architectures needs nothing on GitHub, since `CI passed` covers whatever platforms CI
runs.
Some answers also change what GitHub needs:

- **Visibility.** A private repository costs more to run and lacks some of what a public one
  has. Protecting its `main` needs GitHub Pro or a paid organization plan, and its CI draws on the
  account's included Actions usage; GitHub's
  [Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)
  page has the rates.
  Its image is neither attested nor signed, because without GitHub Enterprise Cloud, GitHub
  attests images only in public repositories, and a keyless signature would name the repository
  in Sigstore's public log.
  GitHub takes private vulnerability reports only in public repositories, so a private
  repository's `SECURITY.md` sends reports to issues.

  To make the repository private, first merge the change that
  `uvx copier update --defaults --vcs-ref=:current: --data visibility=private` makes, then make
  the repository private on GitHub and run `just check-github` on `main`. The Actions policy for
  the rule-change check then no longer matters.
  The update keeps building for ARM64, whose runners draw on a private repository's Actions usage.
  To stop, add `--data 'architectures=[x86_64]'` to it.
  The image's package stays public, because GitHub can't make a public package private again.
  Delete it in its package settings before the next release, which then publishes a new, private
  package.

An update writes nothing into `crates/` apart from a published crate's `LICENSE` files, which it
adds or deletes as publishing or the license changes. So the answers that the crates' own files
hold need more than the update, and two can't change this way at all:

- **Name and kind.** An update records a new `project_name` or `kind` and rewrites the files
  outside `crates/` for it, but the crates keep their names and scaffold, so `just policy` fails,
  or passes on crates the new answer doesn't describe. To rename the project or change what it
  ships, generate a new project and move the code into it.
- **Description.** The update changes `README.md` and `CONTRIBUTING.md`, and the image's
  description label in `Containerfile`. Each crate's
  `description` in its `Cargo.toml` and its `//!` crate docs keep the old sentence; change them
  in a code pull request.
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
