# Reference agents: OS-level tool confinement

Research for ticket #30. Facts only, each with a file path, line number, or
verbatim string. No claim below comes from memory.

Codex source: `https://github.com/openai/codex`, commit `814de47b69dd63a2660fd14f9af66690888d183e`
(sparse checkout of `codex-rs`, cloned 2026-09-26). All Codex file paths below
are relative to `codex-rs/` in that checkout.

## 1. Codex sandbox modes

Defined in `protocol/src/protocol.rs:1072-1120` as `enum SandboxPolicy`:

| Mode | Read | Write | Network |
|---|---|---|---|
| `DangerFullAccess` | everything | everything | full |
| `ReadOnly { network_access }` | everything | nothing | off unless `network_access: true` |
| `WorkspaceWrite { writable_roots, network_access, exclude_tmpdir_env_var, exclude_slash_tmp }` | everything | cwd + extra roots | off unless `network_access: true` |
| `ExternalSandbox { network_access }` | everything | everything (assumes an outer sandbox already confines it) | per `network_access` |

Default writable roots for `WorkspaceWrite` (`protocol/src/protocol.rs:1243-1300`):
cwd, `/tmp` (Unix, unless `exclude_slash_tmp`), and `$TMPDIR` (unless
`exclude_tmpdir_env_var`) — comment: "TMPDIR is per-user, so writes to TMPDIR
should not be readable by other users on the system."

Read-only carve-outs inside writable roots: `.git` and `.codex`
(`protocol/src/permissions.rs:40-42`, applied at line 860-862:
`append_default_read_only_project_root_subpath_if_no_explicit_rule(&mut entries, ".git")`
/ `".codex"`). Comment at `protocol/src/protocol.rs:1122-1126`: this exists "to
ensure that folders containing files that could be modified to escalate the
privileges of the agent (e.g. `.codex`, `.git`, notably `.git/hooks`) ... are
not modified by the agent."

## 2. Codex on macOS: Seatbelt

Invoked via `/usr/bin/sandbox-exec` only — hardcoded, not `$PATH`
(`sandboxing/src/seatbelt.rs:62`, constant `MACOS_PATH_TO_SEATBELT_EXECUTABLE`).
Comment: "only consider `sandbox-exec` in `/usr/bin` to defend against an
attacker trying to inject a malicious version on the PATH."

Policy assembled at runtime in `create_seatbelt_command_args_with_profile`
(`sandboxing/src/seatbelt.rs:882-1099`) by concatenating `.sbpl` fragments and
passing the result via `sandbox-exec -p <policy> -D<key>=<path> ... -- <cmd>`
(args built at lines 1071-1099).

Base policy `sandboxing/src/seatbelt_base_policy.sbpl` (116 lines), explicitly
modelled on Chrome's macOS sandbox policy (line 3-5 comment cites
`chromium/src/sandbox/policy/mac/common.sb`). Key rules, quoted verbatim:

```
(deny default)
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
```

Also allows: `sysctl-read` for a fixed allowlist of hardware/kernel sysctls
(lines 24-77), `iokit-open` for `RootDomainUserClient`, `mach-lookup` for
`com.apple.system.opendirectoryd.libinfo`, POSIX semaphores/shared memory (for
Python multiprocessing / PyTorch OpenMP), and pseudo-tty access (lines
107-116) so interactive shells still see a TTY.

Network policy (`seatbelt_network_policy.sbpl`, added only when network is
enabled) allows a narrow `AF_SYSTEM` socket, DNS/network-config lookups via
`mach-lookup`, and TLS/cert-verification services (`com.apple.trustd.agent`,
`com.apple.SecurityServer`).

Filesystem: when full disk write is not granted, write access is built per
writable root with `.git`/`.codex` (and other protected metadata names)
excluded (`seatbelt.rs:958-978`, `build_seatbelt_access_policy`). When full
disk read/write *is* granted, the policy is simply
`(allow file-write* (regex #"^/"))` / `(allow file-read*)` minus any
explicitly unreadable roots.

