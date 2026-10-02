# Code quality

What Fiber's Rust code must satisfy to merge, beyond passing its tests. This
is what is true now, not a plan. It is settled by
[Code quality: what makes Rust code mergeable](https://github.com/aakshintala/fiber/issues/64);
that ticket's resolution holds the rationale and the rejected alternatives.

It covers Fiber's own code. An extension's Lua or a process extension's
program is its author's. How tests are written is `docs/testing.md`, which
crates may be used is `docs/dependencies.md`, and who implements and reviews
a change is `docs/workflow.md`.

Code under `research/` is throwaway. It is not a member of the Cargo
workspace, and neither the gate nor the `CI` check builds, lints or tests it.

## Tools enforce the rules

A rule a tool can check fails CI. It is not a sentence a reviewer is trusted
to remember. The archived tree wrote down that core must not name built-in
tools and broke the rule 211 times
([ADR 0002](adr/0002-module-boundaries-are-crate-boundaries.md)). codex asks
for Rust files under 800 lines in prose, and 270 of its 3,009 source files
are longer. A check is named for what it checks: the archived tree's
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
deny fails the build.

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
  for one variant needs no allow
- visibility and documentation: rustc's `unreachable_pub` and `missing_docs`
  (see "Visibility" and "Comments")
- `unsafe_code` and `undocumented_unsafe_blocks` (see "`unsafe`")
- an allow with no reason: `allow_attributes_without_reason`
- a lint name the toolchain does not know: rustc's `unknown_lints` and
  `renamed_and_removed_lints`, so a toolchain update that renames a lint
  fails instead of silently dropping it

Test code may unwrap, expect, panic and index. `clippy.toml` sets
`allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests` and
`allow-indexing-slicing-in-tests`.

Turning a lint off for one item takes a written reason:

```rust
#[allow(clippy::indexing_slicing, reason = "len checked on the line above")]
```

The rest of clippy's pedantic group is not enabled. Most of it is taste, and
agents would churn code to satisfy it.

## Panics

A panic means Fiber has a bug. A condition that can happen at run time, such
as a missing file, a refused connection or a malformed response, returns an
error.

Release and debug builds set `panic = "abort"`, so the binary tests start
behaves like the one that ships. A panic ends its process, which is one
session ([ADR 0009](adr/0009-each-session-is-one-process.md)), and resuming
continues from the log, which is fsynced around every side effect
([ADR 0001](adr/0001-session-log-is-the-only-state-of-record.md)). Under
unwind, a panic inside a function that a Lua extension calls unwinds through
Lua's frames, past the extension's `pcall`, to the Rust caller, and leaves any
mutex the callback held poisoned. Abort ends the process at the panic, so no
code runs on half-changed state. Probed with mlua 0.12 on Linux x86_64
([research/extension-runtime/linux-containment](../research/extension-runtime/linux-containment/README.md)).
The lints above make code that can panic fail to compile, so the bar holds
for host callbacks along with everything else.

Release builds also set `overflow-checks = true`, so an integer overflow
panics instead of wrapping to a wrong value. Code that wants wrapping says so
with `wrapping_add` and its siblings.

Measured on macOS arm64 with every runtime crate linked: the binary is
3.17 MiB with unwind and 2.68 MiB with abort.

### What a panic leaves

Before the process aborts, a panic hook:

1. Restores the terminal, when the process has one.
2. Writes the message, the thread's name and a backtrace to
   `crashes/<session_id>-<ms>.txt` in Fiber home (`docs/state.md`). The TUI
   uses the id of the session it is attached to.
3. Prints the same report and the file's path to stderr.

The hook runs on the thread that panicked and needs no other thread, so it
works when the log's own thread is the one that failed. It takes no lock.

A crash file describes a bug in Fiber, not the session. Nothing reads it to
decide anything: a session's log with no `fiber_exited` is what records that
its process died (`docs/events.md`). A second panic inside the hook, running
out of memory, or a full disk leaves no file.

## Errors

Each library crate has its own error enum, built with `thiserror`. A
function that crosses a crate boundary returns that enum, never a
type-erased error such as `Box<dyn Error>`. A bounded set of failures is
named case by case rather than widened into one catch-all case. A crate
that no release binary contains, such as `fakes`, may return std errors
instead of its own enum.

The crate that defines an enum maps each case to a stable code, with no
wildcard arm. The codes and the mapping rule are `docs/errors.md`.

## `unsafe`

`unsafe` is denied in every crate. A block that needs it, such as a call
between `fork` and `exec` that the standard library allows only through
`CommandExt::pre_exec`, turns the lint off for that item with a reason and
carries a `// SAFETY:` comment saying why the block is sound.

Every place Fiber's own code uses `unsafe` is listed here, and CI fails when
the code and the list disagree.

| Crate | File | Why |
|---|---|---|
| none yet | | |

`unsafe` inside dependencies is `docs/dependencies.md`'s.

## Types

Each kind of identifier is its own newtype, even when two share a
representation. A session id, a `seq` and a tool call id are distinct types,
so passing one where another belongs does not compile.

Fields that describe mutually exclusive states are an enum, not a struct
whose fields can contradict each other: `done: bool` beside
`done_at: Option<Timestamp>` admits "done, at no time", where
`enum Status { Open, Done { at: Timestamp } }` cannot. A runtime check for a
combination "that cannot happen" marks a type that is too loose.

## Visibility

An item is private, or `pub(crate)`, unless another crate uses it. rustc's
`unreachable_pub` lint enforces this. Which crate may call which is
`docs/architecture.md`, and the compiler enforces that
([ADR 0002](adr/0002-module-boundaries-are-crate-boundaries.md)).

## Size

CI fails a source file over 800 lines. Unit tests live in their own file, a
`tests.rs` or `<name>_tests.rs` included with `#[cfg(test)] mod tests;`, so a
file's length is its code. A test file, named `tests.rs` or ending in `_tests.rs` or under a crate's `tests/` directory, has no cap. 800 is codex's own target,
picked rather than measured.

## Comments

Agents and future contributors are the readers. The owner does not read the
code.

- Every crate opens with a `//!` comment saying what it owns and naming the
  `docs/<area>.md` page it implements.
- Every public item carries a `///` comment. rustc's `missing_docs` lint
  enforces this.
- Other comments say why, and only where the code would surprise a careful
  reader.
- No comment narrates history: no "changed from", "previously", "now uses" or
  "fixed the bug where". Change the code and leave no trace. History is git's.

## Prose

Code, output and docs contain no emojis. Unicode symbols such as a check mark
or an arrow are fine. A double hyphen (`--`) is never a dash.

## Threads

Before joining a thread, release every lock, lease or channel that thread may
be waiting on. A join ahead of the release it needs deadlocks only under
load ([fiber-zig#352](https://github.com/aakshintala/fiber-zig/issues/352)).

## What a reviewer checks

Only what no tool can:

- the code does what its `docs/<area>.md` page says. When they disagree, the
  doc wins and the code changes
- names match `GLOSSARY.md`
- error messages and event payloads read clearly to their consumer
- every lint allow, `unsafe` block and mutation-test exemption gives a reason
  that holds
- comments say why, not what happened
- a fix sits where the wrong value is made, not where it is noticed, and the
  same mistake elsewhere is fixed in the same pull request
- no struct whose fields can contradict each other (see "Types")
- a new member of a closed set of named cases, such as an event kind, a
  mode or a state, is a variant or a table entry, not one more branch on an
  existing `if`/`else` chain or a second flag that must stay in step with a
  first
- code that works around its doc is reported as a doc finding that cites
  the doc rule and the code, for `docs/workflow.md`, "When a doc should
  change", not as a code fix
- no function with one caller whose body only passes its arguments on to
  another function, and no Fiber-defined trait with one implementation
  other than the three seams (`docs/architecture.md`)
- no code that a standard-library feature, a lint or a crate listed in
  `docs/dependencies.md` already covers. A replacement that adds lines, memory
  or behaviour is not a simplification
- every code a doc says something fails with, is rejected with, or carries
  as a notice's code has a row in `docs/errors.md`, "Registry", or its
  notices table
