# Releasing and upgrading

How a Fiber version is cut and published, how a person installs it, and how
`fiber update` replaces a running install. This is what is true now, not a
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
released version and its own commit. A build with no git history, such as a
source tarball or a mutation-testing copy, prints `fiber <version>`.

Stable releases are versioned by PR: 0.0.N until the public launch, which is
0.1.0.

A nightly pre-release is built from main once a day, only when main changed
since the last nightly, for example `0.0.N-nightly.YYYYMMDD`. A nightly is a
pre-release of the next stable version: with `Cargo.toml` at `0.0.0`, the
first nightly is `0.0.1-nightly.YYYYMMDD`, and SemVer sorts it below `0.0.1`.

A stable release is cut when there is something worth installing, never on a
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

After a stable release, a pull request lowers every ceiling in
`docs/performance.md` that the release measured at less than half its value,
to the measured value times two.

### Nightly pre-releases

The nightly job runs on a daily schedule, and on a manual dispatch of
`release.yml` from `main` (`gh workflow run release.yml`) under the same check:
its preflight computes the next nightly
version from the `fiber` crate's `Cargo.toml` and skips every later job when
main is unchanged since the newest nightly tag or when that version's tag
already exists, so a second nightly the same day, scheduled after a manual
one, skips. A nightly is published as a
GitHub pre-release, so `releases/latest` and the stable channel never see
it. The draft is created with `--prerelease` and published last, with the
same rule as a stable release: publishing is what creates the tag, so a
failed run leaves no tag and no Release. A nightly ships the same three
target archives plus the docs, extensions and installer assets, through the
same gate and the same signing. A nightly writes no `CHANGELOG.md` section
and requires none; its notes are the `git log` range since the previous
stable tag. Nightly jobs reuse the stable steps verbatim so their
measurements are reported ungated exactly as `docs/ci.md`, "Releases",
describes, while ceiling-lowering follow-up pull requests stay on stable
releases, which key off `CHANGELOG.md` sections and version bumps that
nightlies never write.

### Rehearsing the publish path

A rehearsal dispatch (`gh workflow run release.yml --ref main -f rehearse=true`)
runs only the `rehearsal` job. `docs/ci.md`, "Releases", has what it proves
and what it leaves out.

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
file `docs/dependencies.md` requires. The template is the root `about.hbs`
and the licence config is the root `about.toml`. Each checksum file is named
after its archive with `.sha256` added. Each checksum file holds the
archive's SHA-256 as 64 hex digits, optionally followed by whitespace and
the archive's name, as `sha256sum` writes it. The release also carries
`install.sh`.

The release also carries `fiber-docs.tar.gz` and its checksum file: the
repository's `docs/` tree at the release tag, without the documents about
developing Fiber itself (`docs/agents/` and `docs/workflow.md`). The archive
includes the built-in skills, `docs/skills/<name>/SKILL.md`
(`docs/system-prompt.md`, "Skills"). It is the same for every target.
Nothing in the binary embeds documentation.

The release also carries `fiber-extensions.tar.gz` and its checksum file:
every first-party extension at the release tag: every directory under
`providers/` and `extensions/` at the release tag. It is the same for every target. Nothing
in the binary embeds an extension (`docs/extensions.md`, "A fresh install").
The run fails when the packed set is empty or two top-level names collide.
At its top level the archive has one directory per extension, named as it is
installed under `extensions/` in Fiber home (`docs/extensions.md`, "Names"),
with no install record. The docs archive holds the contents of `docs/` at its
top level. The docs and extensions archives are plain ustar
(`tar --format=ustar`), holding only files, directories and relative symlinks
that stay inside their directory.

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
`fiber update` is never checked by Gatekeeper.

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
5. installs to the directory the person picks, offered as
   `$FIBER_INSTALL_DIR`, or `~/.local/bin` if that is unset. When that
   directory is not on `PATH`, it asks whether to add it, and yes appends one
   line to the login shell's startup file (`~/.zshrc`, `~/.bashrc`, or
   `fish_add_path` in `~/.config/fish/config.fish`) unless the line is
   already there. No leaves `PATH` alone and prints the line to add.
6. runs the binary it installed, once, with the internal install step,
   passing the version. That step downloads the same release's docs and
   extensions archives, checks each SHA-256, and puts `docs/` and each
   first-party extension in place in Fiber home by extracting beside and
   renaming, as `fiber update` does. It takes the extensions lock, and records
   each extension at the release's version and the commit the binary was built
   from.
7. runs `fiber setup` (`docs/invocation.md`, "Commands and flags"), which asks the
   rest: providers, import, default model, completions and the hub.

The script reads its questions from `/dev/tty`, so they work under
`curl | sh`. Its flags answer them, after `sh -s --`:

```sh
curl -fsSL .../install.sh | sh -s -- --dir ~/bin --no-path -- --provider openrouter --no-hub
```

`--dir <path>` picks the directory, `--path` and `--no-path` answer the `PATH`
question, and `--no-setup` skips step 7. Everything after a second `--` goes
to `fiber setup` unchanged. Without a terminal the script asks nothing: it
installs to the default directory, leaves `PATH` alone with a warning, and
runs `fiber setup` without a terminal, which does only what its flags say.

