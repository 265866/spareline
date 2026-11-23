# Every check CI runs on a pull request is a recipe here, so `just ci` before pushing runs the same
# checks. The exceptions are the pull request title, which only exists on GitHub, and the release
# builds for each platform and the container image, which CI runs on their own runners. CI runs
# `rule-changes` against the pull request's base, with the checker the base branch pins;
# `just ci <base>` does the same locally.
# On Windows, recipes run in Windows PowerShell, which every Windows install has. The owner's
# recipes, in the `owner` group, need admin rights on the repository or the release token, so
# `just ci` leaves them out.

set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]

# Only the formatter runs on this nightly, for two options stable rustfmt lacks.
# `imports_granularity` is the setting for the Google Rust Style Guide's rule that a `use` imports
# items from one module. The guide lets a codebase on stable Rust skip `format_macro_matchers`, and
# trellis applies it anyway. Everything else builds on the stable toolchain in rust-toolchain.toml.
nightly := "nightly-2026-10-28"

# The nightly the trellis lints build on. `cargo dylint` installs it on first use; CI installs it
# first so the toolchain download shows as its own step.
lints_nightly := "nightly-2026-11-12"

# The trellis commit the lints in Cargo.toml and the checker below come from. `uvx copier update`
# moves both.
trellis_rev := "a5c80eb01b24db7c095429f4921c1891c56ac715"

# rustdoc warnings fail `just docs` and the doctests. Cargo reads `CARGO_ENCODED_RUSTDOCFLAGS`
# before `RUSTDOCFLAGS`, so a caller's setting of either, even an empty one, can't drop the flag.
export CARGO_ENCODED_RUSTDOCFLAGS := "-Dwarnings"

# clippy reads only the root `clippy.toml`, whatever other clippy configuration a directory holds.
export CLIPPY_CONF_DIR := justfile_directory()

# Lists the recipes.
default:
    @just --list

# On a branch stacked on another pull request, pass the branch it targets as the argument, such as
# `just ci origin/my-rules`, for `rule-changes` to compare against it.
#
# Runs every check CI runs.
ci base="origin/main": fmt-check typos actionlint zizmor shear deny links policy lint dylint test docs build-release notices web-check web-build (rule-changes base)

# Installs the pinned toolchains: stable, the nightly rustfmt, and the lints' nightly.
install-toolchains:
    rustup toolchain install
    rustup toolchain install {{ nightly }} --profile minimal --component rustfmt
    rustup toolchain install {{ lints_nightly }} --profile minimal --component rustc-dev,llvm-tools-preview

# Installs the check tools at the versions CI pins in `.github/workflows/`.
install-tools:
    cargo binstall --no-confirm cargo-deny@0.20.2 cargo-shear@1.14.0 cargo-nextest@0.9.150 lychee@0.24.2
    cargo binstall --no-confirm cargo-dylint@6.4.0 dylint-link@6.4.0 typos-cli@1.51.1 taplo-cli@0.10.0
    cargo binstall --no-confirm cargo-about@0.9.3
    go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12
    uv tool install zizmor@1.31.1

# The `trellis-policy` this project runs, installed under `target/` so each project keeps the one
# its trellis commit pins. Recipes run from this directory, so the relative path works whatever
# the project's path holds, spaces included, in sh and PowerShell alike.
policy_root := justfile_directory() / "target" / "trellis-policy"
trellis_policy := "./target/trellis-policy/bin/trellis-policy"

