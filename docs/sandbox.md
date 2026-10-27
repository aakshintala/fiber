# The OS sandbox

This page defines the policy for #3027's opt-in operating-system sandbox,
on each platform, and what it costs a session. The issue delivers the policy
in implementation parts; this page records it ahead of that work. The policy
is settled by [permissions: an opt-in OS sandbox, and whether its commands
skip the reviewer](https://github.com/aakshintala/fiber/issues/3027); that
ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`: sandbox names the feature, confined names a
call that ran inside it, and nothing else. The rule that judges calls is
`docs/permissions.md`, "Confinement" and "Fast paths"; the keys are
`docs/configuration.md`, "Keys"; the escape hatch is `docs/tools.md`,
"Shell". Part 5 adds the `confined` event marker under the [surveyor
ruling](https://github.com/aakshintala/fiber/issues/3027#issuecomment-6425832076).

## The rule

The sandbox is opt-in and off by default. When it is on, a shell call runs
confined under Fiber's own policy: Seatbelt on macOS, Landlock plus seccomp
on Linux; no srt, no bubblewrap. A shell call that runs confined skips the
reviewer, as a new fast path after the credential deny and the person's
standing deny and ask rules, which still come first and still apply. A
built-in destructive list stays reviewed even when confined —
`rm -r`/`rm -rf`, `git reset --hard`, `git clean`, `git checkout --`,
`git restore` — matching any part of a multi-part command, checked before
the confined fast path, best effort by command text.

With the sandbox on, read-only fast-path calls also run confined, so a
wrong `shell.read_only` entry fails instead of writing. An
`unconfined: true` call leaves the sandbox and is reviewed exactly as with
the sandbox off; Fiber never reruns a failed confined call unconfined.

The sandbox's own read deny follows whatever the credential deny covers:
Fiber home's `credentials/`, `file` sources, and whatever #3025 adds.

## What a confined call can do

- Read any file except Fiber home's `credentials/` and the person's extra
  denies (`sandbox.deny_read`).
- Write the workspace and temp: the workspace root the session launched
  against, the system temp directory, and the extra paths the person grants
  (`sandbox.allow_write`, such as `~/.pi/agent`).
- Commit in a delegate's worktree ("Worktrees" below).

## What it cannot do

- Write outside the workspace and temp: not directly, not through a
  grandchild, not through `python -c`, not through a symlink that leads out.
- Write `.git/hooks/`, `.git/config`, a worktree's `.git` file or `.fiber/`
  on macOS. On Linux these take the after-call check instead ("On Linux"
  below).
- Open any socket: no internet, DNS, localhost TCP or Unix sockets. This
  blocks the hub, session socket, `docker.sock` and ssh-agent. The sandbox
  denies sockets on both platforms. A non-empty `sandbox.allow_unix_sockets`
  list makes the sandbox unavailable, so Fiber reviews the call instead.
- Reach Fiber's `run/` socket through a configured path. Fiber rejects any
  path that resolves into `run/`, including paths with `..` or symlinks.
- Lose socket access through fallback: an `unconfined: true` call or an
  unavailable-sandbox fallback retains today's socket access. Only a call
  that runs confined loses it.
- Signal a process outside it on macOS; on Linux from Landlock ABI 6, below
  which a confined call can still `kill` the session or the hub, though it
  cannot drive them. This is the measured limit of the Linux policy, stated
  here so no stronger protection is promised below ABI 6.
- Ask launchd to run anything.

## Unchanged

MCP and extension tool calls, because the server or extension runs outside
any shell sandbox. The calls that reach them are reviewed as today, and so
are `delegate_*`, web fetches to unknown hosts, and file-tool writes outside
the workspace. Fiber's own child processes are unchanged: `host.exec`,
MCP stdio servers (`crates/mcp/src/server.rs`), process extensions, image
and PDF children, delegates, Fiber's own worktree `git`, and the hub's
session spawns.

## On macOS

The policy is a Seatbelt profile run through `/usr/bin/sandbox-exec`. It
grants the workspace and temp for writing, keeps `$TMPDIR` writable (the
built-in `grep` writes its side file there,
`crates/tools/src/shell/matches.rs`), denies the credential directory and
extra read denies, denies hooks, config, the worktree `.git` file and
`.fiber/`, and denies every socket.

Two calls fail inside every Seatbelt sandbox measured and need the model's
escape: `ps` refuses to exec a setuid binary and `pgrep` cannot reach
`sysmond`. A call that needs them sets `unconfined: true` and is reviewed
as with the sandbox off.

`tty: true` works: the profile allows ioctls on the inherited pty, so `stty`
passes. A deny-default profile refuses them without that rule.

## On Linux

The policy is Landlock plus a seccomp filter, applied in the shell's
`pre_exec`. The floor is Landlock ABI 2 (Linux 5.19); below it the sandbox
reports unavailable and calls are reviewed. Landlock grants read and execute
on `/`, full access beneath each granted write root, and read and write on
granted files; the seccomp filter fails `socket()` with `AF_UNIX`,
`AF_INET` or `AF_INET6`, and `io_uring_setup`, with `EPERM`.

Landlock only grants, so inside a writable workspace it cannot deny
`.git/config`, hooks, a worktree's `.git` file or `.fiber/`: git's
lock-and-rename writes `index` and `config` through the same directory
rights that commits need. Before a confined call Fiber records `.git/config`,
the hooks directory, a worktree's `.git` file and `.fiber/`; after it,
Fiber restores any change and reports it in the call's result. A
restoration failure is reported, never silent. Main checkouts and worktrees
are both confined. The grants in a worktree are "Worktrees" below.

## Worktrees

A delegate's worktree git directory sits outside the workspace, so the
sandbox grants it: the worktree's git directory and the shared `objects/`,
`refs/` and `logs/`, while `hooks/`, `config` and the worktree's `.git`
file stay denied. Repointing the `.git` file would make the next unconfined
`git` read a config the call wrote, which is why it is denied. Granting all
of `refs/` lets a confined delegate move branches outside its own; a local
branch moved by mistake is recovered from the reflog. `git push` stays
reviewed.

## When the sandbox is unavailable

Failure goes toward review. When the sandbox cannot be applied — no
Landlock, an old kernel below ABI 2, a missing `sandbox-exec` — the planned
event omits `confined`, calls are reviewed as today and the session says so
once. A call declared `confined` never runs unconfined: if applying the
sandbox fails at spawn, the call fails. The event wording is the surveyor's
ruling: `confined: the call runs only inside the sandbox; if the sandbox
can't be applied at spawn, the call fails and nothing runs.`

## Toggling it

Part 9 of #3027 implements `/sandbox` as a session-only toggle and defines
its syntax. A delegate inherits its parent's setting. Toggling changes the
shell tool's description, a person-initiated prompt-cache miss, which is
acceptable.

## Configuration

The person-only keys, defaults, and repository-config restrictions are listed
in [`docs/configuration.md`](configuration.md), "Keys".

The evidence is [research/sandbox/](../research/sandbox/), including the
opt-in probe matrices in `research/sandbox/opt-in/`.
