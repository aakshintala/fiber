# Installing Fiber

> **Draft.** The install and update flow is still being built ([#412](https://github.com/aakshintala/fiber/issues/412), [#408](https://github.com/aakshintala/fiber/issues/408), and the nightly ticket). This page describes what is true now; anything here can change before the friends release.

Install with the script from the newest release:

```sh
curl -fsSL https://github.com/aakshintala/fiber/releases/latest/download/install.sh | sh
```

What the script does, and what it reads, is [Installing](../releasing.md). In short: it detects the operating system and CPU, downloads the archive and its checksum file, checks the SHA-256 before moving anything into place, and renames the binary into its destination, so a failed install never leaves half a binary. It installs to `FIBER_INSTALL_DIR`, or `~/.local/bin` when that is unset, and warns without failing when that directory is not on `PATH`. The install ends by running [`fiber setup`](../invocation.md) once, which asks the setup questions in one pass.

Pick a version by putting `FIBER_VERSION` in front of `sh`:

```sh
curl -fsSL https://github.com/aakshintala/fiber/releases/latest/download/install.sh | FIBER_VERSION=0.3.0 sh
```

Rolling back is installing an older version the same way. There is no Homebrew formula and no `cargo install`. `fiber setup` is re-runnable at any time: run it again to change any answer, and already-finished steps stay finished.

The script follows a channel setting, stable by default: `--channel nightly`
or `FIBER_CHANNEL=nightly` installs the newest nightly pre-release instead.
An explicit `FIBER_VERSION` wins over the channel. What each channel reads
is [Installing](../releasing.md).

## Updating

`fiber update` updates the binary and every installed extension together, and nothing else: never configuration, credentials, sessions, rules, approvals, pinned copies or extension data. How it stages every download before the first rename, and what it does when the hub is running, is [Updating](../releasing.md).

If the running version is already the newest, only the extensions are updated. `fiber update` never installs an older version. Nothing checks for a new version on its own.