# `cargo install --git` builds the checker as a member of trellis's workspace, so each trellis
# commit builds to the same path, and cargo takes the sources it fetched as unchanging. Given a
# target or build directory that holds a build from another commit, as CARGO_TARGET_DIR or
# `build.build-dir` can give it, cargo installs that build without rebuilding and records the
# pinned commit as installed. So the install builds in target/trellis-policy/build, new each time
# and deleted after, as cargo does when nothing sets a target directory, and `--force` replaces
# whatever cargo's record says is installed. After an install, target/trellis-policy/trellis-rev
# records the commit and the binary's size and SHA-256, and a run skips the install only when all
# three match, so a binary that is missing, or changed or replaced since, is installed again.
# `--quiet` keeps cargo to its errors. Its warnings here are advice that doesn't apply: to add
# `policy_root` to `PATH`, though recipes call the binary by its path, and that the toolchain in
# rust-toolchain.toml builds it, which is the one the project pins.
#
# Installs `trellis-policy` at the pinned commit, unless that build is installed.
[unix]
install-policy:
    #!/usr/bin/env bash
    set -euo pipefail
    bin=target/trellis-policy/bin/trellis-policy stamp=target/trellis-policy/trellis-rev build=target/trellis-policy/build
    record() {
      local hash
      if command -v sha256sum > /dev/null; then hash=$(sha256sum < "$bin")
      elif command -v shasum > /dev/null; then hash=$(shasum -a 256 < "$bin")
      else echo "error: install-policy needs sha256sum or shasum to record the checker it installs" >&2; exit 1
      fi
      echo "{{ trellis_rev }} $(wc -c < "$bin" | tr -d ' ') ${hash%% *}"
    }
    if [ -f "$bin" ] && [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$(record)" ]; then exit 0; fi
    echo "Installing trellis-policy at {{ trellis_rev }}." >&2
    rm -rf "$stamp" "$build"
    trap 'rm -rf "$build"' EXIT
    CARGO_BUILD_BUILD_DIR="$build" cargo install --quiet --locked --force --root "{{ policy_root }}" \
      --target-dir "$build" --git https://github.com/265866/trellis --rev {{ trellis_rev }} trellis-policy
    record > "$stamp"

# Installs `trellis-policy` at the pinned commit, unless that build is installed.
[windows]
install-policy:
    @$ErrorActionPreference = 'Stop'; $bin = 'target/trellis-policy/bin/trellis-policy.exe'; $stamp = 'target/trellis-policy/trellis-rev'; $build = 'target/trellis-policy/build'; function Get-Record { "{{ trellis_rev }} $((Get-Item -LiteralPath $bin).Length) $((Get-FileHash -LiteralPath $bin -Algorithm SHA256).Hash.ToLowerInvariant())" }; if ((Test-Path -LiteralPath $bin) -and (Test-Path -LiteralPath $stamp) -and "$(Get-Content -LiteralPath $stamp -Raw)".Trim() -eq (Get-Record)) { exit 0 }; [Console]::Error.WriteLine('Installing trellis-policy at {{ trellis_rev }}.'); foreach ($path in $stamp, $build) { if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Recurse -Force } }; $env:CARGO_BUILD_BUILD_DIR = $build; $ErrorActionPreference = 'Continue'; cargo install --quiet --locked --force --root "{{ policy_root }}" --target-dir $build --git https://github.com/265866/trellis --rev {{ trellis_rev }} trellis-policy; $status = $LASTEXITCODE; $ErrorActionPreference = 'Stop'; if ($status -ne 0) { if (Test-Path -LiteralPath $build) { Remove-Item -LiteralPath $build -Recurse -Force }; exit $status }; Set-Content -LiteralPath $stamp -Value (Get-Record) -NoNewline; Remove-Item -LiteralPath $build -Recurse -Force -ErrorAction SilentlyContinue

# Formats the Rust and TOML files.
fmt:
    cargo +{{ nightly }} fmt --all
    taplo fmt

# Checks the Rust and TOML files are formatted.
fmt-check:
    cargo +{{ nightly }} fmt --all -- --check
    taplo fmt --check

# Checks spelling in code and docs.
typos:
    typos

# actionlint skips the shell scripts without a word when shellcheck is missing, so this runs
# shellcheck first to fail instead.
#
# Checks the GitHub workflows and their shell scripts.
actionlint:
    shellcheck --version
    actionlint

# Audits the GitHub workflows for security problems.
zizmor:
    zizmor .github

# Checks every declared dependency is used.
shear:
    cargo shear

# Checks dependencies for advisories, licenses, bans and sources.
deny: web-audit
    cargo deny --locked check

# Checks that every relative link and anchor in the Markdown files resolves.
links:
    lychee --offline --include-fragments --no-progress --exclude-path target --exclude-path crates/spareline/web/node_modules './**/*.md'

