# Lints and formatting

The tools that enforce the code rules, formatting, and lints with their allowances. It is part of `docs/code-quality.md`.

## Tools enforce the rules


A rule a tool can check fails CI. It is not a sentence a reviewer is trusted
to remember. The archived tree wrote down that core must not name built-in
tools and broke the rule 211 times
([ADR 0002](../adr/0002-module-boundaries-are-crate-boundaries.md)). A check is named for what it checks: the archived tree's
"public-surface audit" checked for leaked paths, not public items
([fiber-zig#446](https://github.com/aakshintala/fiber-zig/issues/446)).

When more than one mechanism can enforce a rule, the strongest wins: a type
that cannot hold the bad value, then a lint or check that fails CI, then one
shared function every caller goes through, then a check at run time. Agents
copy the guard the surrounding code already uses, so a weak one spreads.

A doc table that mirrors the code has a check that fails when the two
differ. The `unsafe` table below and the crate list in `docs/dependencies.md`
are two. A new table of that kind comes with its check.

What no tool can check is in "What a reviewer checks", and nowhere else.

## Formatting


rustfmt with its default settings. CI runs `cargo fmt --check`.

## Lints


Lints are set once, in the workspace `Cargo.toml` under `[workspace.lints]`,
and every crate inherits them with `lints.workspace = true`. A lint set to
deny fails the build. Every other warning, rustc's default-warn lints
included, fails the gate: `scripts/check` runs clippy with `-D warnings`.
A local build still only warns.

Denied everywhere:

- every lint in clippy's default set (`clippy::all`)
- code that can panic: `unwrap_used`, `expect_used`, `panic`, `todo`,
  `unimplemented`, `indexing_slicing` (write `v.get(3)`, not `v[3]`) and
  `unchecked_time_subtraction`
- debugging left behind: `dbg_macro`, and `print_stdout` and `print_stderr`
  outside `main` and `doors`
- silent number conversion: `cast_possible_truncation`, `cast_sign_loss` and
  `cast_possible_wrap`. Fiber counts bytes against caps and mints `seq`; use
  `u32::try_from(x)`, which fails, rather than `x as u32`, which chops
- dropped errors: `let_underscore_must_use` and `unused_result_ok`.
  `let _ = file.sync_all();` discards an fsync failure, and the log's
  durability rests on fsync
- hash-map order: `iter_over_hash_type`. A `HashMap` iterates in a different
  order every run, and no hash-map order may reach a request
  (`docs/prompt-cache.md`). Use a `BTreeMap`, or sort first
- a `_` arm in a `match` on an enum, so a new variant fails to compile until
  every `match` handles it. Three lints cover it: `wildcard_enum_match_arm`
  when the `_` stands for several variants, `match_wildcard_for_single_variants`
  when it stands for one, and rustc's `unreachable_patterns` when it stands
  for none. `Option` and `Result` are exempt; they never grow. The lints
  also fire on another crate's `#[non_exhaustive]` enum, such as
  `std::io::ErrorKind`, where a `match` must keep its `_` and takes an allow
  with a reason. `==`, `matches!` and `if let` trip none of them, so a test
  for one variant needs no allow. A match that handles one variant and
  ignores the rest uses `if let` or `matches!`; the lints are for matches
  that must grow with the enum
- visibility and documentation: rustc's `unreachable_pub` and `missing_docs`
  (see "Visibility" and "Comments")
- `unsafe_code` and `undocumented_unsafe_blocks` (see "`unsafe`")
- an allow with no reason: `allow_attributes_without_reason`
- a lint name the toolchain does not know: rustc's `unknown_lints` and
  `renamed_and_removed_lints`, so a toolchain update that renames a lint
  fails instead of silently dropping it
- reading or waiting on the real clock: `clippy.toml` disallows
  `std::thread::sleep`, `Instant::now`, `SystemTime::now` and both
  `elapsed` methods. Code reads and waits on an injected
  `contract::clock::Clock` instead, so a test drives time
  (`docs/testing.md`, "Values that change every run")

Test code may unwrap, expect, panic and index. `clippy.toml` sets
`allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests` and
`allow-indexing-slicing-in-tests`.

Turning a lint off for one item takes a written reason:

```rust
#[allow(clippy::indexing_slicing, reason = "len checked on the line above")]
```

The rest of clippy's pedantic group is not enabled. Most of it is taste, and
agents would churn code to satisfy it.
