# Releasing and upgrading

How a Fiber version is cut and published, how a person installs it, and how
`fiber upgrade` replaces a running install. This is what is true now, not a
plan. It is settled by
[Releasing and upgrading: how a version ships](https://github.com/aakshintala/fiber/issues/68);
that ticket's resolution holds the rationale and the rejected alternatives.

What a release must pass before it publishes is `docs/ci.md`, "Releases".
What a release measures is `docs/performance.md`.

## Versions

Fiber's version follows SemVer and lives in the `fiber` binary crate's
`Cargo.toml`. The first release is `0.0.1`. Its git tag is `v0.0.1`.

Before 1.0, a change that breaks something a person or a supervisor relies on
bumps the minor number: the event stream's `schema_version`, a configuration
key, a command, a flag or an exit code. Anything else bumps the patch number.
From 1.0, a breaking change bumps the major number.

`fiber --version` prints the version and the commit, such as
`fiber 0.3.0 (4f2a9c1)`. A build from source between releases reports the last
released version and its own commit.

A release is cut when there is something worth installing, never on a
schedule.

## Cutting a release

A release is one pull request that changes the version in `Cargo.toml` and
adds that version's section to `CHANGELOG.md`. Merging it releases it.

When a push to `main` changes the version, the release workflow:

1. builds the three artifacts from that commit
2. runs the release gate in `docs/ci.md`, "Releases", against those exact
   artifacts
3. signs and notarises the macOS binary
4. creates the tag `v<version>`
5. publishes a GitHub Release whose notes are that version's `CHANGELOG.md`
   section

No step creates a tag before every earlier step has passed, so a failed
release leaves no tag behind. A failed run is re-run from the Actions tab on
the same commit. Nobody creates a version tag by hand.

After a release, a pull request lowers every ceiling in
`docs/performance.md` that the release measured at less than half its value,
to the measured value times two.

### The changelog

`CHANGELOG.md` is public product copy. Each version's section describes what a
person can observe, under the headings Breaking changes, New features,
Improvements, Bug fixes and Security, omitting any that are empty. Each entry
names the change in a few words, then says what it does. Entries carry no
pull request or issue numbers, commit hashes or contributor names, and nothing
without a user-facing outcome, such as CI, tests or refactoring.

A model drafts the section from the diff since the previous tag, with the
commit log as background only, because commit messages go stale and the diff
does not. The person cutting the release checks that every entry traces to the
diff before the pull request merges.

## What a release publishes

One archive per target, each with a checksum file beside it:

| Target | Archive |
|---|---|
| macOS arm64 | `fiber-aarch64-apple-darwin.tar.gz` |
| Linux x86_64 | `fiber-x86_64-unknown-linux-musl.tar.gz` |
| Linux arm64 | `fiber-aarch64-unknown-linux-musl.tar.gz` |

Each archive holds the `fiber` binary and `THIRD-PARTY-NOTICES`, the notices
file `docs/dependencies.md` requires. Each checksum file is named after its
archive with `.sha256` added, and holds that archive's SHA-256. The release
also carries `install.sh`.

Archive names carry no version, so
`https://github.com/aakshintala/fiber/releases/latest/download/<archive>`
always fetches the newest.

Each target is built on its own GitHub runner, never cross-compiled.

The Linux binaries are statically linked against musl, so one file runs on any
Linux distribution, including Alpine and ones with an older glibc. Their
memory allocator is musl's, since Fiber uses the system allocator
(`docs/dependencies.md`). CI's release-profile job builds the Linux x86_64
musl target, so the budgets in `docs/performance.md` measure the binary that
ships.

## Signing

The macOS binary is signed with the owner's Developer ID Application
certificate, with the hardened runtime and a secure timestamp, then notarised:
the release workflow uploads it to Apple's notary service and waits for the
automated scan to pass. A bare binary cannot carry the notarisation ticket
inside it, so when a copy downloaded through a browser first runs, Gatekeeper
checks the ticket online. A copy fetched by `curl`, `install.sh` or
`fiber upgrade` is never checked by Gatekeeper.

The certificate, its password, and the App Store Connect API key's ID, issuer
and private key are five secrets in the repository's `release` environment,
which only the release workflow can read. The certificate expires after five
years and is renewed by replacing the secrets.

The Linux binaries are not signed.

Every archive's integrity check is its SHA-256 file. It catches a corrupted or
truncated download. It does not protect against someone who controls the
repository, and neither would a signature made by that repository's own
release workflow. Extensions are not signed either (`docs/extensions.md`).

## Installing

```sh
curl -fsSL https://github.com/aakshintala/fiber/releases/latest/download/install.sh | sh
```

`install.sh` is POSIX `sh`. It:

1. detects the operating system and CPU, and stops on any combination that is
   not released
2. downloads the archive and its checksum file
3. checks the SHA-256 before moving anything into place
4. extracts the binary next to its destination and renames it into place, so a
   failed install never leaves half a binary
5. installs to `$FIBER_INSTALL_DIR`, or `~/.local/bin` if that is unset, and
   warns without failing when that directory is not on `PATH`
6. runs `fiber install` for the five first-party provider extensions

`FIBER_VERSION=0.3.0` installs that version instead of the newest. Rolling
back is installing an older version this way.

If step 6 fails, such as when `git` is missing, the binary stays installed and
`install.sh` exits non-zero with `fiber install`'s error. A later
`fiber install` or the model picker installs the providers
(`docs/extensions.md`, "A fresh install").

There is no Homebrew formula and no `cargo install`.

## Upgrading

`fiber upgrade` updates the binary and every installed extension together
(`docs/extensions.md`, "Staying current"). It changes only the binary and
`extensions/` in Fiber home, and never touches configuration, credentials,
sessions, rules, approvals or extension data (`docs/state.md`).

It:

1. finds the newest version by requesting
   `https://github.com/aakshintala/fiber/releases/latest` and reading the tag
   from the redirect, which is not a GitHub API call and so is not held to the
   API's limit of 60 requests an hour for each address
2. if that version is newer than the running one, downloads the archive for
   the running binary's own target and its checksum file into the directory
   that holds the binary, checks the SHA-256 and extracts the binary there
3. stages each installed extension at its newest version whose manifest
   accepts the Fiber version being installed, in a fresh directory
   (`docs/state.md`, "What each part holds")
4. renames the staged extensions into place, then the binary

Every download and check finishes before the first rename, so a failure up to
that point changes nothing. In a terminal, `fiber upgrade` shows the version
change and the extension summary `docs/extensions.md` describes, and asks once
before step 4. Without a terminal it goes ahead.

If the running version is already the newest, only the extensions are
updated. `fiber upgrade` never installs an older version.

The binary it replaces is the one that is running. If Fiber cannot write to
that binary's directory, such as `/usr/local/bin` owned by root, it stops with
an error naming the directory. It never asks for elevated privileges.

After step 4, if `fiber remote` is running, `fiber upgrade` restarts it
(`docs/invocation.md`).

Nothing checks for a new version on its own. An idle Fiber does no work.

### How `fiber upgrade` replaces the binary

The new binary is renamed over the old one, never written into it. Writing in
place is refused on Linux with `Text file busy`, and on macOS it gets every
later launch from that file killed. After a rename, a running Fiber keeps the
file it started from until it exits
([research/binary-replacement](../research/binary-replacement/README.md)).

A running Fiber sometimes starts another: the terminal starts `fiber serve`,
and a session starts a delegate. Fiber records its own path when it starts and
always starts that path. After an upgrade, the new process runs the new
binary. Asking the operating system for the running binary's path instead
would fail on Linux, where the path then reads `fiber (deleted)`.

A parent and child on different versions read each other only when they can
read each other's `schema_version` (`docs/invocation.md`), so a breaking
difference is a clear error, not a misread stream.