# The structure rules: workspace lints, manifests, a published crate's license files, build
# scripts, config files, crate dependencies, the release changelog, the docs index, the docs
# layout that `docs_files` and `readme_sections` in Cargo.toml set, and the readme. What only a
# release fails on, such as a readme prompt left to write, prints as a notice.
#
# Checks the structure rules.
policy: install-policy
    {{ trellis_policy }} check

# The release pull request and the release workflow run this before anything is published.
#
# Checks the structure rules as a release does, failing on the notices `just policy` prints too.
policy-release: install-policy
    {{ trellis_policy }} check --release

# Runs clippy on every target with warnings as errors.
lint:
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

# The first run builds the lint library on its nightly, so it takes a while.
#
# Runs the custom lints from trellis, warnings as errors.
dylint: (_dylint "-D warnings")

# cargo-dylint 6.4.0 builds every revision of a git lint library into one target directory, and
# cargo doesn't check the modification times of sources under CARGO_HOME, so after the trellis
# commit in Cargo.toml moves it keeps the library it built before, and a run after an update would
# check with the old lints. target/dylint/trellis-rev records the commit the libraries were built
# from, and a different one deletes them first. It assumes the target directory is `target`.
[private]
[unix]
_dylint $DYLINT_RUSTFLAGS:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$(cat target/dylint/trellis-rev 2> /dev/null)" != "{{ trellis_rev }}" ]; then
      rm -rf target/dylint/libraries
      mkdir -p target/dylint
      echo "{{ trellis_rev }}" > target/dylint/trellis-rev
    fi
    cargo dylint --all --fail-on-no-libraries -- --locked --workspace --all-targets --all-features

[private]
[windows]
_dylint $DYLINT_RUSTFLAGS:
    $ErrorActionPreference = 'Stop'; $stamp = 'target/dylint/trellis-rev'; if (-not (Test-Path -LiteralPath $stamp) -or "$(Get-Content -LiteralPath $stamp -Raw)".Trim() -ne '{{ trellis_rev }}') { if (Test-Path -LiteralPath target/dylint/libraries) { Remove-Item -LiteralPath target/dylint/libraries -Recurse -Force }; New-Item -ItemType Directory -Force -Path target/dylint > $null; Set-Content -LiteralPath $stamp -Value '{{ trellis_rev }}' -NoNewline }
    cargo dylint --all --fail-on-no-libraries -- --locked --workspace --all-targets --all-features

# The flags run ignored tests too, so `#[ignore]` can't skip a test here or in CI even where
# `disabled_code` doesn't report it.
#
# Runs the tests, then the doctests, which nextest does not run.
test:
    cargo nextest run --locked --workspace --all-features --run-ignored all
    cargo test --locked --workspace --doc --all-features -- --include-ignored

# Builds the docs, private items included.
docs:
    cargo doc --locked --workspace --no-deps --document-private-items --all-features

# Builds the release binaries.
build-release:
    cargo build --locked --workspace --release

