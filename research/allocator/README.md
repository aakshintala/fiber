# Allocator comparison (issue #1990)

Whether another allocator lowers Fiber's resident memory is for the memory
budget to measure (`docs/dependencies.md`, "Measuring memory"). This
directory holds that one measurement: the attach timing, the attach peak
and both retained rows (`docs/performance.md`, "Budgets") on three builds
of the same commit, five samples each, on the same Linux x86_64 runner,
plus a fourth result rerunning the mimalloc binary with
`MIMALLOC_PURGE_DELAY=0`:

1. the musl release binary with its system allocator, the target that
   ships (`docs/releasing.md`)
2. the same musl build with mimalloc as the global allocator, patched in
   inside the job only and never committed
3. an `x86_64-unknown-linux-gnu` release build, which uses glibc's
   allocator
4. the musl-mimalloc binary of (2) through a shell wrapper exporting
   `MIMALLOC_PURGE_DELAY=0` and forwarding every argument unchanged, with
   no new build

The manual workflow `.github/workflows/allocator-compare.yml` builds the
three binaries, runs the full benchmark harness five times on each build
and a fourth time on the mimalloc binary through the wrapper, and uploads
the four result files with the commit, the runner and the resolved
mimalloc version. The full harness runs so the attach rows keep their
paging-jig stages; the comparison reads only the five rows below.
It never runs on a push or a pull request: `workflow_dispatch` needs the
file on `main`, so the first numbers arrive after this ticket merges.

Component-level allocator numbers, per crate rather than whole-Fiber, are
in `research/concurrency/README.md`.

## Results

Run [38139307008](https://github.com/aakshintala/fiber/actions/runs/38139307008)
on commit `a8ae8a1d`, runner `ubuntu-24.04`, mimalloc v0.1.52: the median
of five samples per binary.

| Row | musl system | musl + mimalloc | gnu (glibc) |
|---|---|---|---|
| Terminal to its first frame, attaching, 10 MiB (ms) | 944.6 | 265.5 | 311.9 |
| Terminal, attaching a 10 MiB session, peak RSS (KiB) | 41240 | 75836 | 44476 |
| Terminal, memory kept after attaching a 10 MiB session, RssAnon (KiB) | 5796 | 62316 | 32664 |
| Session, memory kept after the busy turn, RssAnon (KiB) | 8128 | 34516 | 6712 |

Changing the shipped allocator is an owner question that follows from
these numbers, not part of #1990.