Two extra deny rules always present when not full-disk-write:
`(deny mach-lookup (xpc-service-name-prefix ""))` and, for two fcntl commands
that bypass `file-write*`, `(deny system-fcntl (fcntl-command 80 110))`
(`seatbelt.rs:1080-1094`, comment: "F_MAKECOMPRESSED = 80,
F_TRANSFEREXTENTS = 110... Even deny-default needs this explicit deny.").

There is a `seatbelt_daemon.rs` module (`daemon::protection_policy`) that adds
a policy fragment protecting the shared app-server RPC Unix-domain socket
directory from filesystem-restricted commands, so a sandboxed process can't
reach the privileged control channel.

## 3. Codex on Linux: bubblewrap is current; Landlock is legacy/backup

`linux-sandbox/src/landlock.rs:1-4` (doc comment, verbatim):
> "In-process Linux sandbox primitives: `no_new_privs` and seccomp.
> Filesystem restrictions are enforced by bubblewrap in `linux_run_main`.
> Landlock helpers remain available here as legacy/backup utilities."

`linux-sandbox/README.md` (verbatim, selected lines):
- "Bubblewrap is the default filesystem sandbox."
- "Filesystem-restricted execution requires bubblewrap. The legacy Landlock
  option is rejected for these policies because it cannot isolate app-server
  Unix sockets."
- "On Linux, Codex prefers the first `bwrap` found on `PATH` outside the
  current working directory whenever it is available... If `bwrap` is
  missing, the helper falls back to the bundled `codex-resources/bwrap`
  binary shipped with Codex."
- "Codex also surfaces a startup warning when `bwrap` is missing... Codex
  surfaces the same startup warning path when bubblewrap cannot create user
  namespaces."
- "WSL1 is not supported for bubblewrap sandboxing because it cannot create
  the required user namespaces, so Codex rejects sandboxed shell commands
  that would enter the bubblewrap path."
- "the helper explicitly isolates the user namespace via `--unshare-user`. By
  default it also creates a PID namespace via `--unshare-pid`."
- "When bubblewrap is active and network is restricted without proxy
  routing, the helper also isolates the network namespace via
  `--unshare-net`."
- "In managed proxy mode, the helper uses `--unshare-net` plus an internal
  TCP->UDS->TCP routing bridge... after the bridge is live, seccomp blocks
  new AF_UNIX/socketpair creation for the user command."

Fail behaviour when bubblewrap is truly unavailable (verified in code, not
just docs): `linux-sandbox/src/launcher.rs:53-60`, `exec_bwrap` matches on
`BubblewrapLauncher::Unavailable` and calls:
```rust
panic!(
    "bubblewrap is unavailable: no system bwrap was found on PATH and no bundled \
     codex-resources/bwrap binary was found next to the Codex executable"
)
```
This is **fail-closed**: the sandboxed command does not run unsandboxed; the
process aborts. Bundled bwrap is verified against a SHA-256 digest before
exec (`linux-sandbox/src/bundled_bwrap.rs:39-68`, `verify_digest`; mismatch
exits with `BUNDLED_BWRAP_DIGEST_VERIFICATION_FAILURE_EXIT_CODE`). Bubblewrap
itself is vendored in-tree at `vendor/bubblewrap/` and built from source, not
merely downloaded.

Network blocking is seccomp, not (only) a namespace: `linux-sandbox/src/landlock.rs:179-300`,
`install_network_seccomp_filter_on_current_thread`. In `Restricted` mode it
denies `connect`, `accept`, `accept4`, `bind`, `listen`, `getpeername`,
`getsockname`, `shutdown`, `sendto`, `sendmmsg`, `recvmmsg`,
`getsockopt`/`setsockopt`, and restricts `socket`/`socketpair` to
`AF_UNIX` only via a `SeccompCondition` on the domain argument (lines
221-231). `io_uring_setup/enter/register` are denied unconditionally in every
mode "because io_uring can create AF_VSOCK sockets without a socket()
syscall" (lines 195-199). Default seccomp action is `Errno(EPERM)` on match,
`Allow` otherwise (lines 282-293). `PR_SET_NO_NEW_PRIVS` is set first
(`set_no_new_privs`, lines 130-136) — required for seccomp, and the code
explicitly avoids setting it when not needed because "many `bwrap`
deployments rely on setuid" (comment, lines 66-69).

Kernel/ABI floor: the retained (legacy) Landlock path pins
`landlock::ABI::V5` (`linux-sandbox/src/landlock.rs:150`), but this path is
not used for filesystem confinement anymore per the doc comment above.

Crate dependencies (`linux-sandbox/Cargo.toml`, `sandboxing/Cargo.toml`,
versions from `Cargo.lock`):
- `landlock = "0.4.4"` (legacy/backup only)
- `seccompiler = "0.5.0"`
- `rustix = "0.38.44"` (also a second `rustix = "1.1.4"` elsewhere in the
  lockfile — two versions coexist in the dependency graph)
- `globset`, `sha2` (bundled-binary digest verification)

Separate helper binary / arg0 trick: confirmed in `arg0/src/lib.rs:98-101`:
```rust
if exe_name == CODEX_LINUX_SANDBOX_ARG0 {
    // Safety: [`run_main`] never returns.
    codex_linux_sandbox::run_main();
}
```
`CODEX_LINUX_SANDBOX_ARG0 = "codex-linux-sandbox"` (`sandboxing/src/landlock.rs:7`,
re-exported). `linux-sandbox/README.md` line 5: "a `codex-linux-sandbox`
standalone executable for Linux that is bundled with the Node.js version of
the Codex CLI." So it is both a standalone bin (`linux-sandbox/Cargo.toml`
`[[bin]] name = "codex-linux-sandbox"`) and reachable in-process via the arg0
dispatch, avoiding a second copy of the binary on disk for the Rust CLI.

