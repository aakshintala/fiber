# Lua containment on Linux x86_64

Question: do the containment claims in `docs/extensions.md` ("When an extension
misbehaves") hold on Linux x86_64 with the real mlua and Lua 5.4? The earlier
measurements in `research/extension-runtime/` were macOS arm64. Ticket: #90.

## Method

- `src/main.rs` is a standalone probe: mlua 0.12, features `lua54`, `vendored`,
  `send` (the same as `pass1/lua`), the same stripped stdlib (no io, os,
  package or debug). `containment run` spawns each case as its own child
  process and records exit code, signal and output, so an abort shows as a
  signal. A case that runs 6 s is killed and marked `TIMEOUT`.
- Four build profiles (`run.sh`): `dev` and `release` set `panic = "abort"`, as
  `docs/code-quality.md` says Fiber's profiles do. `dev-unwind` and
  `release-unwind` set `panic = "unwind"` to show what the abort setting changes.
- Linux: GitHub Actions `ubuntu-24.04`, kernel 6.17.0-1022-azure, rustc 1.98.1,
  2026-09-29 (`raw/linux-uname.txt`). The workflow is `probe.yml` here (it ran
  from the temporary branch `probe/90-lua`, now deleted). macOS: Darwin 25.6.0
  arm64, rustc 1.98.1, same day.
- Raw output: `raw/linux-x86_64.txt`, `raw/macos-arm64.txt`. One line per case
  and profile, timings removed: `raw/*.summary.txt` (made by `summarize.py`).
- The Linux and macOS summaries are identical, apart from the panic message's
  thread id and line numbers. Nothing here differs by platform.

## 1. A Rust panic inside a Lua callback

A host function registered with `create_function` panics. Called bare, under
`pcall`, and under `pcall` inside `coroutine.wrap`.

- `panic = "abort"` (`dev`, `release`): the process aborts with SIGABRT in all
  three, before the caller sees anything. Lua's `pcall` never returns.
- `panic = "unwind"` (`dev-unwind`, `release-unwind`): the panic unwinds through
  Lua's C frames to the Rust caller as a panic, in all three. It does not
  become a Lua error and `pcall` does not catch it. The VM answers `1+1`
  afterwards.
- A mutex held by the panicking callback is poisoned under unwind
  (`panic_lock`), and abort ends the process before that matters.

Rows: `panic_*` in `raw/linux-x86_64.summary.txt`.

## 2. Deadline through `pcall` and coroutines

A watchdog thread sets a flag 100 ms in. The script retries
`pcall(function() while true do end end)` 50 times.

| Hook | Result |
|---|---|
| single stage (`every_nth_instruction(1000)`, errors once the flag is set) | script returns 50: `pcall` swallowed the deadline every time |
| two stage (pass1 `interrupt_escalate`), on the main thread | `deadline exceeded` error at about 100 ms |
| two stage, script creates a coroutine with `coroutine.wrap` and loops inside it | never stops (`TIMEOUT`) |
| two stage, coroutine created by Rust before the hook is armed, resumed | never stops |
| two stage, coroutine yields, is resumed after the deadline, then loops | never stops |
| escalation inside a coroutine, retry loop outside | never stops |
| a fresh `Thread` per attempt, resumed from Rust | never stops |
| `Thread::set_hook` with a single-stage hook | returns 50 (swallowed) |
| `Thread::set_hook` with a two-stage hook that re-arms `lua.current_thread()` | `deadline exceeded` at about 100 ms |
| host-supplied `coroutine.wrap` that arms that hook on each new `Thread` | `deadline exceeded` at about 100 ms |

`Lua::set_hook` arms only the main Lua thread. A coroutine has no hook,
whether it was created before or after the hook was set, so a loop inside one
runs past the deadline. The doc's two-stage design holds when armed per
coroutine; the fix is to arm it with `Thread::set_hook` on every coroutine the
callback runs on, and to replace `coroutine.wrap` so ones the script creates
get it too. Only `coroutine.wrap` was measured; `coroutine.create` was not.

Rows: `hook_*`.

## 3. Allocation past the per-extension cap

`set_memory_limit(8 MiB)`, a loop that appends 1 KiB strings.

- Plain, inside `pcall`, inside a coroutine, inside the instruction hook, and
  from a Rust callback (`create_string` of 64 MiB, `create_table` in a loop):
  a Lua `memory error: not enough memory` every time. No abort, exit status 0.
  The VM stays usable and holds 34 KiB after a collection, and it hit the cap
  again on three further rounds.
- `pcall` catches the memory error, so a script can keep running after it. It
  is an ordinary error to the script.
- `string.rep("x", 1 << 40)` fails earlier with `resulting string too large`, a
  runtime error with no allocation attempted.

Rows: `oom_*`.

## 4. A Lua error while Rust holds a lock

- A host function holds a `Mutex` guard and calls a Lua function that calls
  `error()`, under `pcall`, nested through three frames: the error comes back
  as an `Err`, the guard drops, the mutex is not poisoned and `try_lock`
  succeeds.
- The instruction hook holds the guard while calling a Lua function that
  errors: same result.
- The deadline hook fires while a host function holds the guard around a Lua
  loop: the call fails with `deadline exceeded`, the lock is free.
- The same holds under abort and unwind builds. A panic is the only case that
  poisons (case 1), and under abort there is no unwind to poison.

Rows: `lock_*`.

## Malformed replies

None. No model was called.

## For the owner

- `docs/extensions.md` is corrected: the hook is per coroutine ("It loops or
  hangs"). Every callback runs as a coroutine (#39), so this is the case that
  matters, not an edge.
- `docs/code-quality.md`, "Panics", says that under unwind a panic in a
  function that a Lua extension calls "reaches the extension as an ordinary Lua
  error that its `pcall` can catch". With mlua 0.12 it does not: it unwinds
  past `pcall` to the Rust caller (case 1). The abort decision is unaffected
  (abort still ends the process). That sentence is left unchanged because it is
  outside this ticket.
- The cap, the error boundary and the lock claims hold as written.
- Not measured: `coroutine.create`, Lua's `xpcall` handlers, and RSS after the
  cap (pass1 has that).