# A clarification in about.toml that no longer applies is only logged, and a clean run logs
# nothing, so any output fails it, but one warning: an entry for a crate the dependency graph
# doesn't hold. That is how a rules pull request accepts a license for a crate before the code
# pull request that adds it, as CONTRIBUTING.md says; `just stale-licenses`, which the weekly
# Advisories workflow runs, fails on an entry whose crate has left the graph.
#
# Writes the license notices every release ships to target/THIRD-PARTY-LICENSES.html.
[unix]
notices: web-notices
    #!/usr/bin/env bash
    set -uo pipefail
    mkdir -p target
    log=$(cargo about --color never generate --locked --fail about.hbs -o target/THIRD-PARTY-LICENSES.html 2>&1)
    status=$?
    log=$(awk '$0 == "warning: config found for a crate not present in the graph" { skip = 1; next }
      skip && $0 == "" { skip = 0; next } !skip' <<< "$log")
    if [ -n "$log" ]; then echo "$log" >&2; fi
    [ "$status" -eq 0 ] && [ -z "$log" ]

# Writes the license notices every release ships to target/THIRD-PARTY-LICENSES.html.
[windows]
notices: web-notices
    New-Item -ItemType Directory -Force target > $null; $log = cargo about --color never generate --locked --fail about.hbs -o target/THIRD-PARTY-LICENSES.html 2>&1; $status = $LASTEXITCODE; $skip = $false; $kept = foreach ($line in $log) { $text = if ($line -is [System.Management.Automation.ErrorRecord]) { $line.Exception.Message } else { "$line" }; if ($text -eq 'warning: config found for a crate not present in the graph') { $skip = $true } elseif ($skip -and $text -eq '') { $skip = $false } elseif (-not $skip) { $text } }; $kept | ForEach-Object { [Console]::Error.WriteLine($_) }; if ($status -ne 0 -or $kept) { exit 1 }

# Each entry in about.toml for a crate the dependency graph doesn't hold, which `just notices`
# lets through so a license can be accepted before its crate arrives, fails this once its crate is
# gone. The weekly Advisories workflow runs it on `main`; remove such an entry in a rules pull
# request.
#
# An entry in web-licenses.toml for an npm package the web UI doesn't install fails it too, so it
# installs the packages first.
#
# Checks that every crate about.toml names, and every package web-licenses.toml names, is there.
stale-licenses: web-install install-policy
    #!/usr/bin/env bash
    set -uo pipefail
    {{ trellis_policy }} stale-web-licenses web-licenses.toml crates/spareline/web/node_modules
    web=$?
    log=$(cargo about --color never generate --locked --fail about.hbs -o /dev/null 2>&1)
    status=$?
    # Each warning names the entry's line in about.toml on the line after it.
    lines=$(awk '$0 == "warning: config found for a crate not present in the graph" { found = 1; next }
      found { if (match($0, /about\.toml:[0-9]+:/)) { print substr($0, RSTART + 11, RLENGTH - 12) } found = 0 }' <<< "$log")
    count=$(grep --count --line-regexp --fixed-strings 'warning: config found for a crate not present in the graph' <<< "$log")
    if [ "$count" -gt 0 ]; then
      stale=$(for line in $lines; do sed -n "${line}p" about.toml; done | tr '\n' ' ' | sed 's/ $//')
      entries="$count entries for crates" each=each
      if [ "$count" -eq 1 ]; then entries="an entry for a crate" each=it; fi
      echo "error: about.toml has $entries the dependency graph doesn't hold: ${stale:-see above}. Remove $each in a rules pull request, unless the code pull request that adds its crate is still open; land that instead." >&2
      echo "$log" >&2
      exit 1
    fi
    if [ "$status" -ne 0 ]; then echo "$log" >&2; exit "$status"; fi
    exit "$web"

# A pull request changes either the rules or the code, never both; see CONTRIBUTING.md. Compares
# the working tree, staged, unstaged and untracked files included, with its merge base with
# `base`. CI passes `--head <sha>` to judge a pull request's head commit.
#
# Checks that rules and code change in separate pull requests.
rule-changes base="origin/main": install-policy
    {{ trellis_policy }} rule-changes {{ base }}

# Runs it; put its own arguments after `--`.
run *args: web-build
    cargo run --locked --package spareline -- {{ args }}

# Where the service finds the web UI's build, which `just run` builds first. The image sets it to
# where it puts the build.
export SPARELINE_WEB_DIR := justfile_directory() / "crates" / "spareline" / "web" / "dist"

# `bun install --frozen-lockfile` passes with no lockfile and then picks the versions itself, so
# this fails when bun.lock is missing. `bun run` finds a script's commands first in
# node_modules/.bin, in the web UI's directory and in every directory above it. So if one of those
# holds a `bun` or `bunx` command, the web UI's scripts run on that Bun, not the pinned one, and
# this fails too.
#
# Installs the web UI's packages, exactly as bun.lock lists them.
[working-directory("crates/spareline/web")]
web-install:
    @bun --eval "if (!require('node:fs').existsSync('bun.lock')) { console.error('error: crates/spareline/web has no bun.lock to install from. Write it there with bun install --save-text-lockfile --lockfile-only and commit it.'); process.exit(1); }"
    bun install --frozen-lockfile
    @bun --eval "const path = require('node:path'); for (let dir = process.cwd(), up; ; dir = up) { const bin = path.join(dir, 'node_modules', '.bin'); if (['bun', 'bunx'].some((name) => Bun.which(name, { PATH: bin }))) { console.error('error: ' + bin + ' holds a bun or bunx command, so the web scripts would run on that Bun and not the pinned one. CONTRIBUTING.md says what to do.'); process.exit(1); } up = path.dirname(dir); if (up === dir) break; }"

# Checks the web UI with its `check` script, which type-checks it.
[working-directory("crates/spareline/web")]
web-check: web-install
    bun run check

# Builds the web UI into crates/spareline/web/dist.
[working-directory("crates/spareline/web")]
web-build: web-install
    bun run build

# Checks the web UI's packages for advisories.
[working-directory("crates/spareline/web")]
web-audit:
    bun audit

# Every installed package is listed, dev ones included, since the bundle can hold any of them. It
# fails on a license that neither about.toml accepts nor web-licenses.toml accepts for that
# package. An entry in web-licenses.toml for a package not installed passes, so a rules pull
# request can accept a license before the code pull request adds the package;
# `just stale-licenses` fails on it once the package is gone.
#
# Writes the web UI's license notices to target/THIRD-PARTY-LICENSES-WEB.txt.
web-notices: web-install install-policy
    {{ trellis_policy }} web-notices about.toml web-licenses.toml crates/spareline/web/node_modules target/THIRD-PARTY-LICENSES-WEB.txt

# Builds the container image for this machine's architecture, with the license notices.
image: notices
    docker build --file Containerfile --tag spareline:dev .

# The owner's recipes are bash scripts, which run on Windows in Git Bash: what they check doesn't
# fit on one PowerShell line, and only the owner runs them. They need `gh` signed in as an admin of
# the repository. Branch protection is a repository ruleset, since a ruleset can protect `main`
# before it exists and require checks before they have ever run.
#
# Sets up the GitHub repository as docs/maintaining.md says. Rerun it after an answer change.
[group("owner")]
[arg("dry_run", long="dry-run", value="true", help="Print each gh call instead of running it")]
setup-github $dry_run="false":
    #!/usr/bin/env bash
    set -euo pipefail
    repo=265866/spareline
    err=$(mktemp)
    classic_err=$(mktemp)
    trap 'rm -f "$err" "$classic_err"' EXIT
    if [ "${dry_run:?}" = true ]; then
      echo "Dry run: each gh call is printed instead of run, each lookup finds nothing, and the admin and visibility checks are skipped."
    fi

    lookup() {
      if [ "$dry_run" = true ]; then
        echo "gh api --paginate '$2' --jq '$3'"
        printf -v "$1" ''
        return
      fi
      local value
      value=$(gh api --paginate "$2" --jq "$3") || return
      printf -v "$1" '%s' "$value"
    }
    change() {
      if [ "$dry_run" = true ]; then
        if [ -n "${3:-}" ]; then
          printf 'gh api --method %s %s --input - <<EOF\n%s\nEOF\n' "$1" "$2" "$3"
        else
          echo "gh api --method $1 $2"
        fi
      elif [ -n "${3:-}" ]; then
        gh api --method "$1" "$2" --input - <<< "$3" > /dev/null
      else
        gh api --method "$1" "$2" > /dev/null
      fi
    }

    lookup state "repos/$repo" '"\(.visibility) \(.permissions.admin)"'
    if [ -n "$state" ]; then
      read -r visibility admin <<< "$state"
      if [ "$admin" != true ]; then
        echo "error: setting up $repo needs admin rights on it" >&2
        exit 1
      fi
      if [ "$visibility" != public ]; then
        echo "error: $repo is $visibility, but the project's answers say public; docs/maintaining.md says which to change first" >&2
        exit 1
      fi
    fi

    echo "Squash merging only, with the pull request's title and description as the commit."
    change PATCH "repos/$repo" '{"allow_squash_merge": true, "allow_merge_commit": false, "allow_rebase_merge": false,
     "squash_merge_commit_title": "PR_TITLE", "squash_merge_commit_message": "PR_BODY"}'

    echo "The release label that release-plz puts on its pull request."
    lookup label "repos/$repo/labels?per_page=100" '.[] | select(.name | ascii_downcase == "release") | .name'
    if [ -z "$label" ]; then
      change POST "repos/$repo/labels" '{"name": "release", "color": "0e8a16", "description": "The release pull request"}'
    fi

    echo "The Actions policy that lets rule-changes.yml run on pull_request_target."
    policy='{"name": "Rule-change check", "enforcement": "active",
     "conditions": {"workflow_path": {"include": [".github/workflows/rule-changes.yml"], "exclude": []}},
     "rules": [{"type": "restrict_action_events", "parameters": {"allowed_events": ["pull_request_target"]}}]}'
    lookup policy_id "repos/$repo/actions/policies?has_parents=false&per_page=100" \
      '[.policies[] | select(.name == "Rule-change check" and .source_type == "Repository") | .id] | first // empty'
    if [ -n "$policy_id" ]; then
      change PUT "repos/$repo/actions/policies/$policy_id" "$policy"
    else
      change POST "repos/$repo/actions/policies" "$policy"
    fi

    echo "Private vulnerability reporting, which SECURITY.md points to."
    change PUT "repos/$repo/private-vulnerability-reporting"

    protected="A ruleset that protects main and requires PR title, CI passed and Rule changes stand alone."
    ruleset='{"name": "main", "target": "branch", "enforcement": "active", "bypass_actors": [],
     "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
     "rules": [
      {"type": "deletion"},
      {"type": "non_fast_forward"},
      {"type": "pull_request", "parameters": {"required_approving_review_count": 0,
       "dismiss_stale_reviews_on_push": false, "require_code_owner_review": false,
       "require_last_push_approval": false, "required_review_thread_resolution": false,
       "allowed_merge_methods": ["squash"]}},
      {"type": "required_status_checks", "parameters": {"strict_required_status_checks_policy": false,
       "do_not_enforce_on_create": true, "required_status_checks": [
        {"context": "PR title", "integration_id": 15368},
        {"context": "CI passed", "integration_id": 15368},
        {"context": "Rule changes stand alone", "integration_id": 15368}]}}]}'
    protect() {
      lookup ruleset_id "repos/$repo/rulesets?includes_parents=false&per_page=100" \
        '[.[] | select(.name == "main" and .source_type == "Repository") | .id] | first // empty' || return
      if [ -n "$ruleset_id" ]; then
        change PUT "repos/$repo/rulesets/$ruleset_id" "$ruleset" || return
      else
        change POST "repos/$repo/rulesets" "$ruleset" || return
      fi
      classic=''
      lookup classic "repos/$repo/branches/main/protection/required_status_checks" '.contexts | join(", ")' 2> "$classic_err" \
        || grep --quiet 'HTTP 404' "$classic_err" || { cat "$classic_err" >&2; return 1; }
      if [ -n "$classic" ]; then
        echo "Classic branch protection also requires $classic; the ruleset replaces those checks."
        change DELETE "repos/$repo/branches/main/protection/required_status_checks" || return
      fi
    }
    echo "$protected"
    protect

    echo
    echo "Left to do by hand, as docs/maintaining.md says:"
    echo "- Install the Renovate app on $repo: https://github.com/apps/renovate"
    echo "- Create a fine-grained token with contents and pull request write access to $repo, then"
    echo "  check and store it: just set-release-token"
    echo "- Run just check-github, then push main if it isn't pushed yet."
    echo "- At the first release, do the steps in its section of docs/maintaining.md."