The binary unpacks the archives itself and refuses any entry that is not a
file, a directory or a relative symlink inside its directory.

The script is `install.sh` in the repository's `scripts/` directory. It reads only
`FIBER_INSTALL_DIR`, `FIBER_VERSION` and `FIBER_CHANNEL`; the binary reads `FIBER_HOME`.

`FIBER_VERSION=0.3.0` installs that version instead of the newest, with that
version's docs and first-party extensions. Rolling back is installing an older
version this way.

`fiber update` and `install.sh` follow a channel setting, stable by default.
`install.sh` takes `--channel stable|nightly`, or `FIBER_CHANNEL` when the
flag is absent; an empty `FIBER_CHANNEL` means stable, and the flag beats the
environment either way. An explicit `FIBER_VERSION` wins over the channel and
the channel is ignored. A nightly-form `FIBER_VERSION` stays refused by the
version check above: pinning a single nightly is out of scope. The stable
channel installs from `latest/download` exactly as above, which excludes
pre-releases by GitHub's own rule, so stable ignores nightlies with no
further code. The nightly channel installs the newest nightly: GitHub's
`releases/latest` skips pre-releases, so the nightly channel reads the
release list instead, `https://api.github.com/repos/aakshintala/fiber/releases?per_page=100`,
and picks the newest entry whose tag matches `v<N>-nightly.YYYYMMDD` with
`"prerelease": true` and `"draft": false`, installing from that tag's
`download/<tag>/` URLs. The list is a GitHub API call and so is held to the
API's limit of 60 requests an hour for each address. A failed list fetch
fails the install naming the URL, and no matching nightly fails naming the
channel.

Running `install.sh` again reinstalls the first-party extensions, including
one a person removed. `fiber update` never does: it updates only what is
installed.

There is no Homebrew formula and no `cargo install`.

## Updating

`fiber update` updates the binary and every installed extension together
(`docs/extensions.md`, "Staying current"). It changes only the binary and
`extensions/` and `docs/` in Fiber home, and never touches configuration,
credentials, sessions, rules, approvals, pinned copies or extension data
(`docs/state.md`).

It:

1. finds the newest version on its channel: on stable by requesting
   `https://github.com/aakshintala/fiber/releases/latest` and reading the tag
   from the redirect, which is not a GitHub API call and so is not held to the
   API's limit of 60 requests an hour for each address; on nightly from the
   release list instead, whose match rule and limit are the channel's own,
   shared with `install.sh` ("Installing" above)
2. if that version is newer than the running one, downloads the archive for
   the running binary's own target and its checksum file into the directory
   that holds the binary, checks the SHA-256 and extracts the binary there,
   and downloads and checks the same release's docs and extensions archives
3. stages each installed extension in a fresh directory (`docs/state.md`,
   "What each part holds"): a first-party one from the extensions archive,
   and any other at its newest version whose manifest accepts the Fiber
   version being installed
4. renames the staged extensions into place, then the docs, then the binary

Every download and check finishes before the first rename, so a failure up to
that point changes nothing. In a terminal, `fiber update` shows the version
change and the extension summary `docs/extensions.md` describes, and asks once
before step 4. Without a terminal it goes ahead.

If the running version is already the newest, only the extensions are
updated, and the docs are installed if `docs/` is missing from Fiber home. `fiber update` never installs an older version.

The binary it replaces is the one that is running. If Fiber cannot write to
that binary's directory, such as `/usr/local/bin` owned by root, it stops with
an error naming the directory. It never asks for elevated privileges.

After step 4, if the hub is running, `fiber update` restarts it: through
its login service when it is installed (`launchctl kickstart` on macOS,
`systemctl --user restart` on Linux), and otherwise by sending it `stop`, after
which the next client starts a new one. Clients reconnect with backoff, and
no session stops (`docs/invocation.md`, "The hub").

Nothing checks for a new version on its own. An idle Fiber does no work.

`fiber update` takes `--channel stable|nightly`, or `FIBER_CHANNEL` when the
flag is absent; an empty or unset `FIBER_CHANNEL` means stable, and the flag
beats the environment. An invalid channel is a usage error naming it.
`upgrade` runs the same command with the same flag. The stable channel finds
the newest version through `releases/latest` as above; the nightly channel
reads the release list instead and updates from its nightlies, because
GitHub's `releases/latest` skips pre-releases. The list, its match rule,
its API limit and its failure modes are the channel's own, shared with
`install.sh` ("Installing" above).

### How `fiber update` replaces the binary

The new binary is renamed over the old one, never written into it. Writing in
place is refused on Linux with `Text file busy`, and on macOS it gets every
later launch from that file killed. After a rename, a running Fiber keeps the
file it started from until it exits
([research/binary-replacement](../research/binary-replacement/README.md)).

A running Fiber sometimes starts another: the terminal starts the hub, the
hub starts sessions, and a session starts a delegate. Fiber records its own path when it starts and
always starts that path. After an upgrade, the new process runs the new
binary. Asking the operating system for the running binary's path instead
would fail on Linux, where the path then reads `fiber (deleted)`.

A parent and child on different versions read each other only when they can
read each other's `schema_version` (`docs/invocation.md`), so a breaking
difference is a clear error, not a misread stream.