## 4. Codex on Windows

Two backends, selected by `windows.sandbox` config
(`mxc-sandbox/README.md`, `windows-sandbox-rs/src/wrapper.rs:481-482`):
- `"restricted-token"` → `WindowsSandboxLevel::RestrictedToken`
- `"elevated"` → `WindowsSandboxLevel::Elevated`
- MXC (`SandboxType::WindowsMxc`, `protocol/src/sandbox.rs:15`) — routes
  through Microsoft's `BaseContainerRunner`. From `mxc-sandbox/README.md`:
  "It requires a working Windows process security environment (PSEC). It
  never invokes MXC's AppContainer dispatcher, edits host ACLs, creates
  sandbox users, runs setup, or requests elevation." Managed network for MXC
  "allows IPv4 and IPv6 loopback clients and servers... while denying direct
  non-loopback egress and general inbound network access."
- `windows-sandbox-rs/src/lib.rs:719,725`: "Restricted read-only access
  requires the elevated Windows sandbox backend" / "deny-read overrides
  require the elevated Windows sandbox backend" — so the lighter
  restricted-token backend can't do everything the elevated one can.

## 5. Codex approval / escalation interaction

`core/src/tools/orchestrator.rs:1-8` (module doc, verbatim):
> "Central place for approvals + sandbox selection + retry semantics. Drives
> a simple sequence for any ToolRuntime: approval → select sandbox → attempt
> → retry with an escalated sandbox strategy on denial (no re-approval thanks
> to caching)."

Flow, traced in code (`core/src/tools/orchestrator.rs:305-500`):
1. First attempt runs sandboxed (`initial_attempt`, `sandbox_requested` from
   `sandbox_manager.should_sandbox(...)`).
2. On failure, the error is pattern-matched for
   `CodexErrorDetails::Sandbox(SandboxErr::Denied { output, network_policy_decision })`
   (line 328). Any other error type returns immediately — only a sandbox
   denial triggers escalation.
3. If `!tool.escalate_on_failure()`, or if the approval policy is `Never`/
   `OnRequest` and `!tool.wants_no_sandbox_approval(...)`, or unsandboxed
   execution isn't allowed at all, the denial is returned as-is (no retry).
