# 6. The extension runtime is embedded Lua 5.4

Date: 2026-09-21

## Status

Accepted. Settled by
[Extension runtime: Lua or something else?](https://github.com/aakshintala/fiber/issues/11)
and, for what a package holds and how it is shared,
[Extension distribution](https://github.com/aakshintala/fiber/issues/45),
and, for what runs in the terminal,
[TUI extension seams](https://github.com/aakshintala/fiber/issues/163),
against the runtime comparison in
[#3](https://github.com/aakshintala/fiber/issues/3). The contract is
`docs/extensions.md`; the trust model is `docs/permissions.md`; the seams are
`docs/architecture.md`. Measurements: `research/extension-runtime/` (pass1
disqualification probes, pass2 RSS sweep, pass3 authoring probe, vm-isolation).

## Context

Map premise 8: v0.0.1 ships an extension system where an extension registers
through the same three seams a built-in does — tool, provider and hook — and can
replace a built-in by name. The runtime has to serve an ecosystem where people freely
build and share extensions, as pi's does, with no sandbox.

Four constraints were fixed going in: a hook must answer synchronously inside a
turn under an enforced timeout (`docs/architecture.md`); an extension runs with
the account's full rights and the runtime is not a security boundary
(`docs/permissions.md`); no async runtime, no tokio, zero idle CPU
([ADR 0004](0004-blocking-threads-no-async-runtime.md)); and runtime memory, not
binary size, is the metric that matters.

Candidates measured: Lua 5.4 and Luau (both via `mlua`), JavaScript via
`rquickjs` (QuickJS), and — considered and rejected without a full sweep —
Starlark, WASM (`wasmtime`/`wasmi`), and full TypeScript on a V8-class runtime.

## Decision

**Embed Lua 5.4 through `mlua` (vendored). One Lua VM per extension, created
lazily on first use. No sandbox.** How providers use it is
[ADR 0007](0007-protocols-are-native-providers-are-extensions.md).

The evidence, in the order it decided things:

- **Memory.** Lua 5.4 is the leanest candidate at every concurrency level and on
  every platform (macOS arm64, Linux x86_64, Linux arm64). At 16 concurrent
  interpreters it peaks around 5 MiB against QuickJS's ~11 and Luau's ~14, and it
  reclaims memory to the OS on Linux where QuickJS never does. Per-instance
  marginal cost is ~150 KiB against ~500 for both others
  (`research/extension-runtime/pass2`). Memory is the axis that matters most.

- **Authoring by a model is a wash.** Since Fiber is a coding agent, extensions
  will be model-written. Five models across tiers wrote the streaming provider in
  Lua 5.4, Luau and QuickJS-JS; 14 of 15 ran correctly first try, and no output
  in any language reached for an API the embedding lacks
  (`research/extension-runtime/pass3`). JavaScript's training-data advantage did
  not convert into an authoring edge, and it did not disadvantage Lua. Authoring
  does not break the tie; memory does.

- **Interruptibility.** All three can stop a wedged script under a deadline. Lua
  5.4's mechanism is a debug hook raising an ordinary Lua error, which a script
  can swallow with `pcall`; a two-stage hook (escalate to every-instruction once
  the deadline passes) closes that, verified against an adversarial retry loop
  (`research/extension-runtime/pass1`). QuickJS and Luau get an uncatchable
  deadline from one call; the Lua fix is a known, measured cost, not a defect.

- **Runs source, one binary.** Lua, Luau and QuickJS all run source with no build
  step and link into one static binary. WASM does not: no interpreter runs wasm
  *source*, so a model-authored, git-distributed extension would need a
  compiler in the loop or pre-built bytecode — disqualifying when extensions
  are shared as source, independent of wasm's otherwise good RSS.

One VM per extension costs ~120 KiB per extension and gives per-extension memory
caps, separate GC and crash containment; a shared VM with per-`_ENV` isolation is
leaner at large counts and is the documented fallback
(`research/extension-runtime/vm-isolation`). Lazy instantiation means a session
with no extension in use creates no VM and pays no idle cost.

## Consequences

- **The build gains a C toolchain dependency.** `mlua` vendored compiles Lua's C
  via `cc` on every target. Proven building clean on all three release targets in
  CI. Not unique to Lua — QuickJS compiles C too — but it is now on the release
  path; [#16](https://github.com/aakshintala/fiber/issues/16) owns keeping `cc`
  there.

- **A panic in a host callback ends the session.** Fiber builds with
  `panic = "abort"` and holds all its code, host callbacks included, to a
  no-panic bar that its lints enforce (`docs/code-quality.md`). Under unwind,
  with mlua 0.12, a host callback's panic unwinds through Lua's frames, past
  the extension's `pcall`, to the Rust caller, and leaves any mutex the
  callback held poisoned. Abort ends the process at the panic instead
  ([research/extension-runtime/linux-containment](../../research/extension-runtime/linux-containment/README.md)).
  Extension-level Lua errors are safe either way.

- **JSON is a permanent host-owned API.** Lua has no built-in JSON, so
  `json.decode`/`encode` are host functions Fiber maintains. QuickJS would have
  provided them natively. Small, and already implemented via serde.

- **Lua 5.4 specifically, not 5.5.** 5.5's incremental-GC and array-allocation
  improvements show as 0–3% RSS on both the isolation and streaming workloads —
  its real win is GC pause latency, irrelevant on an I/O-bound provider hot path
  — while 5.4 has far more model training mass and more maturity, on a
  hard-to-reverse choice (`research/extension-runtime/vm-isolation`).

- **Lua code is shared through Fiber's own packages, not npm.** An extension
  is a directory of Lua files, loaded with a `require` limited to that
  directory. Shared code is either vendored into the extension or taken from
  another extension it depends on by name and version. Fiber fetches and
  resolves those dependencies itself (`docs/extensions.md`). npm's libraries
  are reached another way: an extension can be a separate program in any
  language, a process extension, which pays for its own runtime only when a
  person chooses it.

- **Lua is the in-process way to run an extension, not the only way.** A
  process extension registers the same things and has the same host calls
  over a pipe (`docs/extensions.md`). Lua stays for everything that should
  cost about 150 KiB rather than a process: a Node process measured 40 MiB
  idle, Bun 20 MiB and Python 10 MiB (macOS arm64,
  `research/extension-process/`).

- **TUI extensions are Lua too.** The part of an extension that draws in the
  terminal runs the same embedding in the terminal's process
  (`docs/tui.md`, "Extension seams"). One language covers both halves of a
  package, and a TUI extension costs about 30 to 35 KiB of Lua state, where a
  Bun process would cost about 20 MiB against the terminal's 8 MiB idle
  budget.

- **A Lua extension in a session has its own thread and inbox.** Hooks, watcher events,
  timers and replies to host calls arrive there, and a host call suspends
  the calling code as a coroutine, so an extension can poll and wait without
  an async runtime ([ADR 0004](0004-blocking-threads-no-async-runtime.md)
  binds Fiber's Rust code, not an extension's loop). A TUI extension runs on
  the terminal's thread instead, because a renderer must answer inside the
  frame that needs it.

## Rejected

**QuickJS (JavaScript via `rquickjs`).** The strongest alternative: uncatchable
deadline from one call, `JSON.parse` native, best training-data familiarity, and
prebuilt bindings for all three targets. Rejected on memory — ~2× Lua per
instance and it never returns memory to the OS — after pass3 showed its
familiarity advantage did not convert into an authoring edge. It was the "JS
syntax without the ecosystem" middle option, and captured neither the low-RSS
prize nor the npm prize.

**Luau.** Uncatchable deadline with no staging, a first-class `sandbox()`, native
gradual types, and a working `require`. Rejected on memory: heaviest of the
candidates on both a fixed per-process baseline (4–6 MiB before any work) and
per-instance cost, and the penalty is the VM itself — measured identical with the
sandbox on and off — so dropping the sandbox we do not want buys nothing.

**Starlark.** Ruled out in [#3](https://github.com/aakshintala/fiber/issues/3):
idle RSS in the 6.5 MiB band and peak RSS that scaled to 45 MiB on a loop-heavy
script.

**WASM (`wasmtime`/`wasmi`).** Its one real edge is a capability sandbox the
owner explicitly does not want. `wasmtime` is the heaviest runtime and needs
tokio for any real I/O (via `wasmtime-wasi`), violating ADR 0004; `wasmi` is lean
but shares the disqualifier: wasm is bytecode, so a model-authored,
git-distributed extension cannot run as source without a compile step.

**Full TypeScript on a V8-class runtime (the pi model).** Buys npm, real types,
and near-drop-in portability of pi's extension ecosystem. Rejected because it is
a different architecture, not a runtime knob: embedding V8 costs tens of MiB of
RSS and a JIT, breaking the low-RSS premise; shelling out to system Node/Bun
breaks single-binary distribution. npm's libraries stay reachable through a
process extension, which runs Node or Bun as a separate program only when a
person installs one.

**TypeScript for the terminal's half.** UI is where TypeScript's reach is
strongest, and pi's UI extensions are TypeScript. Two forms were weighed for
[#163](https://github.com/aakshintala/fiber/issues/163). TUI extensions on Bun
or Node would be a process beside the terminal, about 20 MiB for Bun idle, and
each render would cross a pipe at 56 to 79 µs against 8 µs into Lua. npm's
terminal libraries, such as Ink and OpenTUI, each own the whole screen, so
none can draw into a slot. Rewriting the terminal itself in TypeScript would
let extensions use its own components, as pi's do, but reopens
[#147](https://github.com/aakshintala/fiber/issues/147): OpenTUI on Bun
measured 88 MiB idle with idle wakeups, against ratatui's 1.9 MiB and none
(`research/tui-surface/`). The terminal is a client, so it can be rewritten
later without touching a session.
