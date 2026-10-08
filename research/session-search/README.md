# Session search: what one scan costs

Evidence for [#578](https://github.com/aakshintala/fiber/issues/578), the
`session_search` tool (`docs/tools.md`, "Searching past sessions"). First
run 2026-10-07 on macOS arm64.

## The question

`session_search` keeps no index: every call scans the session logs and text
artifacts of the project, through `log::SessionScan::scan`. How long does one
scan take as a project's logs grow, with a warm page cache and, on Linux, with
a cold one?

## Method

A Rust program under this directory, built against Fiber's own `log` and
`contract` crates, does two things:

- `gen` writes a synthetic Fiber home: one project of sessions whose logs are
  real Fiber envelopes (`session_started`, a `turn_started`, then shell
  `tool_call_requested` and `tool_call_completed` lines, with a
  `text_completed` every fifth call), until the logs reach a target size.
- `scan` runs one scan of that project with the tool's defaults (own project,
  limit 20) and prints its time, the hits found and the problems. Each run is
  its own process, so a cold run starts with nothing of the corpus in memory.

`run.sh` builds it, generates each corpus once, and for each corpus and query
records the median of 5 runs (`docs/performance.md`, "Measuring"):

- Warm: one untimed scan first, then 5 timed scans.
- Cold, on Linux only: before each run every file of the corpus is evicted
  with `posix_fadvise(DONTNEED)` after `sync`, which needs no root and works in
  a container. `residency.py` then checks with `mincore` that no page of the
  corpus is still in the page cache. A sample whose eviction is not confirmed
  is not scanned; the script retries the eviction up to 3 times and then
  stops. A confirmed cold row reads `confirmed` in `resident_bytes`. macOS has
  no unprivileged way to drop one file from the cache, so it has warm rows only.

Each row of `results.tsv` names its date and platform. The script appends;
it never rewrites earlier rows.

## Where each number came from

- Session log sizes are drawn from a log-normal through the median (326 KiB)
  and 90th percentile (1,749 KiB) of the owner's pooled Claude Code and pi
  session files, cut at their maximum (15,313 KiB). Those figures are from
  [research/session-listing](../session-listing/README.md), "Where each number
  came from".
- Corpus sizes are swept: 300, 1,300 and 4,000 MiB of logs. 1,300 MiB is the
  size of the owner's pi and Claude Code logs that `docs/tools.md` cites.
- Queries: `retry budget`, which about one tool output in 200 holds (many
  hits), and `zqxv no such text`, which nothing holds (no hits).
- Artifacts: no measurement gives the share of tool outputs saved whole as
  artifacts, so it is swept rather than fixed: one output in 25 saved as a
  64 KiB text artifact, and none. Both values are chosen, not measured; the
  real share sits somewhere between them or beyond, and the rows show how
  much it moves the answer.

## Results: macOS arm64, warm

Apple M3 Pro, 11 cores, internal SSD (APFS), 2026-10-07. The scan runs on one
thread.

| Logs | Artifacts | Sessions | Query | Hits | Median |
|---|---|---|---|---|---|
| 304 MiB | 187 MiB | 418 | many hits | 1,096 | 0.90 s |
| 304 MiB | 187 MiB | 418 | no hits | 0 | 0.64 s |
| 1,308 MiB | 808 MiB | 1,795 | many hits | 4,596 | 5.89 s |
| 1,308 MiB | 808 MiB | 1,795 | no hits | 0 | 4.79 s |
| 4,011 MiB | 2,476 MiB | 5,542 | many hits | 14,389 | 21.2 s |
| 4,011 MiB | 2,476 MiB | 5,542 | no hits | 0 | 20.4 s |
| 1,304 MiB | none | 1,753 | many hits | 1,743 | 2.39 s |
| 1,304 MiB | none | 1,753 | no hits | 0 | 2.46 s |

For scale, on the same machine and the same 1,300 MiB corpus with no
artifacts, single-threaded ripgrep (`rg -j1 -i -F -c`) took about 0.4 to
0.5 s for the no-hits query alone, and 0.7 to 1.3 s with the four literals
the scan's raw pass selects on (the query, the two line starts it reads the
session name from, and `session_search`, to skip the tool's own calls). With
the artifacts, single-threaded ripgrep over logs and artifacts took about
3.1 s. These were measured by hand, not by `run.sh`.

What the macOS rows show:

- The scan grows about linearly with what it reads, logs and artifacts
  together.
- Artifacts dominate when they are many small files: at one output in 25,
  the 1,300 MiB corpus took about twice as long as with none.
- An earlier estimate of 0.05 to 0.35 s for 1.3 GB came from multithreaded
  `rg -l -F`, which stops reading each file at its first match. It is not the
  scan's cost: one warm scan of 1,300 MiB of logs alone took about 2.4 s here,
  and `docs/tools.md` now states the measured figure.

## Results: Linux, cold and warm

GitHub's `ubuntu-24.04` runner (2026-10-07): Linux 6.17.0-1022-azure x86_64,
AMD EPYC 7763, 4 vCPUs, ext4 on `/dev/root`, one thread for the scan. A
temporary workflow ran `bash research/session-search/run.sh` there; its rows
are in `results.tsv` (platform `Linux x86_64`). Cold runs evict each file with
`dd iflag=nocache` before the scan. `fincore` is not installed on the runner,
so eviction is unconfirmed (`resident_bytes` reads `unconfirmed`); the cold
medians are 1.6 to 3.0 times the warm ones, which an unevicted cache would not
give. Medians of five runs, in seconds:

| Logs | Artifacts | Query | Warm | Cold |
|---|---|---|---|---|
| 304 MiB | 187 MiB | many hits | 1.49 | 3.79 |
| 304 MiB | 187 MiB | no hits | 1.09 | 3.30 |
| 1,308 MiB | 808 MiB | many hits | 6.43 | 16.2 |
| 1,308 MiB | 808 MiB | no hits | 4.66 | 14.0 |
| 4,011 MiB | 2,476 MiB | many hits | 19.8 | 49.2 |
| 4,011 MiB | 2,476 MiB | no hits | 14.4 | 44.0 |
| 1,304 MiB | none | many hits | 3.93 | 6.71 |
| 1,304 MiB | none | no hits | 4.27 | 6.95 |

A shared runner's timings vary more than a laptop's; read them as the order
of magnitude.

## Results: parallel scan, macOS arm64, warm

The scan reads sessions on parallel threads since
[#1282](https://github.com/aakshintala/fiber/issues/1282). Both runs below are
on the same Apple M3 Pro (11 cores) on 2026-10-07, back to back, against the
same corpora, while other builds ran on the machine (load average 19 at the
start of the first run, 11 at the start of the second). In `results.tsv` they
are the last 16 rows: the first 8 are commit `f06d6b52` (one thread), the last
8 are commit `62f05a6e` (parallel). Medians of five runs, in seconds:

| Logs | Artifacts | Query | One thread | Parallel |
|---|---|---|---|---|
| 304 MiB | 187 MiB | many hits | 1.04 | 0.15 |
| 304 MiB | 187 MiB | no hits | 0.70 | 0.11 |
| 1,308 MiB | 808 MiB | many hits | 4.42 | 1.09 |
| 1,308 MiB | 808 MiB | no hits | 2.88 | 1.00 |
| 4,011 MiB | 2,476 MiB | many hits | 22.2 | 3.08 |
| 4,011 MiB | 2,476 MiB | no hits | 25.7 | 2.65 |
| 1,304 MiB | none | many hits | 2.20 | 0.35 |
| 1,304 MiB | none | no hits | 2.40 | 0.38 |

The load spread the one-thread runs more than the parallel ones (the
1,308 MiB many-hits runs range from 4.0 to 9.3 s). One scan of the 1,304 MiB
corpus with no artifacts, `retry budget`, had a peak memory footprint
(`/usr/bin/time -l`) of 4.7 MiB on one thread and 11.0 MiB in parallel.

## Results: Linux x86_64, parallel scan

GitHub's `ubuntu-24.04` runner (2026-10-08): x86_64, 4 vCPUs (`nproc`
4), parallel scan, run 37742699357. Its 16 rows are the last 16 in
`results.tsv` (platform `Linux x86_64`, date 2026-10-08). Medians of five
runs, in seconds; peak RSS is the scan process's peak in MiB, measured with
GNU time. Earlier rows have no RSS.

| Logs | Artifacts | Query | Warm | Cold | Peak RSS |
|---|---|---|---|---|---|
| 304 MiB | 187 MiB | many hits | 0.58 | 1.20 | 8.2 |
| 304 MiB | 187 MiB | no hits | 0.43 | 1.11 | 6.8 |
| 1,308 MiB | 808 MiB | many hits | 2.44 | 5.16 | 9.1 |
| 1,308 MiB | 808 MiB | no hits | 1.83 | 5.15 | 7.5 |
| 4,011 MiB | 2,476 MiB | many hits | 7.50 | 16.2 | 10.5 |
| 4,011 MiB | 2,476 MiB | no hits | 5.62 | 16.2 | 8.8 |
| 1,304 MiB | none | many hits | 1.54 | 3.05 | 7.6 |
| 1,304 MiB | none | no hits | 1.68 | 3.06 | 7.5 |

Cold is unconfirmed: `fincore` is not on the runner, so nothing checks
that eviction cleared the cache. The confirmed rerun, with `mincore`
checking eviction, is in the next section.

## Results: Linux, cold confirmed

Probe run 37747174815 (2026-10-08, head `a20c051f`), `ubuntu-24.04` and
`ubuntu-24.04-arm`: every cold sample was confirmed before it was scanned, so
no cold median includes an unevicted sample. Its 32 Linux rows are in
`results.tsv` (date 2026-10-08, platforms `Linux x86_64` and `Linux aarch64`).
Medians of five runs, in seconds. The macOS leg of this run gave warm rows
only; its 8 rows end the file.

| Logs | Artifacts | Query | x86_64 warm | x86_64 cold | arm64 warm | arm64 cold |
|---|---|---|---|---|---|---|
| 304 MiB | 187 MiB | many hits | 0.34 | 1.07 | 0.36 | 1.10 |
| 304 MiB | 187 MiB | no hits | 0.25 | 1.08 | 0.26 | 1.09 |
| 1,308 MiB | 808 MiB | many hits | 1.45 | 5.14 | 1.52 | 5.13 |
| 1,308 MiB | 808 MiB | no hits | 1.07 | 5.13 | 1.11 | 5.12 |
| 4,011 MiB | 2,476 MiB | many hits | 4.54 | 16.2 | 4.58 | 16.2 |
| 4,011 MiB | 2,476 MiB | no hits | 3.26 | 16.2 | 3.37 | 16.2 |
| 1,304 MiB | none | many hits | 0.91 | 3.07 | 0.90 | 3.09 |
| 1,304 MiB | none | no hits | 0.97 | 3.07 | 0.97 | 3.09 |

Cold medians are 3.0 to 5.0 times the warm ones on both Linux runners.

## Reproducing

```sh
bash research/session-search/run.sh
```

The corpora go under `$CORPUS` (default `$TMPDIR/fiber-session-search`) and
are kept between runs; they take about 9 GB. Delete the directory when done.
