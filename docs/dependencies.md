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
binary of 20 MiB (20,971,520 bytes) or more (`scripts/release-size`). Compile time is not a criterion: CI caches built
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

Run the probe from a session with
`gh workflow run dependency-probe.yml --ref <branch>`, then
`gh run watch` and `gh run download <run-id>`. The workflow
(`.github/workflows/dependency-probe.yml`) runs `run.sh` on `ubuntu-24.04`,
`ubuntu-24.04-arm` and `macos-26` (macOS arm64), and uploads each table as the
artifact `dependency-rss-<runner>`. A runner's figures differ from run to
run by the noise floor below. The workflow's one input, `probe`, is a choice:
`dependency-rss` (the default) or `session-search`, which runs
`research/session-search/run.sh`, the timings of the session search scan, with
`-f probe=session-search`. It uploads `session-search-<runner>`; only the Linux
runners report cold-cache rows.

A new crate gets a workload in the probe and a row here, measured on Linux
x86_64, Linux arm64 and macOS arm64. A crate already in the tree through another
crate, with the same features, that becomes a direct dependency adds no code:
its row carries the figures it already has, or says "no change, already in
the tree through <crate>", in place of a new measurement. A crate is measured again when its major
version changes, or when a change to its features could plausibly move a
session's memory by 200 KiB or more, the noise floor below. A new dependency
in `Cargo.lock`, a subsystem or embedded data are reasons to measure; a
feature that exposes a few more functions over code already compiled in is
not. When the PR does not measure, it says in one line why the change is
under that floor, and the row stands. What a running session holds, broken down by Fiber's own
crates, belongs to the memory budget (`docs/performance.md`).

Dev-dependencies are compiled only into tests and jigs (`docs/testing.md`,
"Jigs"). They never reach the shipped binary, so they have no memory row.

## Runtime dependencies

Measured on September 25, 2026 with rustc 1.98.1. Linux is GitHub's
`ubuntu-24.04` and `ubuntu-24.04-arm` runners, and macOS is GitHub's macOS arm64 runner (`macos-26`), measured on
October 8, 2026 with rustc 1.99.0.
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
| clap_complete | the shell completion scripts `fiber completion` prints (`docs/invocation.md`, "Commands and flags") | 796 | 1,020 | 1,136 | 18 | 967 |
| thiserror | error types in library crates | ~0 | ~0 | ~0 | 6 | 325 |
| signal-hook | SIGTERM, SIGINT and SIGHUP | ~0 | ~0 | ~0 | 4 | 352 |
| ring | SHA-256, for PKCE, extension binary checksums, the content hash a repository's approvals pin, and an MCP tool's cut-short name; HMAC-SHA256, for `host.hmac_sha256`; credential fingerprints in the fake provider server | ~0 | ~0 | ~0 | 8 | 341 |
| base64 | PKCE, attachments sent to providers, images a client pastes, and the terminal's copy through OSC 52 (`docs/tui.md`, "Selection and copy") | ~0 | ~0 | ~0 | 1 | 328 |
| rustix | the shell tool's pseudo-terminal, new session and process group, and reading a key without echo; `host.exec`'s process groups; signalling MCP servers; killing a `model` or `credential` switch's credential command; signalling a Fiber delegate's process group; the terminal's clipboard read: its process group, its pipe's poll and waiting for its exit; and the test fakes' process and process-group probes | ~0 | ~0 | ~0 | 4 | 330 |
| libc | macOS only: the peak physical footprint for a `peak_memory` line (`docs/state.md`, "What each part holds") | n/a | n/a | ~0 | 1 | n/a |
| ignore, grep-searcher, grep-regex, grep-matcher | the search behind the shell's `grep` and `find` (`docs/tools.md`, "Search") and `session_search`'s scan (`docs/tools.md`, "Searching past sessions") | 2,656 | 2,480 | 1,904 | 25 | 2,886 |
| similar | an edit's diff in `details` (`docs/tools.md`, "edit"), the lines a `write` added and removed (`docs/tools.md`, "write"), an instruction file's diff (`docs/system-prompt.md`, "When something changes"), and a repository's changed code against its approved copy (`docs/extensions.md`, "Code a repository ships") | ~0 | 380 | ~0 | 1 | 389 |
| html5ever | `web_fetch`'s tokenizer, without its tree builder | 808 | 960 | 480 | 19 | 1,058 |
| encoding_rs | `web_fetch`'s decoding by the declared character set | 224 | 332 | 272 | 5 | 490 |
| pulldown-cmark | the terminal's markdown in replies (`docs/tui.md`, "Look") | 428 | 384 | ~0 | 4 | 724 |
| flate2 | decompressing the release's docs and extensions archives (`docs/releasing.md`, "Installing"), and the test fakes' release archives | 560 | 640 | 592 | 6 | 403 |
| jiff | the terminal's local time of day under a prompt bubble and on steering (`docs/tui.md`, "Turns"), and a standing rule's `added` time in `/rules`, with daylight saving and `TZ`, which `std` lacks | 536 | 640 | 384 | 2 | 692 |
| all of the above together | | 8,896 | 8,184 | 5,201 | 155 | 8,165 |
| image, fast_image_resize | the image child; png, jpeg, gif and webp only (`docs/model-routing.md`, "Image limits") | 68,076 | 67,604 | 67,825 | 32 | 5,234 |

