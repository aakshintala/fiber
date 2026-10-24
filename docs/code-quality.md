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
workspace, and neither the gate nor the `CI` check builds or tests it. The one
exception is `research/tui-prototype`, the look reference for the terminal UI,
which `scripts/check` formats and lints when a diff touches it (`docs/ci.md`,
"On every pull request that changes code").

Parts. Read the part a job needs:

- `docs/code-quality/lints.md`: the tools that enforce the rules, formatting, lints and allowances.

## Tools enforce the rules

In `docs/code-quality/lints.md`.

## Formatting

In `docs/code-quality/lints.md`.

## Lints

In `docs/code-quality/lints.md`.

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
code runs on half-changed state. It also means no design can rely on code
running after a panic: cleanup on drop, `catch_unwind` and lock-poison
recovery never run on one. Probed with mlua 0.12 on Linux x86_64
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
   uses the id of the session it is attached to. A process with no session
   id yet, such as the hub or a panic before a session starts, uses its
   process id, as its diagnostic log does.
3. Prints the same report and the file's path to stderr.

The hook runs on the thread that panicked and needs no other thread, so it
works when the log's own thread is the one that failed. It takes no lock of
Fiber's. The only locks it touches are the standard library's, for capturing
the backtrace and for writing to stderr.

A crash file describes a bug in Fiber, not the session. Nothing reads it to
decide anything: a session's log with no `fiber_exited` is what records that
its process died (`docs/events.md`). `fiber doctor` names the newest one, and
old ones are pruned with the diagnostic logs (`docs/state.md`, "What each part
holds"). A second panic inside the hook, running
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
`support` names no stable codes; a caller that reports one of its errors
maps each case to its own code, with no wildcard arm.
A tool that reports another crate's error in a tool call maps each case to
a code `docs/errors.md` allows for tool calls, with no wildcard arm, since
the defining crate's codes may belong to driver and hub commands.

A type that holds a secret prints it redacted in `Debug`, including header
values, `env` values and command arguments. A key leaves `Secret` only on the
line that sends it.

## `unsafe`

`unsafe` is denied in every crate. A block that needs it, such as a call
between `fork` and `exec` that the standard library allows only through
`CommandExt::pre_exec`, turns the lint off for that item with a reason and
carries a `// SAFETY:` comment saying why the block is sound.

Every place Fiber's own code uses `unsafe` is listed here, and CI fails when
the code and the list disagree.

| Crate | File | Why |
|---|---|---|
| `support` | `crates/support/src/group.rs` | `pre_exec` calls `setsid`, and for a `tty` command the `TIOCSCTTY` ioctl on fd 0, between fork and exec |
| `support` | `crates/support/src/peer.rs` | on macOS, `getsockopt` with `LOCAL_PEERPID` reads the socket peer's pid from a borrowed fd, and `sysctl` with `KERN_PROC_PID`/`KERN_PROCARGS2` reads one process's parent pid, effective uid and argument vector into caller-sized buffers |
| `fakes` | `crates/fakes/src/crash_ports.rs` | on macOS, `pre_exec` calls `task_set_exception_ports` so a re-run child's deliberate `abort()` writes no ReportCrash report; its test reads the port back with `task_get_exception_ports` |
| `log` | `crates/log/src/diag/memory.rs` | on macOS, `proc_pid_rusage` reads the peak physical footprint for a `peak_memory` line |
| `cli` | `crates/cli/src/stop_tests.rs` | the close-wait test wraps a write-only file fd as a `UnixStream`, forcing the read error the wait reports as lost |
| `fakes` | `crates/fakes/src/alloc.rs` | the counting allocator implements `GlobalAlloc`, an unsafe trait, and forwards every call to `System` |

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

A source file over 800 lines is a smell: a god file is forming, and its
logic belongs in deeper modules split at real boundaries. `cargo xtask
line-cap` lists every non-test source file over 800 lines. CI does not run
it; it is the audit's counting tool, so every audit counts the same way. A
pull request that adds a new file over 800 lines splits it in the same pull
request; the reviewer checks this, and no gate does.

Each scheduled audit of the code first ranks source files by how often a
ticket waited because an open pull request was changing that file, and
examines the files at the top of that ranking. A file that holds work back
costs more than a long file nobody else touches. The audit then posts a
verdict for every file `cargo xtask line-cap` lists, with its line count: a
ticket labelled `split` that names the piece to split out, or "stays" with
the reason. A file that has grown since its last verdict is examined again.

The split separates what the file owns into modules with their own
interfaces. Lines moved into another file that still reach the parent's
private state, or a trim to get under the line, are not a split. Neither is
a type invented only to get under the line. A file that is cohesive and has
no boundary with state of its own stays over 800 lines. 800 is picked rather
than measured.

A module lives at its standard path: `src/<parent>/<name>.rs` beside
`src/<parent>.rs`. `#[path]` names only a unit-test file, as below, or a
shared test support file that a test or an example includes. Some older
modules still use `#[path]`; a change that splits one moves it to its
standard path.

Unit tests live in their own file, a `tests.rs` or `<name>_tests.rs`
included with `#[cfg(test)] mod tests;`, so a file's length is its code. A
test file, named `tests.rs` or ending in `_tests.rs` or under a crate's
`tests/` directory, is never counted.

A feature joins the app through a registration seam, so features stop
colliding on one file: a built-in command is one table row; a view's
state is its own struct behind one `App` field; a feature's tests go in
a file named for the feature.

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
- in each test the pull request adds or changes: every asserted order or
  count cites the doc line that promises it (`docs/testing.md`, "Event
  streams"); every pointer input follows a readiness signal, not a grid
  match (`docs/testing/screens.md`); every fake's signal is sent after the
  state it announces

Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md#from-docscode-qualitymd).
