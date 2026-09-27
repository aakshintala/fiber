# Linux confinement on GitHub-hosted runners: live probe results

Ticket: Fiber #30.

Run: https://github.com/aakshintala/fiber/actions/runs/36297198603
Runners: `ubuntu-latest` (x86_64) and `ubuntu-24.04-arm` (aarch64), matrix job in the same run.
Probe crate: `landlock` v0.4.7 (current on crates.io at time of probe). No `seccompiler` used or needed:
Landlock is an LSM enforced by the kernel via the `landlock_*` syscalls the `landlock` crate wraps
directly; the checks below need no seccomp-bpf filter.

Both runners returned byte-identical facts except hostname and `uname -m`.

## 1. Kernel, ABI, LSM list

| | ubuntu-latest | ubuntu-24.04-arm |
|---|---|---|
| `uname -r` | `6.17.0-1022-azure` | `6.17.0-1022-azure` |
| `uname -m` | `x86_64` | `aarch64` |
| `/sys/kernel/security/lsm` | `lockdown,capability,landlock,yama,apparmor,ima,evm` | same |
| Landlock ABI (via crate's `RestrictionStatus.landlock`) | `Available { effective_abi: V7, kernel_abi: None }` | same |

`kernel_abi: None` means the kernel's raw `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)`
return value equals the crate's own `ABI::V7` (i.e. the kernel supports exactly ABI 7, nothing beyond what
the crate knows). ABI V7 shipped in Linux 6.15; this 6.17 kernel still reports V7 as the ceiling (no kernel
has shipped V8 yet). ABI V4 (needed for network scoping) is well covered.

Full `RestrictionStatus` the probe printed on both runners:

```
landlock-probe: restriction status = RestrictionStatus { ruleset: PartiallyEnforced, no_new_privs: true, landlock: Available { effective_abi: V7, kernel_abi: None }, log_same_exec: true, log_new_exec: false, log_subdomains: true, all_threads: false }
```

`ruleset: PartiallyEnforced` because the probe requests `AccessFs::from_all(ABI::V9)` (the crate's full,
forward-looking access set) with the crate's default best-effort compat level; V7-and-below rights are
fully enforced, the handful of V8/V9-only fs rights are silently skipped rather than erroring.

## 2. Landlock fs + net enforcement (same on both runners)

Probe: restrict to read-everywhere, write-only to an allowed tmp dir + `/tmp`, deny all TCP connect
(no `NetPort` allow-rules added once `AccessNet::ConnectTcp` is handled), then exec
`/bin/bash -c 'bash tests.sh <allowed_dir>'`.

```
=== write inside allowed dir (expect ok) ===
exit=0
hello

=== write to $HOME/x (expect EACCES) ===
/home/runner/work/fiber/fiber/probe/landlock/tests.sh: line 13: /home/runner/x: Permission denied
exit=1

=== echo > /tmp/x (expect ok) ===
exit=0
hello

=== cat /etc/hostname (expect ok) ===
runnervmtr4k5        (arm runner: runnervmoyp6c)
exit=0

=== curl -sS https://example.com (expect fail if net ABI enforced) ===
curl: (7) Failed to connect to example.com port 443 after 5 ms: Couldn't connect to server
exit=7
```

All four fs expectations hold exactly (write allowed in the granted dir and `/tmp`, denied under
`$HOME`, read allowed everywhere). Since ABI is V7 (>= 4), the network test was also run: TCP connect
is denied kernel-side with no network reachability at all — `curl` fails at connect() in a few
milliseconds (not a DNS or TLS failure), confirming Landlock, not some other layer, blocked it.

## 3. Unprivileged user namespaces / AppArmor userns restriction

```
=== unshare -Urn true ===
unshare: write failed /proc/self/uid_map: Operation not permitted
exit=1

=== bwrap --version ===
bwrap not installed

=== sysctl kernel.apparmor_restrict_unprivileged_userns ===
kernel.apparmor_restrict_unprivileged_userns = 1
```

Identical on both runners. `unshare -Urn` (new user+mount+net namespace, map current user to root)
fails outright with `EPERM` on `write(/proc/self/uid_map)`, consistent with Ubuntu 24.04's AppArmor
unprivileged-userns restriction being on (`kernel.apparmor_restrict_unprivileged_userns = 1`) and no
AppArmor profile on these runners' `unshare`/shell granting the exception. `bwrap` (bubblewrap) is not
preinstalled on either runner image.

## 4. seccompiler

Not needed and not used. Landlock is a first-class LSM; the crate issues `landlock_create_ruleset`,
`landlock_add_rule`, `landlock_restrict_self` directly. No seccomp-bpf filter was required for any of
the fs/net checks above.

## Practical implications for Fiber

- Landlock fs + net confinement (read-everywhere / write-listed-dirs / deny-TCP-connect) works
  out of the box on both GitHub-hosted Linux runner families, no setup beyond the `landlock` crate.
- Do **not** rely on `unshare -Urn` / user-namespace-based sandboxing on these runners as shipped:
  it is blocked by the AppArmor unprivileged-userns restriction, and `bwrap` isn't installed either.
  A namespace-based fallback would need either root, a custom AppArmor profile, or dropping the
  restriction (not available to us on shared runners).
