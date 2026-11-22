# Code style

A trellis project follows the
[Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html), and through it the
official [Rust Style Guide](https://doc.rust-lang.org/nightly/style-guide/) and the
[Rust API Guidelines](https://rust-lang.github.io/api-guidelines/). This page lists which of the
Google guide's rules a check enforces, how to silence a lint, which rules only review can catch,
where trellis goes further than the guide, and how the docs are laid out.

## Checked by tools

`just ci` runs every check below, and CI runs them on every pull request. Rows marked "partly" leave
part of the rule to review. Lint levels live in `[workspace.lints]` in `Cargo.toml`, lint settings
in `clippy.toml`, formatting in `rustfmt.toml`, dependency rules in `deny.toml`, the custom lints'
settings in `dylint.toml`, and the structure rules' settings in `[workspace.metadata.policy]` of
`Cargo.toml`. The custom lints are trellis's [`lints/`](https://github.com/265866/trellis/tree/main/lints),
and the structure rules its [`trellis-policy`](https://github.com/265866/trellis/tree/main/policy).
Each lint's documentation, with an example and its fix, is at the top of its file in `lints/src/`.
Those links show trellis's `main`. A project builds both at the trellis commit that `trellis_rev`
in its `justfile` names, so read them at that commit when `main` has moved on.

| Google guide rule | Enforced by |
| --- | --- |
| Use the formatter, 100 columns, lowercase hex literals, formatted macro matchers | rustfmt with `max_width`, `hex_literal_case` and `format_macro_matchers` (`just fmt-check`) |
| Use `#[rustfmt::skip]` only for manual alignment or generated code | `rustfmt_skip_justification`: every skip needs an `expect` saying which, and no skip goes inside `cfg_attr` |
| Name source files and leaf modules after the primary user-defined item | `file_named_after_item` |
| Don't implement APIs in non-leaf modules | `non_leaf_module_items` |
| Don't use glob imports, and don't define preludes | `clippy::wildcard_imports`, `foreign_prelude_glob` (no glob import of another crate's prelude, which `clippy::wildcard_imports` allows), `enum_glob_outside_fn` (`use Enum::*` only inside a function), `prelude_module` |
| A `use` statement imports items from one module | rustfmt `imports_granularity = "Module"` |
| Prefer locally-scoped trait imports | `module_level_trait_import` |
| Qualify calls to top-level functions with a module name | `qualified_function_calls`, `clippy::absolute_paths` (no paths longer than two segments) |
| Type names are imported and used unqualified | `qualified_type_path`, `unused_qualifications` |
| Avoid `Rc` and `Arc` | `shared_ownership`: each use in a field or signature needs an `expect` saying why |
| Prefer borrows over deep copies | `clippy::implicit_clone`, `clippy::unnecessary_to_owned`, `clippy::needless_pass_by_value` (partly) |
| Accept flexible references (`&[T]`, `&str`, `Option<&T>`) | `clippy::ptr_arg`, `clippy::borrowed_box`, `clippy::ref_option`, `clippy::ref_option_ref` |
| Application code almost never accepts `impl Into<T>` | `impl_into_params`: library APIs too; a parameter whose call-site ergonomics are paramount needs an `expect` saying so |
| Prefer `enum` over `bool` in APIs | `bool_literal_argument` (no literal `true` or `false` passed to the project's own function's `bool` parameter), `clippy::fn_params_excessive_bools`, `clippy::struct_excessive_bools` (partly) |
| Newtypes versus type aliases | `primitive_type_alias` (aliases of primitives and strings) |
| Prefer named fields over tuples and non-newtype tuple structs | `multi_field_tuple` |
| Implement standard traits instead of look-alike methods | `clippy::should_implement_trait`, `clippy::inherent_to_string` |
| Use `#[derive()]` where possible | `clippy::derivable_impls`, `clippy::expl_impl_clone_on_copy` |
| Don't use traits merely to organize code | `single_impl_trait` |
| Extension traits end in `Ext` | `extension_trait_suffix` |
| Error types are `#[must_use]` and `Copy` where possible | `error_type_must_use`, `error_type_copy` |
| Implement `Default` alongside `new()`; options structs implement `Default` | `clippy::new_without_default`, `options_struct_default` |
| Pair important `From` impls with explicit constructors | `from_without_constructor` |
| Never reach inside a poisoned mutex | `clippy::disallowed_methods` on `PoisonError::into_inner`, `get_ref` and `get_mut`, and on `Mutex::clear_poison` and `RwLock::clear_poison` |
| Macros: ordinary items first, `macro_rules!` next, procedural macros last | `macro_definition`: no macro definitions outside the crates `macro_crates` in `dylint.toml` lists; listing a crate is a rule change, and an `expect` of the lint fails |
| Naming methods (`as_`, `to_`, `into_`) | `clippy::wrong_self_convention`, `clippy::new_ret_no_self` |
| Naming lifetimes | `lifetime_naming` (single-letter lifetimes only on functions, at most two) |
| Naming initialization functions (`try_new` returns a `Result`) | `try_new_result` |
| Getters are named `foo()`, not `get_foo()` | `get_prefixed_getter` |
| Document public APIs | `missing_docs`, `rustdoc::missing_crate_level_docs`, `clippy::missing_errors_doc` |
| Document panic cases | `clippy::missing_panics_doc` |
| Document unsafe invariants | `clippy::missing_safety_doc`, `clippy::unnecessary_safety_doc` |
| Write docs as `///` and `//!` comments, not generated by macros | `doc_attribute_style` (no `/** */`, `#[doc = ...]` or `include_str!` docs) |
| Doc comments come before attributes | `doc_after_attribute` |
| Markdown in doc comments | `clippy::doc_markdown`, rustdoc warnings as errors (`just docs`) |
| Docs use capitalization and punctuation | `doc_starts_lowercase`, `clippy::doc_paragraphs_missing_punctuation` |
| Avoid unsafe code, keep it in a crate of its own, explain each block with a `SAFETY:` comment | `unsafe_code = "deny"`, `unsafe_outside_unsafe_crate`, `unsafe_op_in_unsafe_fn`, `clippy::undocumented_unsafe_blocks`, `clippy::multiple_unsafe_ops_per_block`, `clippy::unnecessary_safety_comment`. A crate that needs unsafe code is listed in `dylint.toml`, and each unsafe item in it carries `#[expect(unsafe_code, reason = "...")]`. |
| No unstable features | `unstable_features = "deny"`, and the stable toolchain in `rust-toolchain.toml` |
| Implement `len()` with `is_empty()` | `clippy::len_without_is_empty` |
| Avoid overloading operators for non-mathematical purposes | `operator_overload`: every operator impl needs an `expect` saying what the operator means (partly) |
| Don't access `#[doc(hidden)]` items you don't own | `foreign_doc_hidden` |
| Apply `#[must_use]` where ignoring a result is a mistake | `clippy::must_use_candidate`, `clippy::return_self_not_must_use`, `clippy::double_must_use` |
| Don't run code before `main()` | cargo-deny bans `ctor`, `inventory` and `linkme` (`just deny`) |
| Don't overuse type inference | `inferred_conversion_binding` (the results of `.parse()`, `.into()`, `.try_into()`, `.collect()`, `.sum()`, `.product()`, `.unzip()`, `.partition(..)` and `Default::default()` get a `let` type rather than a turbofish, except where `?`, `unwrap` or `expect` leaves the compiler nothing to infer from) |
| Prefer pattern matching over check-then-unwrap | `clippy::unnecessary_unwrap`, `clippy::panicking_unwrap` |
| A `#[should_panic]` test names the panic it expects | `clippy::should_panic_without_expect` |
| Prefer `expect` over `unwrap` | `clippy::unwrap_used`, allowed in tests |
| Don't omit patterns on `non_exhaustive` enums when matching | `non_exhaustive_wildcard` |
| Mark public enums and structs others use `#[non_exhaustive]` | `clippy::exhaustive_enums`, `clippy::exhaustive_structs`, in a library only; an `expect` with the reason keeps one exhaustive |
| Use context strings in assertions | `clippy::missing_assert_message`, stricter than the guide (see below) |
| Don't use iterator combinator chains when direct iteration is clearer | `iterator_chain_in_for` (no closure adapters in a `for` header, no more than four in a chain), `mutating_iterator_closure` (no closure passed to an `Iterator` method mutates captured state), `clippy::disallowed_methods` on `Iterator::for_each` |
| Don't call `panic::catch_unwind` | `clippy::disallowed_methods`, and `panic = "abort"` in release builds |
| Prefer `format!` or `write!` for concatenation and substitution | `string_concat_chain`, `str_replace_on_literal`, `clippy::format_push_string` |
| Escape invisible characters in literals | `invisible_characters`, `clippy::invisible_characters` |

Beyond the Google guide, the workspace also turns on:

- `code_comment`: no `//` or `/* */` comments in Rust code except `// SAFETY:` ones (see below).
- `suppression_policy`: an exception is an `expect` with a reason, written as
  [Silencing a lint](#silencing-a-lint) says.
- `needless_raw_identifier`: no `r#name` unless `name` is a keyword, since a raw identifier
  changes nothing for the compiler but hides the name from checks that read the source text.
- `missing_workspace_lints`: every crate compiles with the workspace lint levels, clippy's default
  groups included. A `RUSTFLAGS` or `.cargo/config.toml` flag that lowers one of the levels it
  samples (clippy's default groups and a few from `[workspace.lints]`) fails; lowering a lint it
  doesn't sample, such as `-A clippy::needless_return`, isn't caught.
- `disabled_code`: nothing that hides code from the checks. No `#[ignore]` on tests and no
  ```` ```ignore ```` doctests; no `cfg` the lint can show is false, such as `all(unix, windows)`;
  no `cfg` on `clippy`, `dylint_lib`, `doc`, `doctest`, `miri`, `proc_macro`, `rustfmt` or
  `rust_analyzer`, which hide code from one of the lint runs; no `cfg` on `debug_assertions`,
  `feature` or `panic`, which the lint runs never build the other way (the one exception is the
  `cfg_attr` that sets `windows_subsystem` in release builds); no `cfg` written as a macro variable;
  and no `include!`. No `cfg` on a target CI doesn't build: only the platforms `ci_targets` in
  `dylint.toml` lists, through `windows`, `unix`, `target_family = "windows"`/`"unix"` and
  `target_os = "windows"`/`"linux"`/`"macos"`, and no predicate that holds on none of them; never
  `target_arch`, `target_env`, `target_abi`, `target_pointer_width`, `target_vendor`,
  `target_feature`, `target_endian` or `target_has_atomic`. Write a fallback as `cfg(not(..))` of
  the systems with their own code, not as an arm per shipped system, so the code still compiles on a
  development machine whose system the project doesn't ship. Each arm of `cfg_select!` follows these
  rules, and an arm no built system selects, such as `_` after `unix` when only Linux is built,
  fails. Call `cfg!` and `cfg_select!` by those names: a `use` that imports either macro, renamed or
  not, fails, while a module or function named `cfg` imports as usual. A `cfg` or `cfg_attr`
  attribute, a `cfg!` or a `cfg_select!` in a string a macro parses as code, such as serde's
  `into = ".."`, fails too. No `cfg!(test)`; `#[cfg(test)]` goes only on a test `mod` or inside
  test code, and `cfg_attr(test, ..)` only inside test code. An `#[ignore]` a macro writes fails
  too. A test attribute, `#[test]` or another crate's test macro such as `#[sqlx::test]`, takes no
  arguments: an argument can set a run count of 0, retry a test until it passes, or run code that
  can exit before the failure is reported, so any argument fails.
- `local_doc_hidden`: no `#[doc(hidden)]` on the project's own items, which would skip
  `missing_docs`. An item a macro needs public takes
  `#[cfg_attr(dylint_lib = "trellis_lints", expect(local_doc_hidden, reason = "..."))]`.
- `clippy::let_underscore_must_use`, `clippy::unused_result_ok` and `discarded_result`: no
  `let _ =`, `_ =` or `.ok();` that discards a `Result` or a `#[must_use]` value, no unread
  `let _name =` binding of a `Result` (or its `.is_ok()`, `.is_err()`, `.ok()`, `.err()`), and no
  `if` or `if let` on one with an empty body and no `else`. A failure is handled or passed on.
- `clippy::pedantic`, and these restriction lints: `allow_attributes` with
  `allow_attributes_without_reason` (use `#[expect(..., reason = "...")]`), `dbg_macro`, `todo`,
  `unimplemented`, `print_stdout`, `print_stderr`, `exit`, `mod_module_files` (name a module
  `foo.rs`, not `foo/mod.rs`), `rc_buffer` and `rc_mutex`.
- These rustc lints: `rust_2018_idioms`, `let_underscore_drop`, `meta_variable_misuse`,
  `missing_debug_implementations`, `redundant_lifetimes`, `single_use_lifetimes`,
  `trivial_casts`, `trivial_numeric_casts`, `unit_bindings`, `unreachable_pub`,
  `unused_lifetimes` and `unused_qualifications`.
- `rustdoc::unescaped_backticks`.
- `trellis-policy check` (`just policy`): every crate has an entry in `internal_dependencies` and
  depends directly only on the workspace crates its entry lists, dev and build dependencies
  included; a crate in `dev_only_crates` is allowed to every crate, but only as a dev-dependency. So
  a new crate or a new edge between crates is a rule change. It also checks that every crate's
  `[lints]` table is exactly `workspace = true`; that no crate has a build script unless
  `build_script_crates` lists it, since a build script can set a `cfg` that hides code from the lint
  runs, and that a listed crate's `build` key names a file in the crate's own directory, in ASCII,
  with no `\`, no part that ends in a dot or a space and no part of the form of a Windows short
  name, such as `CUSTOM~1.RS`. An entry may name a package no crate has,
  so renaming the package of a crate with a build script can land as a rule change, a code change
  and a rule change. Nothing removes a stale entry, and it lets whichever crate later takes the name
  have a build script, though that script still lands in a rule change. It checks that tool
  configuration files exist only at the root, with no variant name that would override them, and
  that each TypeScript settings file, a `.json` file whose name starts with `tsconfig`, reads only
  one way and extends and references only other such files in the repository, by a path that
  starts with `./` or `../`, so every file a `tsconfig.json` reaches is a rule, though a web UI's
  `check` script, which is code, still picks the settings file and flags `tsc` runs with; that the
  root `.gitattributes` sets only `text`, `-text`, `text=auto` and `eol=lf` and defines no
  attribute macro, in the working tree and in git's index, so a checkout holds the bytes the diff
  showed, apart from line endings, where another attribute such as `working-tree-encoding`,
  `ident` or `filter` could have git write other bytes; that the workspace's members are exactly
  the packages one directory below
  `crates/`, each in a directory with an ASCII name that Windows reads as written, with no symbolic
  link for `crates` or a crate's directory, since which changes are rules depends on that place;
  that no two paths in the repository are one path on macOS or Windows, whose file systems ignore
  letter case in any script and, on macOS, whether an accented letter is one character or two,
  since a checkout there holds only one of them; that no path has a part of the form of a Windows
  short name, such as `cargo~1.tom`, which is at most eight characters that end in `~` and digits,
  then at most three after a dot, since Windows opens `Cargo.toml` under that name and a checkout
  of both there can hold one file, with the other's contents; that the workspace excludes
  nothing, pulls in no non-member crate by path, and loads only the trellis lints as its dylint
  library; that each crate's `Cargo.toml` holds only allowed keys (no `test = false`,
  `harness = false` outside a `[[bench]]`, `auto*` or `required-features`), inherits `edition`,
  `rust-version` and `publish`, and `license` when the workspace has one, and keeps its sources,
  `#[path]` files included, inside the crate, with no symbolic link in or above its directory;
  that the repository holds no git submodule, or other git repository, anywhere, since a checkout
  that initializes one holds files no check or review read; that
  `.cargo/config.toml` holds only the Windows `+crt-static` flags; that a crate depends directly
  only on proc-macro crates listed in `proc_macro_crates`, since a proc macro writes code the custom
  lints don't check; that the root `AGENTS.md` is the only file of instructions for coding agents;
  that every crate's commits reach the release notes through `changelog_include` in
  `release-plz.toml`; and that the docs keep the layout under [Documentation](#documentation).
- `overflow-checks` and `panic = "abort"` in release builds: an overflow stops the program rather
  than wrapping, and nothing catches a panic. `trellis-policy` fails a change that turns either
  off, including a per-package or `.cargo/config.toml` override.
- `trellis-policy rule-changes` (`just rule-changes`): a pull request changes the rules or the code,
  never both, so review sees every loosened rule apart from the code that might have needed it. A
  crate's `Cargo.toml` is code, except the part of a change that touches its `[build-dependencies]`,
  its `build` key or its `license`, which is a rule, so a build script and its dependencies land
  together, and so does a change of license with each crate's line. The file a `build` key names is
  a rule too, though the modules a build script reads are code. A crate whose directory moves whole,
  renamed with no file in it changed, is neither rule nor code, so a crate can move beside the
  `path` of each dependency on it. A file in the crate that reads its own location, such as a build
  script that reads `CARGO_MANIFEST_DIR`, can then do something else, as a code change can change
  what a listed build script emits. `.gitignore`, `.ignore`, `bunfig.toml`, `.npmrc` and every
  TypeScript settings file are rules in any directory, since they change what a check sees, and
  so is a git submodule, whose files no check sees. A path that Windows reads as another path is
  a rule too: one with a part that ends in a dot or a space, that holds a `:` or a `\`, or that
  has the form of a short name. A web UI's
  `package.json`, its scripts and its sources are code. A proc-macro dependency is code: a rules
  pull request lists the crate in `proc_macro_crates`, and a code pull request then adds the
  dependency with its first use, since `cargo shear` rejects a dependency nothing uses. A `.rs`
  change that only adds, removes or moves `expect` attributes is neither, so a new lint lands with
  the `expect`s it needs, and a code change with the `expect`s it forces elsewhere.

## Silencing a lint

Put an `expect` with a reason on the smallest item the lint applies to:

- a clippy or rustc lint: `#[expect(clippy::lint_name, reason = "...")]`;
- a custom lint: `#[cfg_attr(dylint_lib = "trellis_lints", expect(lint_name, reason = "..."))]`.
  The `cfg_attr` keeps ordinary builds, which don't know the lint, from warning about it.
- private code that only its tests use until its first caller lands:
  `#[cfg_attr(not(test), expect(dead_code, reason = "..."))]`. A plain `expect(dead_code, ..)`
  goes unmet in the test build, where the tests use the code.

An `expect` fails once its lint stops firing, so a stale exception doesn't linger. Nothing else
silences a lint, and these all fail `just dylint`. `suppression_policy` reads the source text, so
a `cfg_attr` that only one of the clippy and dylint runs sees can't hide any of them.

- `allow` and `warn`, and every inner lint attribute (`#![...]`), at the top of a crate or module
  or inside a function.
- An `expect` without a reason, or one that names a lint group, such as `warnings` or
  `clippy::pedantic`, rather than single lints.
- A reason with fewer than 20 characters or fewer than 4 different words besides the words of the
  lint's name, so `"x"` or the lint's name spelled out fails.
- Any `#[clippy::...]` attribute, such as `#[clippy::msrv]` or `#[clippy::cognitive_complexity]`,
  which changes what clippy checks without an `expect`. Set the Rust version with `rust-version`
  and thresholds in `clippy.toml`.
- A `cfg_attr` around a lint level in any other form than the two above,
  `cfg_attr(dylint_lib = "trellis_lints", expect(lint, reason = "..."))` and
  `cfg_attr(not(test), expect(dead_code, reason = "..."))`.
- A lint-level attribute on a `mod`, other than an `expect` naming only `file_named_after_item` or
  `non_leaf_module_items`, or on an `impl` block or a trait for a lint that doesn't report at the
  block. An `expect` of those two lints may go on no item with a module beneath it, in a function
  body or below a `#[cfg(test)]` module too, whose reports it would cover as well. A `#[cfg(test)]`
  module that holds no module may be beneath it.
- A lint attribute built from macro variables or assembled by a macro: a `#` that doesn't start
  an attribute, an attribute that doesn't start with a path (`paste`'s `[<...>]`), or brackets
  after a `$name:tt` variable.
- After expansion, an `allow`, `warn`, inner lint level, or `expect` without a reason or naming a
  group that a macro writes, unless `trusted_macro_crates` in `dylint.toml` lists the crate that
  defines the macro (`serde_derive`, not `serde`). Even then, a lint level a trusted macro writes
  over code written in the source may name only lints `trusted_macro_lints` in `dylint.toml` lists
  for that crate, and never a group, so a macro can't switch clippy off for the code passed to it,
  whatever that code is, a literal or a path included. So trellis trusts no macro that writes lint
  levels over code passed to it, such as clap's derives, `#[tokio::main]`, `#[tokio::test]` and
  `#[tracing::instrument]`; the [idioms](#idioms-under-these-rules) replace them.

Some lints have no exceptions, and an `expect` naming one fails: `code_comment`,
`suppression_policy`, `missing_workspace_lints`, `disabled_code`, `doc_after_attribute` and
`needless_raw_identifier` (fix the code), and `macro_definition` (a macro goes only in a crate that
`macro_crates` in `dylint.toml` lists).

## Checked in review

No check decides these, so review does: check every change against this list, and the
[docs review checklist](#docs-review-checklist), before it's committed.

- **Ownership.** Each `Rc` or `Arc` the `shared_ownership` lint makes you justify is really
  needed, and no `.clone()` stands in for a borrow.
- **Functions.** Return iterators rather than `Vec` where that is just as simple. Use `Option`
  for simple failures, `Result` when the caller needs to know why, and a panic only for a bug or
  a broken invariant. Use an `enum` rather than a `bool` parameter whose meaning isn't obvious at
  the call.
- **Types and traits.** A newtype rather than an alias whenever the meaning differs, not only for
  primitives. Add trait implementations when they are needed. Generics rather than `dyn Trait`
  unless code size matters. No abstraction before a second caller needs it.
- **Naming.** Names say what something is, without invented abbreviations. Generic parameters are
  short and reflect their bound.
- **Docs.** Public items have docs in full sentences that say what the code can't.
- **Exceptions.** Every `expect` gives a reason that holds up; a reason that only restates the
  lint is a finding.
- **Unsafe.** Each use says why safe code can't do it.
- **Tests.** New behavior has a test that would fail without it, and no test asserts the opposite
  of the behavior it claims to check or narrows itself to one platform to dodge a failure.
- **Other features.** Mark a function `const` only if it will stay `const`. Annotate the types of
  long iterator chains.

## Idioms under these rules

Write these like this rather than inventing a workaround:

- **Formatting traits.** Import the traits and types and keep the `fmt` module for its result
  alias: `use std::fmt::{self, Display, Formatter};`, then `impl Display for Order` and
  `fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result`. `qualified_type_path` rejects
  `fmt::Display` and `fmt::Formatter`, and allows `fmt::Result` because `Result` is ambiguous.
- **Matching `#[non_exhaustive]` enums.** List every variant you ignore before the wildcard, as the
  guide asks, even when that is many; it is what makes a new variant visible in review.
- **Hex literals.** Group the digits by byte, such as `0x1e_1e_2e`. `clippy::unreadable_literal`
  rejects a long literal without separators.
- **Boxed callbacks.** Name a boxed callback's type with an alias, such as
  `type Handler = Box<dyn Fn(&str, u32) -> Result<Vec<u8>, String> + Send + Sync>;`.
  `clippy::type_complexity` rejects a type that complex spelled out in a field. A closure has no
  `Debug`, so a public struct that holds one implements `Debug` by hand for
  `missing_debug_implementations`: print the other fields and end with `finish_non_exhaustive()`,
  which `clippy::missing_fields_in_debug` accepts.
- **Command lines.** Build the parser with clap's builder API: a function returning
  `Command::new(..)` with an `Arg::new(..)` per option, `clap::value_parser!(T)` for typed values,
  and a function that reads the `ArgMatches` into a struct. Implement `clap::ValueEnum` by hand for
  an enum of choices. clap's derives write lint groups over the expressions in `#[arg(..)]`.
- **Async runtimes.** Build the runtime in `main`:
  `Builder::new_multi_thread().enable_all().build()?.block_on(run(cli))`, with
  `tokio::runtime::Builder` imported. An async test is a `#[test] fn` that passes its `async`
  block to a small `block_on` helper building a current-thread runtime. `#[tokio::main]` and
  `#[tokio::test]` write `allow`s over the function's body.
- **Waiting on several futures.** Use the `futures` crate's functions, not macros:
  `future::join(a, b)`, `future::try_join(a, b)`, `future::join_all(futures)` and
  `future::select(a, b)`, which takes pinned futures and returns an `Either`, with
  `futures::future` imported. Spawn a varying number of tasks into a `tokio::task::JoinSet`, and
  bound a wait with `tokio::time::timeout`. `tokio::select!`, `tokio::join!` and
  `tokio::try_join!` write `allow`s over the code passed to them.
- **Spans.** Enter spans by hand: `let _span = tracing::info_span!("load").entered();` in a
  synchronous function, and `future.instrument(tracing::info_span!("load"))`, with
  `use tracing::Instrument as _;` in the function, for a future. `#[tracing::instrument]` writes
  `allow`s over the function's body.
- **Error messages.** Write a thiserror message as text that names fields inline, such as
  `#[error("no user {name}")]`. A format argument after the message, `#[error("{}", .name.trim())]`,
  fails, since thiserror evaluates it under `allow`s of its own, `deprecated` among them; give the
  error a field that holds the value instead.
- **Serde strings.** Name a function in a serde string, such as `#[serde(default = "..")]`,
  `deserialize_with` or `skip_serializing_if`, through its module or type, as the guide qualifies
  a call: `#[serde(default = "defaults::retries")]` with `use crate::defaults;`, or
  `skip_serializing_if = "Option::is_none"`. A function defined in the same module may go by its
  bare name, and `with` takes a module by its bare name. Write the path as plain text: two
  segments at most, no `crate::`, `self::` or `super::`, no turbofish, and no other way into the
  module's or the function body's scope for the last name, such as importing the function as
  well or a glob import that brings it. A type or bound in a string, such as
  `try_from = "Vec<u8>"` or `bound(deserialize = "T: Deserialize<'de>")`, follows the same rule
  for each path in it, generic arguments included, so `Vec<std::string::String>` or
  `T: crate::limits::Bounded` fails. serde writes these paths under
  `allow(unused_qualifications, clippy::absolute_paths, ..)`, so anything those lints could
  report fails. A bare name the module imports fails `qualified_function_calls`.
- **Pinning.** Pin a future with `std::pin::pin!`. `tokio::pin!` writes `allow(unused_mut)` over the
  caller's `let`.

## Where trellis goes further than the guide

- **No code comments.** The guide allows `//` comments; trellis doesn't, except `// SAFETY:`. A
  comment drifts from the code, and it is where a workaround gets justified instead of fixed. A
  coding agent that reads a justified workaround copies it to the next place that looks alike,
  rather than finding the best fix there. The why goes where it is checked or reviewed with the
  code: a doc comment, a name, an assertion message, or the `reason` of an `#[expect(...)]`.
  Configuration files keep their comments.
- **Exceptions carry reasons.** Rules the guide leaves to judgment, such as `Rc` and `Arc`,
  `impl Into<T>` on a library API and `#[rustfmt::skip]`, are linted, and each justified use carries
  `#[expect(lint, reason = "...")]`, so every exception is written down where review sees it.
- **Formatting.** The guide's rule that a `use` statement imports items from one module needs
  rustfmt's `imports_granularity`, which only nightly rustfmt has. The guide lets a codebase on
  stable Rust skip `format_macro_matchers`, also nightly-only, and trellis applies it anyway.
  `just fmt` uses the nightly pinned in the `justfile`, while the code itself builds on stable.
- **Assertion messages.** The guide asks you to consider a message on every assertion.
  `clippy::missing_assert_message` requires one outside tests.

## Rules that don't apply

- **`#[non_exhaustive]` in a binary or service.** The guide asks for it on public enums and
  structs that other teams use. A binary or service has no such users, so
  `clippy::exhaustive_enums` and `clippy::exhaustive_structs` are on only in a library.
- **API design in a binary or service.** The guide's API design rules bind a library and guide an
  application.
- **Codebase-specific rules.** Rules tagged for Chromium, Android or Fuchsia (no `async`, no
  `std::thread`, flat source layout, small crates, their test frameworks) are for those
  codebases.
- **Lints that would push against the guide stay off:** `clippy::clone_on_ref_ptr`,
  `clippy::redundant_type_annotations`, `clippy::missing_const_for_fn`, `clippy::expect_used`,
  `clippy::panic`, `clippy::single_char_lifetime_names`, `clippy::string_add` (the guide asks
  for `format!` only when joining more than two strings) and `clippy::enum_glob_use` (it also
  flags enum globs inside functions, which the guide allows; `enum_glob_outside_fn` replaces it).

## Documentation

A trellis project's docs have one shape: a short README, an index in its Documentation section,
and a page in `docs/` for each task a reader has.

### Docs checked by `just policy`

`trellis-policy`'s `docs_index` and `docs_layout` rules check these, with `docs_files` and
`readme_sections` in `[workspace.metadata.policy]`. They read Markdown as GitHub renders it:
text in code, in HTML or in an HTML comment is neither a heading nor a link.

- Every file `docs_files` lists exists: `README.md`, `AGENTS.md`, `CONTRIBUTING.md`,
  `SECURITY.md`, `CHANGELOG.md`, the license files and the pages in `docs/`. A missing setting
  fails rather than checking nothing.
- The root `README.md` has exactly one level-1 heading, its title, written as a `#` line with
  text, outside any list or quote, with only badges, comments and blank lines before it.
- Its top-level `##` headings are exactly the sections `readme_sections` lists, in order, each
  written as plain text, such as `## Install`. It has no `##` heading inside a list item, quote
  or footnote, no setext heading (a line underlined with `=` or `-`) and no HTML heading tag such
  as `<h2>`.
- The Documentation section is an index and nothing else. Each entry is a one-line list item: a
  link written exactly `[Title](path)` with a relative path, then `: ` and a description that
  has a letter and ends in `.`, such as `- [Title](path): what the reader does with it.` Every
  page directly under `docs/` has an entry.
- A page in a subdirectory of `docs/` is linked from a page in the directory above it, with any
  link GitHub renders.
- Every page, at any depth under `docs/`, starts with a `# Title` line, after any comments,
  then a blank line and a paragraph with a letter in it outside images. Pages use the `.md`
  extension, and neither the README nor a page starts with a byte order mark.
- No HTML in the README or a page leaves a comment, a tag or a quoted attribute value open, and
  every HTML element opened in a section closes in the same section, so nothing that follows is
  hidden or rendered inside it. Comments may come before the title and in the index.

### Docs review checklist

No check decides these, so review does.

- **A short README.** Anything longer than a few commands moves to a page in `docs/`.
- **One page per task.** Each page covers one task a reader has, such as installing, verifying a
  release or configuring, and its index entry says what the reader does with it.
- **Page shape.** The paragraph after the title says who the page is for and what it gets them.
  Numbered steps with commands to copy follow, then reference tables if the page needs them.
- **Voice.** Short declarative sentences, in the second person and the present tense. No
  marketing.
- **Where things go.** Design decisions go in `docs/design.md`, and compatibility promises in
  `docs/stability.md`. release-plz writes `CHANGELOG.md`, which isn't edited by hand except as the
  release steps in `docs/maintaining.md` say.
- **Same pull request.** A change to user-facing behavior updates its page in the same pull
  request, and a change to the design updates `docs/design.md`.