4. Otherwise the code builds a `retry_reason` (e.g. `"Network access to
   \"<host>\" is blocked by policy."` or a reason derived from the denied
   command's output), asks for approval via
   `tool_ctx.session.request_approval(...)` (unless
   `bypass_retry_approval` applies via strict-auto-review caching), then
   builds a `retry_attempt` and re-runs. `retry_sandbox_exe` is `None`
   (fully unsandboxed) when `unsandboxed_allowed`, otherwise it retries with
   the (possibly less restrictive) sandbox exe again.

Per-command override surfaced to the model: `core/src/tools/handlers/shell_spec.rs:239-263`
defines a `sandbox_permissions` tool-parameter value `"require_escalated"`,
described as "for unsandboxed execution," gated by a `justification` field
(`core/src/tools/handlers/mod.rs:101`: "`justification` requires an explicit
`sandbox_permissions`; use `sandbox_permissions: \"require_escalated\"` for
unsandboxed execution").

## 6. Codex size and dependencies (non-test `.rs`, `wc -l`)

| Crate | Lines | Notes |
|---|---|---|
| `linux-sandbox/src` | 7,269 | bwrap invocation, digest verify, arg0 dispatch, proxy routing |
| `sandboxing/src` | 4,178 | shared seatbelt/bwrap/landlock policy building, manager |
| `mxc-sandbox/src` | 779 | Windows MXC binding |
| `windows-sandbox-rs/src` | 23,385 | full Windows backend: ACLs, WFP-equivalent, service identity, provisioning |
| `bwrap/src` | 45 | tiny wrapper bin (`[[bin]] name = "bwrap"`) around a C-compiled dependency (`cc`, `pkg-config` build-deps) |

Note `windows-sandbox-rs` is far larger than the Linux/macOS backends
combined — Windows confinement (dedicated restricted token / elevated
process, ACL management, provisioning, setup) is structurally the heaviest
of the three platforms in this codebase.

## 7. Codex: does the sandbox cover MCP servers, child processes, apply_patch?

- **apply_patch**: yes. `core/src/tools/runtimes/apply_patch.rs:117` implements
  `fn sandbox_preference(&self) -> SandboxablePreference` — apply_patch goes
  through the same `ToolOrchestrator` sandbox-selection path as shell exec.
- **Child processes of a sandboxed command**: yes, by construction — Seatbelt
  base policy line 10-11: "child processes inherit the policy of their
  parent" (`(allow process-exec)` / `(allow process-fork)`); bwrap uses Linux
  namespaces, which are inherited by children.
- **MCP servers**: no — the MCP server *process itself* is launched
  unsandboxed. `rmcp-client/src/stdio_server_launcher.rs:625-640`, the
  `ExecParams` passed to start an MCP server process sets
  `sandbox: None`. (This is the executor/remote stdio launcher; the field
  exists specifically to be toggled, and here it is explicitly off for MCP
  server startup.) The MCP *protocol* separately has a capability flag
  `server_supports_sandbox_state_meta_capability` (`codex-mcp/src/binding.rs:287-288`)
  letting an MCP server advertise whether it understands Codex's sandbox
  state — this is metadata exchange, not confinement of the server process.

## 8. Claude Code (binary `~/.local/share/claude/versions/2.1.283`, via `strings`)

**Off by default.** Exact string extracted from the binary:
> "Exit with an error at startup if sandbox.enabled is true but the sandbox
> cannot start (missing dependencies or unsupported platform). When false
> (default), a warning is shown and commands run unsandboxed. Intended for
> managed-settings deployments that require sandboxing as a hard gate."

So Claude Code's default behaviour for a missing/broken sandbox is
**fail-open with a warning**, the opposite of Codex's fail-closed panic — a
strict mode (`sandbox.enabled: true` under managed settings) exists to make
it fail-closed instead.

No settings-key trace of a `~/.claude/settings.json` sandbox block on this
machine (checked `~/.claude/settings.json`: no `sandbox` key present, which
is consistent with the false default).

### macOS
- Uses `/usr/bin/sandbox-exec` (string: `/usr/bin/sandbox-exec`), i.e. macOS
  Seatbelt, same primitive as Codex.
- String: "macOS only: Allow access to com.apple.trustd.agent in the sandbox.
  Needed for Go-based CLI tools (gh, gcloud, terraform, etc.) to verify TLS
  certificates when using httpProxyPort with a MITM proxy and custom CA."
  This is an opt-in weakening (`sandbox.network.allowMachLookup`-style knob),
  confirmed by a companion string: "the sandbox on this device is configured
  with a setting that weakens its isolation (sandbox.allowAppleEvents,
  sandbox.enableWeakerNestedSandbox, or sandbox.network.allowMachLookup)."
- String: "Opening an app, file or URL with `open`, or scripting another app
  with `osascript`, is blocked inside this sandbox (macOS Launch Services and
  Apple Events are off) and fails with errors such as -10822" — so the
  macOS profile denies Launch Services / Apple Events by default, same
  spirit as Codex's Seatbelt.

### Linux
- Uses bubblewrap: strings `Linux bubblewrap`, `getIsBubblewrapSandbox`,
  `bubblewrap (bwrap) not executable at `, `bubblewrap (bwrap) not
  installed`, `install missing tools (e.g. apt install bubblewrap socat)`.
  Unlike Codex, there is **no bundled bwrap fallback** visible in strings —
  the user is told to `apt install bubblewrap`.
- Network/socket blocking uses a *separate* helper binary, `apply-seccomp`,
  shipped via the `@anthropic-ai/sandbox-runtime` npm package, not embedded
  in the main binary. Strings:
  - "[SeccompFilter] apply-seccomp binary not found in any expected location"
  - "[Sandbox Linux] apply-seccomp binary not available - unix socket
    blocking disabled. Install @anthropic-ai/sandbox-runtime globally for
    full protection."
  This is a **fail-open** path specifically for Unix-socket blocking: if the
  helper binary is missing, Claude Code still runs the sandboxed command
  (bwrap filesystem/namespace confinement still applies) but unix-socket
  blocking is silently dropped, with only a log line, not a hard failure.
  Environment override: `CLAUDE_CODE_BUBBLEWRAP` — "Linux/WSL only: Absolute
  path to the bwrap (bubblewrap) binary. Overrides auto-detection via PATH.
  Only honored from admin-controlled managed settings."
  Bwrap flags seen in strings: `--ro-bind`, `--unshare-net`, `--unshare-pid`,
  `--unshare-user`, `--tmpfs`.

### Windows
Structurally different from Codex: a **dedicated low-privilege OS user
account** plus **Windows Filtering Platform (WFP)** for network, not a
seatbelt/bwrap-style single-process sandbox:
- String: "Name for the dedicated sandbox user account that `srt-win
  install` creates and the sandboxed child runs as. Default:
  `srt-sandbox`."
- String: "No logout is needed: the WFP filter keys on the dedicated
  `srt-sandbox` user's SID, so your network is unaffected."
- Filesystem confinement is ACL-based, not namespace-based: strings
  `[Sandbox Windows] acl grant`, `[Sandbox Windows] acl revoke`,
  `[Sandbox Windows] fs applied:`.
- Requires a one-time elevated install: "Windows sandbox needs a one-time
  install (one UAC prompt): npx sandbox-runtime windows-install."
- String confirming this is deliberately network+process isolation, not
  filesystem, on Windows: "macOS and Linux/WSL only: skip filesystem
  isolation entirely while keeping network and seccomp isolation... Ignored
  on native Windows, where the sandboxed process runs as a separate user
  with no inherent rights, so skipping the filesystem rules would [imply a
  gap the flag exists to prevent]."
- Shell dependency: "Sandboxed bash on Windows requires Git Bash, which is
  not installed. Install Git Bash, or run this command unsandboxed
  (dangerouslyDisableSandbox)." — sandboxing on Windows only covers the
  Git-Bash-run shell tool, and PowerShell is a documented separate path.

### Escalation / bypass knobs (exact strings)
- `dangerouslyDisableSandbox` — per-command escalation parameter. Strings:
  - "You should always default to running commands within the sandbox. Do
    NOT attempt to set `dangerouslyDisableSandbox: true` unless..."
  - "The `dangerouslyDisableSandbox` parameter is disabled in this session's
    configuration; setting it does not take a command out of the sandbox."
    (i.e. it can itself be locked off by settings.)
  - "Treat each command you execute with `dangerouslyDisableSandbox: true`
    individually. Even if you have recently run a command with this
    setting, you should default to running future commands within the
    sandbox." — no caching/blanket escalation; it's requested per call.
- `excludedCommands` — allowlist of command patterns that skip the sandbox
  entirely. String: "All bash commands invoked by the model must run in the
  sandbox unless they are explicitly listed in excludedCommands." Also:
  "[sandbox] excludedCommands restricted to trusted settings tiers: ignoring
  ... sandbox.excludedCommands entr[ies]" — user/project-tier
  `excludedCommands` can be ignored under managed settings.
- `autoAllowBashIfSandboxed` — approval-prompt behaviour, defaults to true.
  String: "Sandboxed commands get unrestricted read/write access to the host
  filesystem; network egress is still confined to network.allowedDomains.
  ... Does not change Bash prompting: sandbox.autoAllowBashIfSandboxed is
  independent and still defaults to true." So by default, once a command is
  inside the OS sandbox, Claude Code auto-approves it without asking the
  user — the sandbox substitutes for the approval prompt, not the reverse.
- Network confinement is domain-allowlist based, not blanket-deny:
  `network.allowedDomains`, string: "When true, the sandbox runtime
  deterministically denies hosts not in allowedDomains instead of
  prompting." This implies the un-strict default is to *prompt* on
  non-allowlisted hosts rather than silently deny.
- File-read confinement is a separate, non-sandbox mechanism:
  `permissions.blockReadsOutsideWorkingDirectories` — string: "When set to
  `true`, Code sessions refuse reads outside their working directories (the
  session folder plus any `allowedWorkspaceFolders`). The file tools (Read,
  Grep, Glob) refuse them in every permission mode; where Claude Code's
  sandbox runs (macOS, or Linux and SSH hosts with bubblewrap...)." This
  option is off by default (no default-true evidence found) and is enforced
  partly by the file tools themselves, not only by the OS sandbox.

## 9. pi: no OS sandbox

`/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/docs/security.md`,
quoted verbatim (selected passages):

> "Pi can read, change, and execute files with the permissions of the
> account that started it, and it does not ask for approval before every
> tool call. Extensions, package installers, language servers, and other
> child processes run with those same permissions unless an
> operating-system or virtualization boundary restricts them."

> "Safety comes from limiting the files, credentials, processes, and network
> services Pi can access and affect if a generated action is wrong or
> hostile. Watching the transcript, using project trust, and reviewing
> changes do not create a security boundary."

Table in the doc ("Choose how to run Pi") lists three options: run directly
as the OS user (weakest), run entirely inside a container/VM/sandbox
("usually the strongest practical option" — external, not built-in), or run
Pi itself outside an isolated environment with only its built-in tools
running inside it.

> "Expected local-agent behavior, prompt injection from untrusted content,
> **lack of a built-in sandbox**, and behavior from user-installed
> extensions or skills are generally outside the security boundary unless
> the report demonstrates a privilege-boundary bypass..."

So pi explicitly disclaims having a built-in OS sandbox; all confinement is
delegated to an external container/VM chosen by the operator.

## Verification performed

Checked one claim against its source before finishing: that Codex's
Linux launcher prefers a system `bwrap` on `PATH` and falls back to a
bundled one, per `linux-sandbox/README.md`. Read the actual code at
`linux-sandbox/src/launcher.rs:38-60` (`exec_bwrap`) and
`linux-sandbox/src/launcher.rs:132-142` (`preferred_bwrap_launcher`,
`bundled_bwrap::launcher()` as the fallback branch) and confirmed the
enum `BubblewrapLauncher::{System, Bundled, Unavailable}` implements exactly
that order, with `Unavailable` causing a `panic!` (fail-closed) rather than
silently running the command unsandboxed. The doc and the code agree.

## Summary table: does each agent confine tools at the OS level?

| Agent | Built-in OS sandbox | macOS | Linux | Windows | Default | On failure |
|---|---|---|---|---|---|---|
| Codex | Yes | Seatbelt (`sandbox-exec`) | bubblewrap (namespaces) + seccomp (network); Landlock legacy | restricted-token / elevated process / MXC | sandboxed unless policy is `DangerFullAccess` | fail-closed (panics rather than running unsandboxed) |
| Claude Code | Yes, opt-in | Seatbelt (`sandbox-exec`) | bubblewrap + optional separate `apply-seccomp` helper | dedicated low-priv user + WFP + ACLs | **off** (`sandbox.enabled: false`) | fail-open with a warning by default; fail-closed only if `sandbox.enabled: true` is forced |
| pi | No | — | — | — | n/a | n/a — confinement is the operator's job (container/VM) |