Notes:

- ureq's figure is one live HTTPS request to example.com, through the OS
  trust store and the custom connector that keeps the socket for
  cancellation (`docs/architecture.md`, "Cancellation"). ring, which rustls
  uses for cryptography, carries C and assembly. aws-lc-rs, the alternative
  provider, adds 6 crates and 663 KiB for nothing Fiber needs.
- mlua carries Lua's C source.
- ratatui's figure is its two 200 by 50 screen buffers. Any full-screen
  terminal UI holds a screen model of that size.
- thiserror, ring, rustix and libc are already in the tree
  through other crates (rustls, crossterm, mlua), so listing them directly
  adds no crate. libc is a direct dependency on macOS only; Linux reads
  `/proc/self/status` instead.
- Random ids come from the standard library's `RandomState`, which the
  operating system seeds, so no crate mints them. The one secret random
  value, the PKCE verifier, comes from ring.
- serde_json's `preserve_order` feature is never enabled
  (`docs/prompt-cache.md`).
- The search row is ripgrep's walker (`ignore`), its search loop
  (`grep-searcher`), its regex adapter (`grep-regex`, which brings
  `regex`) and the matcher trait that gives each match's span for `-o`
  (`grep-matcher`, already in the tree through the other two), measured together walking the probe's own tree and searching every
  file. Alone, `regex` measured 1,780 KiB and `ignore` 1,392 KiB on Linux
  x86_64; about 440 KiB of `regex`'s binary is Unicode tables. The search and
  similar rows were measured on September 26, 2026.
- The html5ever and encoding_rs rows were measured on macOS on
  October 5, 2026, and on Linux on October 6, 2026.
- html5ever's figure is a generated 64 KiB page through the tokenizer alone,
  counting tokens in the sink, as the converter does without a tree.
- encoding_rs's figure is decoding a 64 KiB windows-1252 page by its declared
  character set.
- pulldown-cmark's figure is parsing a 2 KiB reply of headings, emphasis,
  lists and code blocks.
- flate2 is already in the tree through `png`, with the same `rust_backend`
  (`miniz_oxide`, pure Rust), so listing it directly adds no crate. No other
  backend is enabled. Its figure is gzipping a 4 MiB ustar-like stream in
  process, a block at a time, then streaming it from memory through the
  decoder; the compressing side is the fixtures', not Fiber's. It was measured
  on macOS on October 7, 2026, with rustc 1.99.0, where its stripped binary is
  397 KiB against the empty program's 315 KiB. The together row was measured
  before flate2 was listed.
- clap_complete's figure is generating the bash, zsh and fish scripts for a
  command shaped like the clap row's. It includes clap: on macOS it is
  1,040 KiB over the clap row from the same run, and its binary 179 KiB
  larger. Only `fiber completion` generates a script; a session never does.
  The clap_complete and together rows were measured on macOS on October 7,
  2026, with rustc 1.99.0. In that run the together binary with
  clap_complete peaked within noise of the one without it (peak footprints
  of 6,048 and 6,064 KiB against 5,760 and 6,128 KiB in two rounds),
  because its peak is the busiest other workload.
- jiff is built with default features off and only `std`, `tz-system` and `tzdb-zoneinfo`: it reads `/etc/localtime`, `TZ` and the system's zoneinfo database, and bundles no database. `time`, already in the tree through ratatui-widgets, is not used: its local-offset lookup does not work in a multithreaded process. jiff's figure is reading the system zone and formatting 1,000 times of day. The jiff and together rows were measured on October 8, 2026, on the runners above (run 37809842530); the together row was measured again with jiff linked, so its other crates' figures moved with the run.
- `web_fetch` converts a page with html5ever's tokenizer feeding Fiber's own
  single-pass writer (`crates/tools/src/web_fetch/markdown.rs`), not with a
  parser that builds the page's document tree. The smallest maintained crate
  that converts HTML to markdown on its own, htmd 0.5.5, passes `cargo deny`
  with this repository's `deny.toml` but pulls 30 crates (html5ever,
  markup5ever_rcdom, xml5ever, string_cache, phf and their dependencies) and
  builds a DOM of the page. It is the tree that costs: htmd's tree
  (markup5ever_rcdom) is what cost 232 MB. Peak memory footprint on macOS
  arm64, from `/usr/bin/time -l` on a generated page, against the 24 MiB busy
  session budget (`docs/performance.md`):

  | Page | htmd | Fiber's converter |
  |---|---|---|
  | 1 MiB | 24.9 MB | 3.5 MB |
  | 10 MiB (the download cap) | 232.2 MB | 23.0 MB |

  Fiber's converter figures are html5ever's tokenizer feeding the writer,
  with the page held in memory, measured on October 5, 2026.
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

### Proxies

