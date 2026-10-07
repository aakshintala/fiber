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
  from the page cache with GNU `dd if=<file> iflag=nocache count=0`, which
  needs no root and works in a container. When `fincore` is installed, the
  bytes still resident after eviction are recorded in `resident_bytes`, so a
  row shows whether eviction worked. macOS has no unprivileged way to drop one
  file from the cache, so it has warm rows only.

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
- The warm figure in `docs/tools.md` (0.05 to 0.35 s for 1.3 GB) is
  multithreaded `rg -l -F`, which stops reading each file at its first match.
  It is not the scan's cost: one warm scan of 1,300 MiB of logs alone took
  about 2.4 s here.

## Results: Linux, cold and warm

Not yet run. A Linux machine runs `bash research/session-search/run.sh` and
records here: `uname -a`, the CPU, the file system, whether `fincore`
confirmed eviction, and the rows it appended to `results.tsv`. Until then
there are no Linux figures, and `docs/tools.md` cites none.

## Reproducing

```sh
bash research/session-search/run.sh
```

The corpora go under `$CORPUS` (default `$TMPDIR/fiber-session-search`) and
are kept between runs; they take about 9 GB. Delete the directory when done.
