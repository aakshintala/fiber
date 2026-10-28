# Evidence: an opt-in OS sandbox for Fiber

Measured October 21 to 22, 2026, against fiber `main` at `2a3c542`. No
model was called: srt and `codex sandbox` only wrap a shell command, and
codex ran with an empty `CODEX_HOME` in the scratch directory, so the
user's `~/.codex` was neither read nor written.

Source: the first comment on
[#3027](https://github.com/aakshintala/fiber/issues/3027), reproduced here
as the record. The raw TSVs and scripts were not kept. What follows states
the method in words and the matrices as measured; nothing below invents
them.

## Environment

| | |
|---|---|
| Machine | Apple silicon, 15 cores, macOS 27.0 (build 26A5378n) |
| macOS sandbox | `/usr/bin/sandbox-exec`, marked deprecated in its man page, present and enforcing |
| Claude Code | 2.1.304 (binary strings only) |
| sandbox-runtime (srt) | 0.0.81, installed with `npm i --prefix` into the scratch directory; Node 26.4.0 |
| Codex CLI | 0.163.0 (`/opt/homebrew/bin/codex`, a Node launcher around the Rust binary) |
| Linux | Colima VM, Ubuntu 24.04.4, kernel 6.8.0-117-generic, arm64; Docker 29.5.2 |
| Linux container | `node:22-bookworm-slim` plus bubblewrap 0.8.0, socat, ripgrep, srt 0.0.81, codex 0.163.0 and `ll-exec` (below); runs as uid 1000; nothing from the host mounted; image (355 MB) deleted after each run |
| Landlock in the container | ABI 4. Docker's default seccomp profile allows the Landlock syscalls |
| User namespaces | `kernel.apparmor_restrict_unprivileged_userns=1`; `unshare -Ur` fails |

**Load.** Other agent sessions were building on this Mac during the runs:
the load average was 11 to 13 on 15 cores for the later runs. The probe
outcomes do not depend on load. The macOS timings do, so both runs are
reported.

## Fixture and safety

`scripts/fixture.sh up` builds `~/.cache/fiber-sandbox-probe/`. It sits
outside `/tmp` and `$TMPDIR` on purpose: every sandbox measured allows
writes there, so a fixture in the scratch directory under `/private/tmp`
could not tell the workspace, temp and outside apart. `fixture.sh down`
deletes it.

- `repo/` is a git repository in a normal checkout: workspace "repo", every probe.
- `wt/` is a `git worktree` of `repo`; its `.git` is a file pointing at
  `repo/.git/worktrees/wt`. It is workspace "wt", git probes only, and it
  is shaped like a Fiber delegate's worktree.
- `outside/`, `secret/canary`, `fiber-home/credentials/token` and
  `run/hub.sock` stand in for a sibling directory, a credential, Fiber
  home's `credentials/` and `~/.fiber/run/hub`. The runner binds the Unix
  socket and a localhost TCP port outside every sandbox.

Real `~/.ssh`, `~/.aws`, `~/.config/gcloud` and `~/.config/gh` were tested
only with `ls <dir> >/dev/null 2>&1`: names only, discarded, exit code
recorded. No real credential was read, printed or copied. The launchd
escape probe submitted a job only from inside a sandbox; the runner removes
any job with the probe's label and its file after each sandbox. No GitHub
write was made.

## Names used in the raw files

| Raw name | What it is | In the issue |
|---|---|---|
| `bare` | no sandbox | baseline |
| `fiber-a` | `sandbox-exec -f configs/fiber-a.sb`, Fiber's own profile | Option A, macOS |
| `option-c` (Linux raw files) | `ll-exec` with every workspace grant, `--deny-read` for both canaries and `--seccomp` | **Option A, Linux** (renamed after the first runs; the raw directory keeps the old name) |
| `ll-alone` | `ll-exec` with Landlock grants only: no seccomp, no read denies | Landlock without the seccomp half |
| `bwrap-direct` | `bwrap` with ro-bind `/`, the workspace bound read-write, tmpfs over the canaries and `run/`, ro-bind over hooks and config, `--unshare-user/-pid/-net` | Option C |
| `bwrap-no-run-hide` | the same without the tmpfs over `run/` | Option C, socket check |
| `srt-nonet`, `srt-allowlist` | `srt -s configs/srt-*.json`: `allowWrite ["."]`, `denyRead` the two canaries, `allowedDomains` empty or example.com, github.com and the package registries | Option B |
| `codex-ro`, `codex-ws`, `codex-ws-net` | `codex sandbox -c sandbox_mode=...`, with `sandbox_workspace_write.network_access=true` for `-net` | reference |

`linux/ll-exec.c` makes the syscalls Fiber would make from the shell's
`pre_exec` through the `landlock` and `seccompiler` crates.

- **Landlock:** read and execute on `/`, full access beneath each `-w`,
  read and write on each `-f`, and TCP bind and connect handled when ABI is
  4 or more. Landlock can only grant, so `--deny-read` grants read on every
  entry along the path from `/` except the denied path.
- **seccomp:** `socket()` with `AF_UNIX`, `AF_INET` or `AF_INET6`, and
  `io_uring_setup`, fail with `EPERM`. This is arm64 only here; x86_64
  needs its own syscall numbers.

## Probe matrix, macOS (`raw/macos/matrix.*.md`)

`ok`: the effect happened, checked rather than taken from the exit code.
`--`: refused, with the exit code. `n/a`: not applicable. Blank: not run.

Workspace "repo" (a normal checkout):

| probe | bare | A (fiber-a) | srt, no network | srt, allowlist | codex read-only | codex workspace-write | codex workspace-write + network |
|---|---|---|---|---|---|---|---|
| write in workspace | ok | ok | ok | ok | -- (1) | ok | ok |
| write sibling outside workspace | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| write host `$TMPDIR` | ok | ok | -- (1) | -- (1) | -- (1) | ok | ok |
| write the sandbox's own `$TMPDIR` | ok | ok | ok¹ | ok | -- (1) | ok | ok |
| write `/tmp` | ok | ok | -- (1) | -- (1) | -- (1) | ok | ok |
| write `.fiber/` | ok | -- (1) | ok | ok | -- (1) | ok | ok |
| read in workspace | ok | ok | ok | ok | ok | ok | ok |
| read canary outside workspace | ok | ok | -- (1) | -- (1) | ok | ok | ok |
| read Fiber-home `credentials/` canary | ok | -- (1) | -- (1) | -- (1) | ok | ok | ok |
| list `~/.ssh`, `~/.aws`, `~/.config/gcloud`, `~/.config/gh` | ok | ok | ok | ok | ok | ok | ok |
| write outside by a grandchild | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| write outside by `python3` | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| write through a workspace symlink leading outside | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| `ps` (setuid) | ok | -- (126) | -- (126) | -- (126) | -- (126) | -- (126) | -- (126) |
| `pgrep` | ok | -- (3) | -- (3) | -- (3) | -- (3) | -- (3) | -- (3) |
| `kill -0` a process outside | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| connect to a Unix socket outside (the stand-in hub) | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | **ok** |
| bind a Unix socket in workspace | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | ok |
| localhost TCP | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | ok |
| DNS lookup | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | ok |
| HTTPS to example.com | ok | -- (6) | -- (56)² | ok | -- (6) | -- (6) | ok |
| `git ls-remote` a public GitHub repo | ok | -- (128) | -- (128) | ok | -- (128) | -- (128) | ok |
| `cargo build`, no dependencies | ok | ok | ok | ok | -- (101) | ok | ok |
| `cargo build`, one dependency, warm cache | ok | ok | ok | ok | -- (101) | ok | ok |
| `npm install` one package | ok | -- (1) | -- (1) | -- (1)³ | -- (1) | -- (1) | -- (1)³ |
| write `.git/hooks/` | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| `git config` (writes `.git/config`) | ok | -- (255) | -- (4) | -- (4) | -- (255) | -- (255) | -- (255) |
| `git commit` | ok | ok | ok | ok | -- (128)⁴ | -- (128)⁴ | -- (128)⁴ |
| `git checkout -b` | ok | ok | ok | ok | -- (128) | -- (128) | -- (128) |
| `launchctl submit` (launchd runs a program outside) | not run | -- (1) | -- (1) | not run | -- (1) | -- (1) | not run |

Workspace "wt" (a git worktree whose git directory is outside the
workspace, like a delegate's):

| probe | bare | A | srt, no network | srt, allowlist | codex read-only | codex workspace-write | codex workspace-write + network |
|---|---|---|---|---|---|---|---|
| write `hooks/` (in the common dir) | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| `git config` | ok | -- (255) | -- (255) | -- (255) | -- (255) | -- (255) | -- (255) |
| `git commit` | ok | **ok** | -- (128) | -- (128) | -- (128) | -- (128) | -- (128) |
| `git checkout -b` | ok | **ok** | -- (128) | -- (128) | -- (128) | -- (128) | -- (128) |
| rewrite the worktree's `.git` file (its gitdir pointer) | ok | -- (1) | **ok** | **ok** | -- (1) | -- (1) | -- (1) |

1. srt sets `TMPDIR=/tmp/claude`. On the very first srt run that directory
   did not exist yet, and the write failed. It existed for every later run.
2. srt's proxy refuses the CONNECT.
3. Network was allowed, but npm's cache in `~/.npm` is outside the workspace.
4. codex keeps `.git` read-only inside writable roots, so a commit needs an escalation.

## Probe matrix, Linux (`raw/linux/<mode>/matrix.*.md`)

**Which sandboxes start.** Three Docker modes were tried:

| mode | Landlock alone, Option A | Option C (bwrap) | srt | codex |
|---|---|---|---|---|
| default (Docker's seccomp and AppArmor profiles) | runs | "No permissions to create new namespace" | same, fails closed | same, fails closed |
| `seccomp=unconfined`, `apparmor=unconfined` | runs | "loopback: Failed RTM_NEWADDR: Operation not permitted": the user namespace is created without capabilities (Ubuntu 24.04's AppArmor rule) | same | same |
| root with `CAP_SYS_ADMIN` and `systempaths=unconfined` (not what a person gets) | runs | runs | "apply-seccomp: write /proc/self/setgroups (nested userns is capability-restricted...)": fails closed | runs |

srt's Linux path never started here. Making it start needs
`apparmor_restrict_unprivileged_userns=0` on the VM, a system setting this
task does not change. #30's probe found the same on GitHub's runners:
`unshare -Urn` fails and `bwrap` is not installed.

Workspace "repo", default mode. The Landlock and Option A columns are
identical in all three modes:

| probe | bare | Landlock alone | **Option A** (Landlock + read-deny walk + seccomp) | Option C (bwrap)⁵ | bwrap without hiding `run/`⁵ | codex workspace-write⁵ | codex + network⁵ |
|---|---|---|---|---|---|---|---|
| write in workspace | ok | ok | ok | ok | ok | ok | ok |
| write sibling outside | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| write `/tmp` | ok | ok | ok | ok | ok | ok | ok |
| write `.fiber/` | ok | ok | **ok** | ok | ok | ok | ok |
| read canary outside | ok | ok | -- (1) | -- (1) | -- (1) | ok | ok |
| read Fiber-home `credentials/` canary | ok | ok | -- (1) | -- (1) | -- (1) | ok | ok |
| write outside by grandchild, `python3`, symlink | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| `ps`, `pgrep` | ok | ok | ok | ok | ok | ok (pgrep) / -- (ps) | same |
| `kill -0` a process outside | ok | **ok** | **ok** | -- (1) | -- (1) | -- (1) | -- (1) |
| connect to the Unix socket outside | ok | **ok** | -- (1) | -- (1) | **ok** | -- (1) | ok |
| bind a Unix socket in workspace | ok | ok | -- (1) | ok | ok | -- (1) | ok |
| localhost TCP | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | ok |
| DNS lookup (UDP) | ok | **ok** | -- (1) | -- (1) | -- (1) | -- (1) | ok |
| HTTPS, `git ls-remote` | ok | -- | -- | -- | -- | -- | ok |
| `npm install` one package | ok | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) | -- (1) |
| write `.git/hooks/` | ok | ok | **ok** | -- (1) | -- (1) | -- (1) | -- (1) |
| `git config` | ok | ok | **ok** | -- (4) | -- (4) | -- (255) | -- (255) |
| `git commit` | ok | ok | ok | ok | ok | -- (128) | -- (128) |

5. These columns are from the root-with-`CAP_SYS_ADMIN` mode, the only one
   in which they start.

Workspace "wt", default mode:

| probe | bare | Landlock alone | **Option A** |
|---|---|---|---|
| write `hooks/` | ok | -- (1) | -- (1) |
| `git config` | ok | -- (255) | -- (255) |
| `git commit`, `git checkout -b` | ok | -- (128) | **ok** |
| rewrite the worktree's `.git` file | ok | ok | **ok** |

`cargo` rows are `n/a` on Linux: the image has no Rust toolchain.
Installing one means downloads from static.rust-lang.org, which is outside
this task's allowed hosts. `$TMPDIR` is unset in the container, so the
"sandbox's own `$TMPDIR`" row is meaningless there.

## Cost per call

Each wrapper runs `/bin/bash -c true`, the shape Fiber's shell spawns.
"Extra resident" is the wrapper's process tree while it runs
`/bin/sleep 3`, minus `sleep` itself.

macOS (`scripts/timing.py`):

| wrapper | run 1: median added (p95 total) | run 2, interleaved, load ~ 13: median added (p95 total) | extra resident while running |
|---|---|---:|---:|
| `bash -c`, no sandbox | 2.4 ms total (2.7) | 4.4 ms total (5.0) | 0 |
| **Option A**: `sandbox-exec -f fiber-a.sb` | **+6.2 ms** (9.1) | **+12.3 ms** (18.4) | **0**, sandbox-exec execs |
| srt's own profile through `sandbox-exec -p` (srt's per-call cost when embedded) | +7.5 ms (10.4) | +14.8 ms (20.8) | 0 |
| srt CLI per call (Node, proxy start, profile, sandbox-exec) | +100.3 ms (108.5) | +183.7 ms (207.1) | 76 to 77 MiB (`node`) |
| `codex sandbox` CLI per call | +55.5 ms (65.0) | +88.9 ms (100.8) | 75 MiB (Node launcher plus `codex`) |

Run 1 ran sequentially on a quieter machine; its JSON was overwritten by a
later run, so its figures are taken from the session transcript. Run 2 is
`raw/timing-macos.json`: 40 runs, interleaved, under load. A sequential run
under the same load (`raw/timing-macos.sequential-loaded.*`) gave +13.6 ms
for Option A and +187.9 ms for the srt CLI. #30's earlier probe measured
"about 6 ms added per shell call" (`research/sandbox/macos-probe.md`),
which agrees with run 1. In its own agent, codex spawns `sandbox-exec`
directly, so its in-agent cost is close to the `sandbox-exec -p` row, not
the CLI row.

srt as a library, the way Claude Code embeds it
(`scripts/srt-lib-timing.mjs`, `raw/srt-lib-timing-macos.json`):

- Import takes 61 ms and `initialize()` (proxy start) 3.4 ms, once per session.
- Per call: building the profile takes 3.0 ms, and building plus running
  takes 16.3 ms median (18.3 p95), +13.9 ms over bare `bash -c`.
- The Node process holding the runtime and proxy is 85 MiB resident,
  against 41.5 to 46 MiB for bare Node. Fiber, being Rust, would have to
  run this as a sidecar.

Linux (inside the container, `raw/linux/<mode>/timing.json`, 30 runs):

| wrapper | default mode | root-with-`CAP_SYS_ADMIN` mode | extra resident |
|---|---|---:|---:|
| `bash -c`, no sandbox | 0.3 to 0.7 ms total | 0.3 ms total | 0 |
| Landlock alone | +0.3 ms | +0.1 ms | 0 |
| **Option A**: Landlock, read-deny walk and seccomp | **+0.6 to +0.8 ms** | +0.3 ms | **0** |
| Option C: bwrap | does not start | +1.9 ms | 3.2 MiB (two `bwrap` processes per running command) |
| srt CLI | does not start | does not start | n/a |
| `codex sandbox` CLI | does not start | +28.4 ms | 96 MiB (Node launcher, `codex`, two `codex-linux-sandbox`, `bwrap`) |

For comparison, one stage-one review costs 2.0 to 2.4 s median (4.9 to 6.5
s p95) and about 10 to 12k input tokens plus 118 to 402 output tokens
(`research/decision-models/README.md`, "Fiber's reviewer on imported
sessions": 175 calls, Luna and Muse). On Muse before #1471 a review cost
1,000 to 1,300 output tokens (#1471).

## Share of reviewed calls a sandbox would contain (`scripts/estimate.py`, `raw/estimate.md`)

| source | P: runs unharmed (usage.md) | R: read-only today (#1497) | contained share of reviewed calls | if `cd X &&` counts as read-only |
|---|---:|---:|---:|---:|
| pi | 81.4% | 22.5% | 76% | 68% |
| Claude Code main | 72.5% | 12.6% | 68% | 65% |
| Claude Code subagents | 83.7% | 6.5% | 83% | 72% |
| switchyard#204 canary, **measured** (#1587, 2026-10-11) | | | **104 / 114 = 91%** | |

The formula is (P − R) / (1 − R). It assumes every read-only call runs
unharmed and every call that breaks is a reviewed one. **This is an
estimate:** the two sweeps ran on different dates over different corpus
sizes (27,818 against 67,226 pi calls).

- usage.md over-counts breaks. Any `npm` or `bun` invocation counts as
  writing outside the workspace, and worktree commits count as breaks,
  though Option A allows them.
- usage.md also misses breaks it cannot see: `ps` and `pgrep` on macOS,
  and writes made inside an inline script.

What leaves the sandbox is mostly `gh` (issue, pr, run, api) and `git
push`/`fetch`: 12 to 24% of all shell calls in usage.md.

## Escape hatches

| | how a command leaves the sandbox | is that judged? | source |
|---|---|---|---|
| Claude Code 2.1.304 | per call, `dangerouslyDisableSandbox: true`; `excludedCommands` patterns; `allowUnsandboxedCommands: false` turns the parameter off | yes: "This goes through the permission gate (a user prompt, or the auto-mode classifier when auto mode is active)". Sandboxed calls are auto-allowed: "`autoAllowBashIfSandboxed` is independent and still defaults to true" | binary strings |
| Claude Code's instruction to the model | "Immediately retry with `dangerouslyDisableSandbox: true` (don't ask, just do it)", but "A sandbox denial on a credential, a file or a host that the task does not involve is the boundary above at work: tell the user rather than retrying" | | binary strings |
| codex | per call, `sandbox_permissions: "require_escalated"` with a `justification`; on a sandbox denial the orchestrator asks for approval, then retries escalated | yes, by the person or by guardian in auto-review. Guardian reviews only what the sandbox cannot contain | `research/sandbox/reference-agents.md` §5; #477 comment |
| srt | none of its own: it is a CLI and a library, and the embedder decides | n/a | README |
| Fiber, proposed | per call, `unconfined: true` on the shell call | yes, as every call is today | design issue Q7 |

## Findings the issue relies on

1. **A shell call can drive Fiber unless Unix sockets are fenced.**
   `docs/invocation.md` ("Remote clients"): "a session's shell reaches the
   local socket". A driver's reply answers a permission request
   (`docs/permissions.md`, "Headless"). Seatbelt deny-default, srt and
   Option A on Linux block it.
   - codex with `network_access=true` does not: the stand-in hub answered.
   - bubblewrap's `--unshare-net` does not on its own: a pathname socket
     ignores network namespaces, so `run/` must also be hidden.
2. **Landlock alone leaves three gaps that seccomp or a PID namespace
   close:** UDP and DNS (Landlock ABI 4 handles TCP only), pathname Unix
   sockets, and signals to same-user processes outside (ABI < 6).
   - Option A's seccomp filter closes the first two.
   - The third stays open on kernels before ABI 6, Linux 6.12: a confined
     call can `kill` the session or the hub, though it cannot drive them.
3. **Landlock cannot protect `.git/config`, hooks, a worktree's `.git`
   file or `.fiber/` inside a writable workspace.** It only grants, and a
   grant on a directory reaches everything beneath it. Git writes `index`
   and `config` through lock-and-rename in the same directory, so allowing
   commits also allows replacing `config`. Hence Q5.
4. **The worktree `.git` file is an escape route.** Repointing it makes the
   next unconfined `git` read a config the call wrote.
   - srt guards it on Linux only, as its README states, and on macOS left
     it writable (measured).
   - codex and Option A on macOS refuse it.
5. **Worktree commits.** srt and codex both fail `git commit` in a
   worktree, because the git directory is outside the writable root. Option
   A grants that directory plus the common `objects/`, `refs/` and `logs/`,
   and commits work.
6. **macOS limits of every Seatbelt sandbox measured:** `ps` (setuid) fails
   with 126 and `pgrep` with 3 ("sysmond service not found").
   - A deny-default profile also refuses ioctls on an inherited pty: `stty`
     failed under Option A and srt, and passed under codex. One rule fixes
     it (`configs/fiber-a.sb`, re-measured with `scripts/pty-probe.py`).
7. **`launchctl submit` was refused** under every deny-default profile
   tried. With `(allow default)`, the profile style of #30's first probe and
   of switchyard's seal, launchd would be reachable. That was not measured
   here.
8. **srt's temp policy** allows `/tmp/claude` only. Fiber's built-in `grep`
   side file lives in `std::env::temp_dir()`
   (`crates/tools/src/shell/matches.rs:16`).
9. **Reads stay wide open by default in every tool.** srt, codex and Option
   A all let a confined call list `~/.ssh` and `~/.aws`. Reads matter only
   through exfiltration, which needs a network, a later reviewed push, or
   the model provider. The last two are already open today through the read
   and edit fast paths.

## What could not be measured, and why

- **srt on Linux.** It never started here: it needs capability-bearing user
  namespaces, and enabling those on the VM is a system setting this task
  does not change. Its Linux policy is described from its README and source
  only.
- **`cargo` on Linux.** The container has no Rust toolchain, and getting
  one means downloading from hosts outside this task's allowed list.
- **The escape hatches end to end.** Watching `dangerouslyDisableSandbox`
  or `require_escalated` go through a review needs a model call, which this
  task does not make. Their behaviour is cited from binary strings and
  source.
- **The real drop in reviewer spend.** That needs a Fiber build with the
  sandbox, which does not exist. The 91% comes from switchyard#204's
  classification. switchyard#204's per-call command lists stay on the
  owner's machine, so the 6 calls it left unclassified could not be
  checked.
- **Landlock ABI 6 or later** (signal and abstract-socket scoping). The
  kernel here is 6.8, ABI 4. #30 found ABI 7 on GitHub's runners.
- **Configured Unix-socket path grants.** The matrix tested the empty-default
  policy, which denied Unix sockets; it did not test any person-configured
  path grants.
- **Windows.** Not a Fiber target (`docs/releasing.md`).
- **macOS timings on a quiet machine.** Other sessions were building, so
  two runs are reported.
- **The launchd probe with no sandbox.** Not run on purpose: it would have
  scheduled a real job.

## Files

Each path is relative to the scratch `sandbox-design/` directory.

- `scripts/fixture.sh`: builds and removes the fixture.
- `scripts/probe.sh`: the probe run inside each sandbox.
- `scripts/run-macos.sh`: the macOS matrix. `ONLY_WS` limits it to one workspace.
- `scripts/run-linux.sh`: builds and deletes the image and runs the Docker
  modes, with `linux/run-inside.sh` running in the container.
- `scripts/matrix.py`: folds the TSVs into tables.
- `scripts/timing.py`, `scripts/srt-lib-timing.mjs`: cost per call.
- `scripts/pty-probe.py`: the pty check.
- `scripts/srt-dump-profile.mjs`: prints srt's generated wrapper and profile.
- `scripts/estimate.py`: the skip-share estimate.
- `configs/fiber-a.sb`: Option A's Seatbelt profile. Its system section is
  copied from srt 0.0.81's generated profile; its file, socket and git rules
  are Fiber's.
- `configs/srt-*.json`: srt settings.
- `linux/ll-exec.c`: Option A's Linux mechanism. `linux/Dockerfile` builds
  the image.
- `raw/macos/`, `raw/linux/<mode>/`: TSV, stderr and matrices for each
  sandbox and workspace.
- `raw/srt-profile-macos.sb`, `raw/srt-wrapped-macos.txt`: srt's generated
  macOS profile and wrapper.
- `raw/timing-macos*.{md,json}`, `raw/srt-lib-timing-macos.json`,
  `raw/linux/*/timing.json`: timings.
- Before the raw files are committed, the home directory in them was
  replaced with `$HOME`, and the localhost-proxy credential in
  `raw/srt-wrapped-macos.txt`, which belonged to a dead process, was
  redacted.
- `raw/issues/`: the read-only issue threads used (fiber #30, #477, #1471,
  #1497, #1587, #2419, #2836, #2847 and switchyard#204).

To rerun: `scripts/fixture.sh up`, then `scripts/run-macos.sh`,
`scripts/timing.py <root> ~/.cache/fiber-sandbox-probe 40`,
`scripts/run-linux.sh`, and finally `scripts/fixture.sh down`.
