# Allocator comparison (issue #1990)

Whether another allocator lowers Fiber's resident memory is for the memory
budget to measure (`docs/dependencies.md`, "Measuring memory"). This
directory holds that one measurement: the attach timing, the attach peak
and both retained rows (`docs/performance.md`, "Budgets") on three builds
of the same commit, five samples each, on the same Linux x86_64 runner:

1. the musl release binary with its system allocator, the target that
   ships (`docs/releasing.md`)
2. the same musl build with mimalloc as the global allocator, patched in
   inside the job only and never committed
3. an `x86_64-unknown-linux-gnu` release build, which uses glibc's
   allocator

The manual workflow `.github/workflows/allocator-compare.yml` builds the
three binaries, runs the full benchmark harness five times on each, and
uploads the three result files with the commit, the runner and the
resolved mimalloc version. The full harness runs so the attach rows keep
their paging-jig stages; the comparison reads only the five rows below.
It never runs on a push or a pull request: `workflow_dispatch` needs the
file on `main`, so the first numbers arrive after this ticket merges.

Component-level allocator numbers, per crate rather than whole-Fiber, are
in `research/concurrency/README.md`.

## Results

Pending the first run after merge. The run fills this table with the
median of its five samples per binary, and names the commit and runner.

| Row | musl system | musl + mimalloc | gnu (glibc) |
|---|---|---|---|
| Terminal to its first frame, attaching, 10 MiB (ms) | pending | pending | pending |
| Terminal, attaching a 10 MiB session, peak RSS (KiB) | pending | pending | pending |
| Terminal, memory kept after attaching a 10 MiB session, RssAnon (KiB) | pending | pending | pending |
| Session, memory kept after the busy turn, RssAnon (KiB) | pending | pending | pending |

Changing the shipped allocator is an owner question that follows from
these numbers, not part of #1990.
