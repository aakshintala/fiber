# Dependencies

Which crates Fiber depends on, which parts it writes itself, and what a crate
must pass to be admitted. This is what is true now, not a plan. It is settled
by
[Dependency policy: which crates, and what we write ourselves](https://github.com/aakshintala/fiber/issues/65);
that ticket's resolution holds the rationale and the rejected alternatives.

The crates are `docs/architecture.md`. Threads rather than an async runtime is
[ADR 0004](adr/0004-blocking-threads-no-async-runtime.md). Which CI jobs run
where is `docs/ci.md`. What Fiber
promises about memory is
`docs/performance.md`.

## Admitting a crate

Every direct dependency of every Fiber crate is listed in the tables on this
page, with what Fiber uses it for. CI fails when a `Cargo.toml` names a crate
that is not listed. Adding a crate therefore means editing this page in the
same pull request.

A crate is admitted when:

- its memory cost and the crates it adds are measured and recorded here (see
  "Measuring memory")
- it does not need an async runtime, which ADR 0004 rules out; rmcp, reqwest
  and sqlx fail on this alone
- its licence is on the allowed list and it has no open advisory (see "Supply
  chain")
- something Fiber has decided to build uses it
- it is maintained: an advisory marking it unmaintained is the test, so
  cargo-deny checks this with the rest
- the pull request that adds it says what Fiber would otherwise have to write

Fiber writes a thing itself when it is a small, fully specified format that
Fiber owns end to end, such as server-sent events or JSON-RPC. It also writes
one when the only crates for it fail the rules above.

The `tui` crate prefers an existing crate to writing its own. Its crates meet
every rule above, and their memory is measured and recorded, but memory is not
a reason to refuse one: the terminal is its own process
(`docs/architecture.md`), so nothing it admits reaches a session's memory. No
other Fiber crate depends on a crate admitted only for `tui`, and CI checks
each crate's dependency tree for it.

Crates used only by the image child (`docs/invocation.md`, "Processes") meet
every rule above, and their memory is measured and recorded, but memory is not
a reason to refuse one: the session never runs image code, so nothing they
admit reaches a session's memory.

Transitive crates are not listed. Each one's memory is counted in the direct
crate that pulls it in, and cargo-deny checks its licence and advisories.

`unsafe` Rust in a dependency is not counted or gated. The runtime table
records which crates carry C or assembly, because a crash there ends the
whole process, which
[ADR 0009](adr/0009-each-session-is-one-process.md) accepts. Rules for
Fiber's own `unsafe` code are `docs/code-quality.md`.

Binary size is recorded, not gated per crate. CI fails a stripped release
binary over 20 MiB. Compile time is not a criterion: CI caches built
dependencies. On macOS arm64, serde, ureq, ratatui, rusqlite and mlua build from clean in 8
seconds, and adding ten candidates, syntect among them, takes it to 16.

## Measuring memory

`research/dependency-rss` measures each crate alone. A small program runs a
fixed workload shaped like Fiber's use of the crate, such as one HTTPS request
through the OS trust store for ureq, or raw mode on a pseudo-terminal for
crossterm. `run.sh` builds one binary per crate and reports the median of 5
runs, minus a program that does nothing. It also builds every runtime crate
together.

- Linux reports peak RSS. It counts the pages of the binary's own code that
  ran. That is why rusqlite, whose SQLite code is 1.7 MiB, costs about 2 MiB
  on Linux.
- macOS reports peak memory footprint, the figure Activity Monitor shows.
  macOS RSS also counts system framework pages shared with every other
  process. A crate that links `Security.framework` shows about 4.5 MiB of RSS
  before it runs a line of code, and about 0.6 MiB of footprint.

Each figure is the crate standalone. Crates that share dependencies cost less
together than the sum of their rows, so the table also has a row with all
runtime crates linked into one binary. Its workloads run one after another,
so that row is the peak of the busiest one on top of everything linked, not
the cost of all of them holding memory at once. The image child's crates are
not in that together binary: the session never runs image code.

A new crate gets a workload in the probe and a row here, measured on Linux
x86_64, Linux arm64 and macOS arm64. A crate is measured again when its major
version or its enabled features change. What a running session holds, broken down by Fiber's own
crates, belongs to the memory budget (`docs/performance.md`).

Dev-dependencies are compiled only into tests and jigs (`docs/testing.md`,
"Jigs"). They never reach the shipped binary, so they have no memory row.

## Runtime dependencies

Measured on September 25, 2026 with rustc 1.98.1. Linux is GitHub's
`ubuntu-24.04` and `ubuntu-24.04-arm` runners, and macOS is an Apple M3 Pro.
Memory is in KiB over a program that does nothing. Differences under 200 KiB
are run-to-run noise and show as ~0. Crates is the number of crates in the
crate's own tree. Binary is the stripped Linux x86_64 release binary with
only that crate, in KiB; the empty program is 323 KiB.

| Crate | Used for | Linux x86_64 | Linux arm64 | macOS arm64 | Crates | Binary |
|---|---|---:|---:|---:|---:|---:|
| serde, serde_json | the log, the event stream, every wire format | ~0 | ~0 | ~0 | 11 | 418 |
| ureq, rustls-platform-verifier | HTTP over TLS, trusting the OS certificate store | 3,356 | 3,076 | 2,048 | 31 | 2,545 |
| ratatui | drawing the terminal UI | 1,084 | 1,344 | 1,376 | 41 | 469 |
| crossterm | terminal input, raw mode and output | ~0 | 256 | ~0 | 28 | 443 |
| mlua | the extension runtime, Lua 5.4 vendored | 912 | 704 | ~0 | 23 | 785 |
| clap | the command line | 452 | 448 | ~0 | 17 | 782 |
| thiserror | error types in library crates | ~0 | ~0 | ~0 | 6 | 325 |
| signal-hook | SIGTERM, SIGINT and SIGHUP | ~0 | ~0 | ~0 | 4 | 352 |
| getrandom | random ids | ~0 | ~0 | ~0 | 3 | 325 |
| ring | SHA-256, for PKCE and extension binary checksums | ~0 | ~0 | ~0 | 8 | 341 |
| base64 | PKCE, and attachments sent to providers | ~0 | ~0 | ~0 | 1 | 328 |
| rustix | the shell tool's pseudo-terminal, new session and process group | ~0 | ~0 | ~0 | 4 | 330 |
| ignore, grep-searcher, grep-regex | the search behind the shell's `grep` and `find` (`docs/tools.md`, "Search") | 2,656 | 2,480 | 1,904 | 25 | 2,886 |
| similar | an edit's diff in `details` (`docs/tools.md`, "edit") | ~0 | 380 | ~0 | 1 | 389 |
| all of the above together | | 7,048 | 6,104 | 4,288 | 136 | 6,452 |
| image, fast_image_resize | the image child; png, jpeg, gif and webp only (`docs/model-routing.md`, "Image limits") | 68,076 | 67,604 | 72,352 | 32 | 5,234 |

Notes:

- ureq's figure is one live HTTPS request to example.com, through the OS
  trust store and the custom connector that keeps the socket for
  cancellation (`docs/architecture.md`, "Cancellation"). ring, which rustls
  uses for cryptography, carries C and assembly. aws-lc-rs, the alternative
  provider, adds 6 crates and 663 KiB for nothing Fiber needs.
- mlua carries Lua's C source.
- ratatui's figure is its two 200 by 50 screen buffers. Any full-screen
  terminal UI holds a screen model of that size.
- thiserror, signal-hook, getrandom, ring and rustix are already in the tree
  through other crates (rustls, crossterm, mlua), so listing them directly
  adds no crate.
- serde_json's `preserve_order` feature is never enabled
  (`docs/prompt-cache.md`).
- The search row is ripgrep's walker (`ignore`), its search loop
  (`grep-searcher`) and its regex adapter (`grep-regex`, which brings
  `regex`), measured together walking the probe's own tree and searching every
  file. Alone, `regex` measured 1,780 KiB and `ignore` 1,392 KiB on Linux
  x86_64; about 440 KiB of `regex`'s binary is Unicode tables. The search,
  similar and together rows were measured on September 26, 2026.
- The image row is `image` 0.25 with default features off and only the png,
  jpeg, gif and webp codecs, plus `fast_image_resize` 6 with its `image`
  feature, Lanczos3. It is pure Rust and passes cargo-deny. Memory is the
  image child's, measured on September 29, 2026
  (`research/image-limits/README.md`); the session never runs image code, so
  the "all of the above together" row does not include it. An 81-megapixel
  PNG, which the 50-megapixel cap refuses, peaks at 308,372 KiB in the probe on Linux x86_64. A header-only read with
  `image` costs 4,184 KiB in the probe, which is why the child and not the session
  reads the header. `image` alone is not used: its own resize builds a
  full-width f32 buffer and doubles memory and time (157,120 KiB against 72,352 KiB
  peak, 201 ms against 107 ms per 12 MP photo, macOS). Separate crates
  (zune-jpeg, png, gif, image-webp, fast_image_resize, jpeg-encoder) are not
  used: jpeg-encoder's licence includes IJG, which is not on the allowed
  list.

### Root certificates

Fiber trusts the operating system's certificate store, through
rustls-platform-verifier, so a certificate installed by a company that
inspects TLS works as it does in curl or a browser. On Linux the verifier reads
only the system store. When that store is empty, as in a minimal container
without `ca-certificates`, Fiber falls back to Mozilla's root list, which is
compiled in through ureq. Empty means the platform's certificate loader
returned no certificates. A test runs Fiber against an empty store and checks
it connects through the fallback.

## Programs Fiber runs

Fiber needs no program installed beyond the operating system's own, with three
exceptions:

- The shell tool runs `/bin/bash`, or `sh` where bash does not exist
  (`docs/tools.md`, "Shell").
- Reading a PDF for a provider that cannot take one natively renders its
  pages with poppler's `pdftoppm`. It is optional: without it, that one call
  fails and names the package (`docs/tools.md`, "read").
- Installing, updating or fetching an extension runs the system `git`, so a
  person's SSH keys and credential helpers apply (`docs/extensions.md`,
  "Names"). Without it the command fails with `usage` and says to install git
  (`docs/errors.md`).

`grep` and `find` in the shell tool run Fiber's own search, not a system
program (`docs/tools.md`, "Search").

## Waiting on other decisions

These crates are the choice if the named decision needs one. Each is measured
already.

| Crate | Needed if | Linux x86_64 | Linux arm64 | macOS arm64 | Crates | Binary |
|---|---|---:|---:|---:|---:|---:|
| rusqlite, SQLite bundled | Fiber keeps a derived database, such as for cross-session search; today listing sessions reads the logs (`docs/state.md`) | 2,236 | 1,984 | ~0 | 14 | 2,268 |
| pulldown-cmark | the terminal UI renders markdown ([Epic: TUI](https://github.com/aakshintala/fiber/issues/82)) | 428 | 384 | ~0 | 4 | 724 |

rusqlite carries SQLite's C source.

Syntax highlighting is the terminal UI's decision. The obvious crate, syntect,
costs 8,704 KiB on Linux x86_64 just to load its syntax definitions. It has 44
crates, and cargo-deny fails it out of the box on two unmaintained crates,
yaml-rust and bincode.

## Written ourselves

| What | Why not a crate |
|---|---|
| Server-sent events parsing | a line protocol of a few dozen lines |
| MCP's JSON-RPC and both transports | rmcp needs tokio |
| Checking tool arguments against their input schema | see below |
| The shell tool's command recogniser | it must fail closed, not parse all of shell |
| BM25 for `tool_search` | one scoring formula over a few hundred short documents |
| Comparing extension version tags | a `v1.4.0` tag is three numbers compared in order |
| The OAuth callback listener | one request on a `std::net::TcpListener` |
| Percent-encoding OAuth URLs and parsing the callback query | a fixed format from RFC 3986, a few dozen lines |
| File locking | `std::fs::File::lock`, stable since Rust 1.89 |
| Timestamps | the log's `ts` is milliseconds since the epoch, from `std::time` |

Tool arguments are checked against a subset of JSON Schema: `type`,
`properties`, `required`, `additionalProperties`, `enum`, `items`, `minimum`,
`maximum`, `minLength`, `anyOf` and `$ref`. A `$ref` is followed only within
the same schema, such as `#/$defs/name` or `#/definitions/name`; one that
points elsewhere, or leads back to itself, fails the check with a message
saying so. A built-in tool's schema uses only the
subset, and a test fails if one does not. In an extension's or an MCP
server's schema, a keyword outside the subset is skipped, not failed, so a
server whose schema uses one still works. Checking those schemas is best
effort. The jsonschema
crate covers the whole specification, but it costs 14,808 KiB on Linux x86_64
and brings 79 crates, more than every runtime crate together.

The shell tool's recogniser splits a command on `&&`, `||`, `;` and `|` and
reads each part as plain words. Anything it cannot read plainly makes the
call declare `executes`, as `docs/tools.md` requires. codex parses shell with
tree-sitter-bash; a recogniser that fails closed does not need a full parser.

Fiber uses the system allocator. Whether another allocator lowers resident
memory is for the memory budget (`docs/performance.md`) to measure.

## Tests and development tools

Dev-dependencies are listed here, and CI checks them like any other
dependency.

| Crate or tool | Kind | Used for |
|---|---|---|
| insta | dev-dependency | whole-screen and value snapshots (`docs/testing.md`) |
| proptest | dev-dependency | property tests that shrink and replay a failing case by seed |
| cargo-nextest | tool | running tests, one process each |
| cargo-mutants | tool | the mutation check on every pull request |
| cargo-deny | tool | licences, advisories and crate sources |
| cargo-about | tool | the release's third-party notices file |
| xtask | tool | the workspace's own CI helper, `cargo xtask`: selection, the `CI` verdict and the gate's checks (`docs/ci.md`). It uses serde_json, proc-macro2 and pulldown-cmark, and no Fiber crate depends on it |
| proc-macro2 | xtask dependency | tokenising Rust source for the `unsafe` table check (`docs/code-quality.md`, "`unsafe`") |
| pulldown-cmark | xtask dependency | reading Markdown for the docs check (`docs/ci.md`, "The docs check"); the terminal UI's use waits below |

## Supply chain

- `Cargo.lock` is committed. Fiber is a binary, and a release must build from
  exactly the versions that were tested.
- cargo-deny runs in CI and checks licences, advisories and that every crate
  comes from crates.io.
- Allowed licences: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception,
  BSD-2-Clause, BSD-3-Clause, ISC, Zlib, BSL-1.0, Unicode-3.0,
  CDLA-Permissive-2.0, Unlicense, CC0-1.0, MIT-0 and MPL-2.0. Copyleft
  licences that reach beyond the file, such as GPL, LGPL and AGPL, are not
  allowed. A crate offered under several licences passes if one of them is
  on the list.
- Every release ships a notices file with the licence text and copyright
  notice of every crate compiled into the binary. cargo-about generates it
  from `Cargo.lock`, and CI fails if it cannot.
- An advisory fails a pull request that changes `Cargo.lock`, the daily
  advisory run and a release (`docs/ci.md`), including one that marks a crate
  unmaintained.
  An exception names the advisory, the path that pulls the crate in and the
  condition for removing the exception, as codex's `deny.toml` does.

## Toolchain

Fiber has no separate minimum supported Rust version. It is a binary, not a
library, so the toolchain CI pins is the only version Fiber supports.
