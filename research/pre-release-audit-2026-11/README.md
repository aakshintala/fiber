# Pre-release audit, November 2026: raw data

Raw data behind the pre-release audits and the review-prompt trial. The
findings themselves are on GitHub: #3655 (performance), #4270 (architecture
and cleanup), and aakshintala/switchyard#1993 (behaviour-review trial). This
folder is what those comments were computed from, kept so the numbers can be
checked or recomputed. It was produced by AI agents and not reviewed by a
person.

Nothing here is a plan or a rule. Paths inside the files that start with
`/tmp/claude-0/` point at the session that produced them and no longer exist.

## `audit/`

- `findings.json`: all 460 findings of the architecture and cleanup audit,
  one object each: `unit` (the slice), `kind` (`code`, `tests` or `cross`),
  `title`, `cat`, `sev`, `raw` (the finder's removable-line estimate),
  `verdict` (`confirmed` or `partly`), `cor` (the verifier's corrected
  estimate), `reason` (the verifier's note), `anchors`, `evidence`, `prop`
  (proposed change).
- `dup.py`: the script behind the "lines in verbatim-repeated 8-line blocks"
  figures.

## `bugs/`

`b1.json` to `b5.json`: 115 issues labelled `bug`, classified one object
each. `b1` to `b4` are the 90 most recent at the time; `b5` is 25 older ones
(`"cohort": "older"`). Keys: `is_product_bug`, `not_product_kind`,
`found_by`, `area`, `defect_kind`, `doc_promised`, `test_existed_nearby`,
`why_missed`, `fix_test_level`, `severity`, `note`. One agent classified each
issue from its body, comments and fix; nobody double-checked the labels.

## `reviews/`

- `prs.json`: the last 49 merged pull requests at the time: hours open,
  commits, lines, review findings, workflow runs, commit subjects.
  `ci_failure_causes` is a keyword match on commit subjects, not CI logs.
- `review_findings.json`: 268 first-round review findings on those pull
  requests, with `axis`, `severity`, `kind`, `outcome`, `text`. `outcome` is
  the weakest field: "fixed" was inferred where a later verify round did not
  list the finding as open.

## `trial/`

The blind trial of the behaviour-review prompt.

- `pairs.json`, `holdout.json`: the pull requests reviewed, with the escaped
  bug each one introduced (`bug`, `defect`), `base_sha` and `head_sha`.
  The #4329 pair in `holdout.json` was dropped from scoring because its bug
  was already named in the pull request.
- `REVIEWER.md`, `REVIEWER2.md`: the briefs the blind reviewers were given
  in rounds one and two. `VERIFY.md`: the verifiers' brief.
- `results/wt<PR>/RESULT.md`: the round-one review of that pull request
  (first draft of the skill). `RESULT2.md`: the round-two review (revised
  draft). `VERIFY.json`, `VERIFY2.json`: the verifier's row for each finding:
  `real_at_merge`, `on_main_now`, `tracked`, `value`, `evidence`.

## `perf-scenarios/`

What the process-tree measurements in #3655 were run with.

- `*.json`: scripts for the `scripted` provider. `s1_N` is N delegates each
  running a tool, `s3_N` one session with N background jobs, `s4` nested
  delegates three by three, `stream` a 1,500-chunk reply at one chunk every
  20 ms, `one` a single text reply, `rev` the reviewer's replies.
- `home-config.json`, `home-rules`: the `FIBER_HOME` config and standing
  rules the runs used.
- `row.py`: one line per scenario for a process tree (processes, RSS, PSS,
  private memory, threads, wakeups and CPU over 10 seconds).
  `treesample.py`, `sample.py`, `wake.py`: the same measures in other forms.
- `term.py`: opens N real terminals in pseudo-terminals, sends a prompt and
  samples every `fiber` process by role.

The numbers in #3655 came from a local glibc release build in a 4-CPU
container, one run each. They are not medians and not the musl build.
