# macOS CI file readback diagnosis

The failures in [CI run 43748474387](https://github.com/aakshintala/fiber/actions/runs/43748474387), job `Tests (macos-26)`, point to transient I/O failures on the runner's temporary filesystem. They do not establish an APFS defect or identify a storage component.

## Direct evidence from the job log

- The first-attempt failure cluster spans 19:05:45Z–19:06:08Z. The run reports 88 tests failing both nextest tries in that interval; the retry leg later passed them.
- Several tests report malformed or empty JSON while reading staged `providers/*.json` files. The reported parse location is line 1, column 1.
- At the same time, unrelated filesystem operations fail with `Input/output error (os error 5)`: writes to extension commit-journal files, Git clone `fsync` of a pack index, and reads from test-local credential files. The errors concern separate temporary roots and different operations, not only provider JSON parsing.
- The job's preflight reports 87,802 MiB free. Its log has no `No space left` or `os error 28` entry.
- The failures are clustered in time. Nearby macOS jobs for the runs named in [issue #4934](https://github.com/aakshintala/fiber/issues/4934) do not show the same malformed-provider parse errors.

The error text is evidence of I/O failures reported by the operating system on the runner. It does not show why the filesystem returned those errors, and does not identify the underlying storage implementation or hardware.

## Source sequence

- In `crates/main/tests/support/mod.rs`, `Setup::provider` creates provider JSON with `write_json` in its test's own temporary root, then calls `install_with_retry`. The helper calls `extensions::plan` and `Plan::commit` synchronously. Each `Setup` owns its own temporary root, so these tests do not intentionally share the source file.
- In `crates/extensions/src/install.rs`, `place` creates a distinct staging directory, calls `copy`, writes the install record, and only then calls `config::read_providers` on that staged directory. `copy` recursively copies regular files with `std::fs::copy` and returns only after the directory traversal completes.
- In `crates/extensions/src/manage.rs`, `Plan::commit` delegates staged swaps to `commit_all`; this occurs after planning and staging.

The source order provides no read-before-copy window in this path: the provider is first written by the test, copied into staging, and parsed only after the copy returns. A filesystem I/O failure during that sequence can still leave a bad or unreadable staged copy, which is consistent with the simultaneous independent I/O errors in the job.

## Diagnosis and limits

Taken together, the independent `os error 5` failures on writes, reads and copy-related work, their concentration in the same short interval, and the synchronous copy-before-parse source sequence point to a transient runner temporary-filesystem I/O fault—not a Fiber read-before-copy ordering defect. The available log cannot identify the underlying storage component or a more specific physical cause. No retry, deadline change or speculative Fiber behavior change is justified by this evidence.

## Sources

- [Tests (macos-26) job log](https://github.com/aakshintala/fiber/actions/runs/43748474387)
- `crates/main/tests/support/mod.rs` (`Setup::provider`, `install_with_retry`)
- `crates/extensions/src/install.rs` (`place`, `copy`)
- `crates/extensions/src/manage.rs` (`Plan::commit`, `commit_run`)
- [Issue #4934: evidence and diagnosis requested](https://github.com/aakshintala/fiber/issues/4934)