Every HTTP request Fiber makes goes through a proxy when the environment names
one, as curl does: model calls, `host.http`, an extension's prepare step and
`web_fetch`. Fiber reads the variables ureq reads, in this order, and uses the
first one set: `ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`, each in upper or
lower case. `NO_PROXY` (or `no_proxy`) lists the hosts that bypass it, as exact
names, `*.` or `.` suffixes, or `*` for every host. There is no configuration
key: these variables are the platform's, not Fiber's.

The proxy is an HTTP proxy, reached with `CONNECT`, so TLS runs end to end
through it and the certificate rules above still apply. A `socks5://` proxy is
not supported, because ureq's SOCKS support is not compiled in. An
extension's git fetch runs the system `git`, which reads the same variables
itself.

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
  (`docs/errors.md`). Creating and removing the git worktrees sessions run in
  runs it too (`docs/architecture.md`).

`grep` and `find` in the shell tool run Fiber's own search, not a system
program (`docs/tools.md`, "Search").

## Waiting on other decisions

These crates are the choice if the named decision needs one. Each is measured
already.

| Crate | Needed if | Linux x86_64 | Linux arm64 | macOS arm64 | Crates | Binary |
|---|---|---:|---:|---:|---:|---:|
| rusqlite, SQLite bundled | Fiber keeps a derived database, such as an index for session search; today listing and searching sessions read the logs (`docs/state.md`, `docs/tools.md` "Searching past sessions") | 2,236 | 1,984 | ~0 | 14 | 2,268 |

rusqlite carries SQLite's C source.

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
| Syntax highlighting in the terminal | see below |

Tool arguments are checked against a subset of JSON Schema: `type`,
`properties`, `required`, `additionalProperties`, `enum`, `items`, `minimum`,
`maximum`, `minLength`, `maxLength`, `minItems`, `maxItems`, `anyOf` and
`$ref`. A `$ref` is followed only within
the same schema, such as `#/$defs/name` or `#/definitions/name`; one that
points elsewhere, or leads back to itself, fails the check with a message
saying so. A built-in tool's schema uses only the
subset, and a test fails if one does not. In an extension's or an MCP
server's schema, a keyword outside the subset is skipped, not failed, so a
server whose schema uses one still works. Checking those schemas is best
effort. The jsonschema
crate covers the whole specification, but it costs 14,808 KiB on Linux x86_64
and brings 79 crates, more than every runtime crate together.

The terminal highlights code blocks with its own lexer
(`crates/tui/src/highlight.rs`): each language's keywords, strings, comments,
numbers, types, constants, function names and operators, from a table per
language. The obvious crate, syntect,
costs 8,704 KiB on Linux x86_64 just to load its syntax definitions. It has 44
crates, and cargo-deny fails it out of the box on two unmaintained crates,
yaml-rust and bincode. arborium 2.18, tree-sitter grammars behind a feature
per language, passes cargo-deny with 33 crates, but the grammars for the 18
languages the terminal highlights add 25,500 KiB to the stripped binary on
macOS arm64, about 9,500 KiB of it SQL's and 5,300 KiB C++'s. That alone is
over the 20 MiB cap, on a binary already 12.3 MiB. Highlighting one block in
each of three languages peaks 6,832 KiB over an empty program (macOS arm64
footprint, October 6, 2026). tree-sitter-highlight with the same grammar
crates compiles the same parse tables.

The lexer's tables are static data, so a reply with no code loads nothing for
it. Through the `draw` jig, a reply with one code block in each of Rust,
Python and JavaScript draws in 1.9 ms and peaks at 1,632 KiB of footprint,
against 1.6 ms and 1,344 KiB for the same reply without the blocks: the whole
process, macOS arm64, October 6, 2026.

The shell tool's recogniser splits a command on `&&`, `||`, `;` and `|` and
reads each part as plain words. Anything it cannot read plainly makes the
call declare `executes`, as `docs/tools.md` requires. A recogniser that fails
closed does not need a full parser.

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
| zsh, fish | tool | the completion tests load `fiber completion`'s scripts in each shell (`docs/testing.md`, "Running tests"); bash is on every runner already, and macOS ships zsh. CI installs zsh and fish on Linux and fish on macOS |
| dash, shellcheck | tool | the install-script test runs `install.sh` under `/bin/dash` (macOS ships it; it is Ubuntu's `/bin/sh`); `scripts/check` runs shellcheck on Linux, and GitHub's `ubuntu-24.04` image ships it |
| xtask | tool | the workspace's own CI helper, `cargo xtask`: selection, the `CI` verdict and the gate's checks (`docs/ci.md`). It uses serde_json, proc-macro2 and pulldown-cmark, `fakes` in its tests, and no Fiber crate depends on it |
| proc-macro2 | xtask dependency | tokenising Rust source for the `unsafe` table check (`docs/code-quality.md`, "`unsafe`") |
| pulldown-cmark | xtask dependency | reading Markdown for the docs check (`docs/ci.md`, "The docs check"); the terminal's use is in the runtime table |

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
  condition for removing the exception.

## Toolchain

Fiber has no separate minimum supported Rust version. It is a binary, not a
library, so the toolchain CI pins is the only version Fiber supports.

Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md#from-docsdependenciesmd).