# Read-only: it reads the settings and never a secret's value.
#
# Checks the GitHub repository's settings match what setup-github sets.
[group("owner")]
check-github:
    #!/usr/bin/env bash
    set -euo pipefail
    repo=265866/spareline
    wrong=0
    err=$(mktemp)
    trap 'rm -f "$err"' EXIT
    verdict() {
      if [ "$2" = "$3" ]; then
        echo "ok: $1"
      else
        echo "wrong: $1: \"${2:-nothing}\", expected \"$3\""
        wrong=1
      fi
    }

    settings=$(gh api "repos/$repo" --jq '.visibility, "squash \(.allow_squash_merge), merge commits \(.allow_merge_commit), rebase \(.allow_rebase_merge), squash commit \(.squash_merge_commit_title) and \(.squash_merge_commit_message)"')
    { read -r visibility; read -r merging; } <<< "$settings"
    verdict "visibility" "$visibility" public
    verdict "merge settings" "$merging" "squash true, merge commits false, rebase false, squash commit PR_TITLE and PR_BODY"
    label=$(gh api --paginate "repos/$repo/labels?per_page=100" --jq '.[] | select(.name | ascii_downcase == "release") | "present"')
    verdict "release label" "${label:-missing}" present
    policy_id=$(gh api --paginate "repos/$repo/actions/policies?has_parents=false&per_page=100" \
      --jq '[.policies[] | select(.name == "Rule-change check" and .source_type == "Repository") | .id] | first // empty')
    policy=missing
    if [ -n "$policy_id" ]; then
      policy=$(gh api "repos/$repo/actions/policies/$policy_id" --jq '"\(.enforcement), \(.conditions.workflow_path.include // [] | join(" ")) on \([.rules // [] | .[] | select(.type == "restrict_action_events") | .parameters.allowed_events[]] | join(" "))"')
    fi
    verdict "Actions policy for rule-changes.yml" "$policy" "active, .github/workflows/rule-changes.yml on pull_request_target"
    reporting=$(gh api "repos/$repo/private-vulnerability-reporting" --jq 'if .enabled then "on" else "off" end')
    verdict "private vulnerability reporting" "$reporting" on
    rules_jq='([.[] | select(.type == "required_status_checks")
        | .parameters.required_status_checks[]
        | "\(.context) from \(if .integration_id == 15368 then "GitHub Actions" else .integration_id // "any source" end)"]
        | unique | join(", ")) + "; " + ([.[].type | select(. == "deletion" or . == "non_fast_forward" or . == "pull_request")] | unique | join(", "))'
    if ! classic=$(gh api "repos/$repo/branches/main/protection/required_status_checks" --jq '.contexts | join(", ")' 2> "$err"); then
      if ! grep --quiet 'HTTP 404\|Upgrade to GitHub' "$err"; then
        cat "$err" >&2
        exit 1
      fi
      classic=''
    fi
    if ! ruleset=$(gh api --paginate "repos/$repo/rulesets?includes_parents=false&per_page=100" \
      --jq '[.[] | select(.name == "main" and .source_type == "Repository") | "\(.id) \(.enforcement)"] | first // empty' 2> "$err"); then
      cat "$err" >&2
      exit 1
    fi
    read -r ruleset_id enforcement <<< "${ruleset:-}"
    if [ -z "$enforcement" ]; then
      echo "wrong: main's protection: there's no ruleset named main; run just setup-github"
      wrong=1
    elif [ "$enforcement" != active ]; then
      echo "wrong: main's protection: the ruleset main is ${enforcement/evaluate/in evaluate mode}, so it protects nothing; run just setup-github"
      wrong=1
    else
      # Only the ruleset itself lists who may bypass it, and only to an account that may change it.
      bypass=$(gh api "repos/$repo/rulesets/$ruleset_id" --jq 'if (.bypass_actors | type) != "array" then "unreadable"
        elif .bypass_actors == [] then "nothing"
        else [.bypass_actors[] | "\(.actor_type) \(.actor_id) (\(.bypass_mode))"] | join(", ") end' 2> "$err") \
        || { cat "$err" >&2; exit 1; }
      verdict "what may bypass main's ruleset" "$bypass" nothing
      # Another ruleset's rules don't count: this reads who may bypass only the one named main.
      rules=$(gh api "repos/$repo/rules/branches/main" \
        --jq "[.[] | select(.ruleset_id == $ruleset_id)] | $rules_jq" 2> "$err") || { cat "$err" >&2; exit 1; }
      if [ "$rules" = '; ' ]; then
        echo "wrong: main's protection: the ruleset main is active but applies no rule to the branch main; run just setup-github"
        wrong=1
      else
        verdict "main's required checks and rules" "$rules" \
          "CI passed from GitHub Actions, PR title from GitHub Actions, Rule changes stand alone from GitHub Actions; deletion, non_fast_forward, pull_request"
      fi
    fi
    verdict "classic branch protection's required checks" "${classic:-none}" none
    token=$(gh api --paginate "repos/$repo/actions/secrets?per_page=100" --jq '.secrets[] | select(.name == "RELEASE_PLZ_TOKEN") | "present"')
    verdict "RELEASE_PLZ_TOKEN secret" "${token:-missing}" present
    echo "Not checked: whether the Renovate app is installed, and the release token's access and expiry."
    echo "GitHub doesn't show a secret's value; the release workflow checks the token on every push to main."
    exit "$wrong"

# The repository's `permissions.push` says whether the token's account may push. A classic or
# OAuth token then gets 422 for a request missing its required fields whatever it may do, but a
# fine-grained token without the permission a request needs gets 403 first, so the two requests
# below create nothing and show the token's own permissions. An expired or revoked token gets 401.
# The release workflow runs this before release-plz, which would otherwise fail with a bare 403.
#
# Checks that the token in RELEASE_PLZ_TOKEN can update the release pull request.
[group("owner")]
check-release-token:
    #!/usr/bin/env bash
    set -euo pipefail
    repo=265866/spareline
    fix="docs/maintaining.md says which access the token needs, under Before the first push, and how to replace it, under Renew the release token."
    report() {
      if [ -n "${GITHUB_ACTIONS:-}" ]; then
        echo "::$1::$2"
      else
        echo "$1: $2" >&2
      fi
    }
    if [ -z "${RELEASE_PLZ_TOKEN:-}" ]; then
      report error "RELEASE_PLZ_TOKEN is empty or missing. $fix"
      exit 1
    fi
    # Prints the response's status line and headers, then its body; gh fails on any status but 2xx.
    request() {
      if [ -n "${3:-}" ]; then
        GH_TOKEN="$RELEASE_PLZ_TOKEN" gh api --include --method "$1" "$2" --input - <<< "$3" 2> /dev/null || true
      else
        GH_TOKEN="$RELEASE_PLZ_TOKEN" gh api --include --method "$1" "$2" 2> /dev/null || true
      fi
    }
    status() { awk 'NR == 1 { print $2; exit }' <<< "$1"; }
    failure() { echo "${1:-no response from GitHub}" | head -n 1; }

    response=$(request GET "repos/$repo")
    case $(status "$response") in
      200) echo "ok: the token can read $repo" ;;
      401) report error "GitHub rejects the token: it has expired, been revoked or been mistyped. $fix"; exit 1 ;;
      403 | 404) report error "The token has no access to $repo. $fix"; exit 1 ;;
      *) report error "Reading $repo with the token failed: $(failure "$response")"; exit 1 ;;
    esac
    if ! grep --quiet '"push":true' <<< "$response"; then
      report error "The token's account can't push to $repo. Create the token with an account that has write access. $fix"
      exit 1
    fi
    expires=$(sed -n 's/^[Gg]ithub-[Aa]uthentication-[Tt]oken-[Ee]xpiration: *//p' <<< "$response" | tr -d '\r')
    soon=$(date -u -d '+14 days' +%F 2> /dev/null || date -u -v+14d +%F)
    if [ -n "$expires" ] && [[ ! "${expires%% *}" > "$soon" ]]; then
      report warning "The release token expires $expires. Replace it before then; $fix"
    fi

    failed=0
    for access in "contents:POST:repos/$repo/git/refs" "pull requests:POST:repos/$repo/pulls"; do
      name=${access%%:*} rest=${access#*:}
      response=$(request "${rest%%:*}" "${rest#*:}" '{}')
      case $(status "$response") in
        422) echo "ok: the token can write $name" ;;
        403 | 404) report error "The token can't write $name in $repo. $fix"; failed=1 ;;
        *) report error "Checking the token's access to $name failed: $(failure "$response")"; failed=1 ;;
      esac
    done
    exit "$failed"

# The token is read without echoing it, from the terminal or from standard input, and reaches gh
# on standard input, so it shows in no command line.
#
# Checks a new release token as check-release-token does, then stores it as RELEASE_PLZ_TOKEN.
[group("owner")]
set-release-token:
    #!/usr/bin/env bash
    set -euo pipefail
    IFS= read -r -s -p "Paste the release token: " token || [ -n "$token" ]
    echo
    RELEASE_PLZ_TOKEN="$token" "{{ just_executable() }}" --justfile "{{ justfile() }}" check-release-token
    printf '%s' "$token" | gh secret set RELEASE_PLZ_TOKEN --repo 265866/spareline
    echo "Stored the token as the RELEASE_PLZ_TOKEN secret of 265866/spareline."
