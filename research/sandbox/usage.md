# Sandbox confinement: how often would shell calls break? (ticket #30)

Measured 2026-09-26, one pass per corpus, on this machine's real session logs. Confinement modeled: write access to the session's cwd and a temp dir, no network, no writes anywhere else.


## Heuristic limits (read before trusting the percentages)

- Classification runs on the RAW command STRING, not on what the process
  actually did. `git commit --dry-run` and `git commit` classify the same.
- A pipeline/chain (`&&`, `||`, `;`, `|`, and a bare newline -- most Bash
  calls in these logs are multi-line scripts) is classified as a whole: if
  ANY segment trips a class, the whole command counts for that class,
  because a `&&` chain stops at the first blocked segment. A segment's
  "first tool+subcommand" is used for the top-15 label even when later
  segments also tripped other classes. Splitting on newline is required --
  without it, a 3-statement script (`mkdir x`, newline, `strings ~/foo >
  x/out`, newline, `wc -l x/out`) reads as ONE segment starting with
  `mkdir`, and every later token -- including `~/foo`, which is only ever
  READ by `strings` -- gets scanned as one of `mkdir`'s write targets
  (caught in spot-check, fixed by adding `
` as a split point). The same
  splitting is applied inside heredoc bodies, since there is no heredoc-
  aware parsing; a body line that happens to contain `&&`/`;`/`|`/a newline
  is treated the same as real shell syntax (usually harmless: heredoc body
  text rarely starts a line with a recognized tool name).
- `WRITE_OUTSIDE_TOOL` is presence-based: any invocation of cargo, npm, pip,
  go, brew, gh, claude, codex, pi, or a listed editor is flagged, regardless
  of subcommand -- e.g. `npm --version` counts the same as `npm install`.
  This over-counts tools whose subcommand truly touches nothing outside cwd,
  and under-counts nothing (conservative for "would this ever touch
  outside state").
- `pi`/`code` as a bare first word can collide with unrelated shell tokens
  (a loop variable, a project called `code`); not filtered out.
- `write_outside_explicit`/`write_tmp` require an actual write signal, not
  just a path mention: file redirection (`>`, `>>`, `&>`, excluding fd-dup
  forms like `2>&1`), a long-form `--output`/`--output=` flag, or a first
  word from a small write-verb list (`cp mv rm mkdir touch ln chmod chown
  tee dd rsync tar unzip zip patch`; `sed`/`perl` only count with `-i`).
  Short `-o`/`-O` is deliberately NOT treated as "output file" -- spot-check
  caught `lldb -b -o run -o "bt 30" -o quit` misread as writing to files
  named `run` and `quit` (lldb's `-o` means "run this debugger command"),
  resolved into the wrong directory and wrongly flagged outside workspace.
  Short `-o` is too overloaded across tools (`grep -o`, `sort -o`, `ssh -o`,
  `tar -o`, `ls -o`, ...) to trust; this drops a few genuine cases (e.g.
  `curl -o file`) but curl/wget are already unconditionally in the network
  class regardless. Everything else
  -- `cd`, `ls`, `cat`, `grep`, `find`, `git log`, a script run with no
  output flag -- is read-only under this heuristic, even though an inline
  python/node/bun/zig script *could* write anywhere; the log has no
  visibility into a script's own body, so such writes are invisible here
  (an undercount, most likely benign since sampled inline scripts mostly
  edit relative, in-workspace paths).
- A leading `cd <path>` (or `cd` with no args, or mid-chain via `&&`) moves
  a per-command "effective directory" used to resolve relative write
  targets and to pick which repo a later `git` segment sees; the sandbox
  boundary itself (`workspace_root`) never moves. `cd -` (previous dir) is
  untrackable without real shell state and is treated as a no-op.
  Quoting/escaping edge cases in path tokens are not specially handled.
  "Outside" is plain string-prefix against the session's recorded cwd, not
  symlink-resolved. `/dev/null` is a standing exception (never "outside").
- `/tmp` and `$TMPDIR` detection matches literal `/tmp/`, `/private/tmp/`,
  `/var/folders/*/*/T/` and the `$TMPDIR` token; it does not resolve the
  variable's actual value from the environment (unknown at log-read time).
- Git-worktree detection (class 3) reads `<cwd>/.git` on THIS machine, now.
  A session whose cwd no longer exists, or whose worktree was since removed
  or converted, is silently treated as "not a worktree" (undercounts class
  3). Cached once per distinct cwd string.
- "Network, maybe" (cargo) is reported separately and NOT included in the
  strict "would break" denominator, because whether it actually reaches the
  network depends on whether `~/.cargo/registry` already has the crate --
  information the log doesn't carry. A second, lower-bound "unharmed"
  number treats it as a break. A `--offline` flag (spot-checked: present on
  11/53 of the sampled network-maybe cargo commands) genuinely rules out
  network but is not detected, so "maybe" over-counts by that much.
- File-redirect detection (`>`) has no real shell tokenizer, so a literal
  `>` INSIDE a quoted sed/awk/perl expression (e.g. `sed 's/.*x > y//'`,
  spotted during spot-check on `grep ... | sed 's/.*lifecycle > //'`) can
  misread as a redirect. Mitigated by dropping any candidate target that
  contains a stray quote character (the common shape of this false
  positive), which is not a complete fix for every quoting shape.
- `cp`/`mv`/`ln` treat only the LAST non-flag argument as the write target
  (the destination); earlier arguments are sources, which may legitimately
  sit outside cwd while the write itself stays inside (`cp ~/x ./y` only
  writes inside). Other multi-arg write verbs (`mkdir`, `rm`, `touch`,
  `chmod`, `chown`, `tar`, ...) treat every non-flag argument as a
  candidate write target -- correct for `mkdir a b`, over-broad for
  something like `chmod 700 path` (the mode "700" is also checked as a
  candidate path but never matches "outside cwd" since it doesn't start
  with `/` or `~`, so it's harmless in practice).
- pi's session cwd is read once from the session-header line and applied to
  every shell call in that session (pi sessions don't record a per-call
  cwd); Claude Code carries cwd on every line, used per call.


## pi

session files: 689, sessions with >=1 shell call: 598
total shell calls: 27818

| class | calls | % of calls | sessions | % of sessions-with-shell |
|---|---|---|---|---|
| 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get) | 3467 | 12.5% | 349 | 58.4% |
| 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached) | 0 | 0.0% | 0 | 0.0% |
| 2a. Writes outside workspace: explicit path outside cwd (non-temp) | 288 | 1.0% | 98 | 16.4% |
| 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break) | 2016 | 7.2% | 235 | 39.3% |
| 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor) | 4411 | 15.9% | 390 | 65.2% |
| git write subcommand (any) -- only a break if cwd is a git worktree (3.) | 1129 | 4.1% | 274 | 45.8% |
| 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace) | 0 | 0.0% | 0 | 0.0% |

**Would pass unharmed under 'workspace + temp write, no network' (strict: excludes network, write_outside_explicit, write_outside_tool, git_write_worktree; does NOT count cargo's maybe-network as a break):** 22655/27818 (81.4%)
**Same, but also treating cargo build/test/run/install/add/update as a break (lower bound):** 22655/27818 (81.4%)

top 15 commands, 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get):
  - gh pr: 1297
  - gh issue: 568
  - gh api: 487
  - gh run: 442
  - git fetch: 284
  - git push: 248
  - curl: 51
  - bun install: 30
  - git pull: 22
  - gh auth: 10
  - npm install: 7
  - gh label: 6
  - gh repo: 5
  - gh search: 4
  - gh variable: 2

top 15 commands, 2a. Writes outside workspace: explicit path outside cwd (non-temp):
  - cp: 73
  - rm: 50
  - sed: 49
  - cat: 23
  - mkdir: 17
  - echo: 8
  - chmod: 8
  - touch: 7
  - try: 5
  - ln: 4
  - awk: 4
  - grep: 4
  - assert: 4
  - if: 4
  - patch: 3

top 15 commands, 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break):
  - cat: 630
  - zig: 254
  - rm: 170
  - mkdir: 165
  - sed: 79
  - timeout: 75
  - node: 72
  - cp: 68
  - bun test: 67
  - chmod: 25
  - gh run: 25
  - sort: 24
  - npm test: 23
  - do: 20
  - gh pr: 19

top 15 commands, 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor):
  - gh pr: 1355
  - bun test: 881
  - gh issue: 580
  - gh api: 488
  - gh run: 454
  - bun run: 100
  - npm test: 67
  - pi: 59
  - bun install: 30
  - bun build: 29
  - npm run: 26
  - bun 2>&1: 18
  - codex: 15
  - bun ': 13
  - bun repro-min.ts: 12

top 15 commands, git write subcommand (any) -- only a break if cwd is a git worktree (3.):
  - git add: 353
  - git branch: 245
  - git worktree: 236
  - git checkout: 102
  - git stash: 91
  - git rebase: 33
  - git commit: 21
  - git rm: 13
  - git merge: 13
  - git apply: 6
  - git tag: 5
  - git mv: 5
  - git reset: 3
  - git switch: 1
  - git init: 1

## Claude Code (main)

session files: 239, sessions with >=1 shell call: 189
total shell calls: 14269

| class | calls | % of calls | sessions | % of sessions-with-shell |
|---|---|---|---|---|
| 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get) | 3354 | 23.5% | 139 | 73.5% |
| 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached) | 79 | 0.6% | 12 | 6.3% |
| 2a. Writes outside workspace: explicit path outside cwd (non-temp) | 242 | 1.7% | 83 | 43.9% |
| 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break) | 1039 | 7.3% | 128 | 67.7% |
| 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor) | 3463 | 24.3% | 142 | 75.1% |
| git write subcommand (any) -- only a break if cwd is a git worktree (3.) | 1176 | 8.2% | 127 | 67.2% |
| 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace) | 3 | 0.0% | 2 | 1.1% |

**Would pass unharmed under 'workspace + temp write, no network' (strict: excludes network, write_outside_explicit, write_outside_tool, git_write_worktree; does NOT count cargo's maybe-network as a break):** 10338/14269 (72.5%)
**Same, but also treating cargo build/test/run/install/add/update as a break (lower bound):** 10338/14269 (72.5%)

top 15 commands, 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get):
  - gh issue: 1399
  - gh pr: 585
  - gh run: 397
  - gh api: 275
  - git push: 272
  - git fetch: 230
  - git pull: 36
  - gh label: 35
  - curl: 28
  - gh repo: 21
  - gh workflow: 16
  - git clone: 12
  - gh auth: 11
  - bun install: 11
  - npm ci: 7

top 15 commands, 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached):
  - cargo build: 56
  - cargo test: 15
  - cargo check: 4
  - cargo run: 2
  - cargo add: 1
  - cargo install: 1

top 15 commands, 2a. Writes outside workspace: explicit path outside cwd (non-temp):
  - cat: 48
  - sed: 36
  - rm: 26
  - cp: 20
  - mkdir: 14
  - ln: 12
  - codex: 10
  - echo: 9
  - mv: 8
  - printf: 3
  - unzip: 3
  - use: 3
  - gh issue: 3
  - do: 3
  - timeout: 3

top 15 commands, 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break):
  - cat: 389
  - rm: 63
  - gh issue: 60
  - echo: 49
  - zig: 48
  - cp: 40
  - mkdir: 35
  - sed: 29
  - tee: 25
  - gh api: 19
  - [2026-09-23]: 13
  - codex: 13
  - timeout: 12
  - ": 12
  - ': 10

top 15 commands, 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor):
  - gh issue: 1418
  - gh pr: 729
  - gh run: 438
  - gh api: 278
  - bun test: 110
  - codex: 76
  - cargo build: 54
  - gh label: 37
  - npm ci: 36
  - pi: 33
  - claude: 32
  - npm run: 21
  - gh repo: 20
  - gh workflow: 18
  - code: 13

top 15 commands, git write subcommand (any) -- only a break if cwd is a git worktree (3.):
  - git add: 365
  - git worktree: 303
  - git checkout: 170
  - git branch: 100
  - git commit: 62
  - git switch: 53
  - git stash: 42
  - git rm: 20
  - git rebase: 17
  - git merge: 10
  - git tag: 8
  - git mv: 7
  - git reset: 6
  - git init: 6
  - git apply: 4

top 15 commands, 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace):
  - git add: 2
  - git switch: 1

## Claude Code (subagents)

session files: 177, sessions with >=1 shell call: 174
total shell calls: 8794

| class | calls | % of calls | sessions | % of sessions-with-shell |
|---|---|---|---|---|
| 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get) | 1079 | 12.3% | 130 | 74.7% |
| 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached) | 6 | 0.1% | 3 | 1.7% |
| 2a. Writes outside workspace: explicit path outside cwd (non-temp) | 81 | 0.9% | 32 | 18.4% |
| 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break) | 1067 | 12.1% | 131 | 75.3% |
| 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor) | 1067 | 12.1% | 113 | 64.9% |
| git write subcommand (any) -- only a break if cwd is a git worktree (3.) | 575 | 6.5% | 100 | 57.5% |
| 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace) | 3 | 0.0% | 1 | 0.6% |

**Would pass unharmed under 'workspace + temp write, no network' (strict: excludes network, write_outside_explicit, write_outside_tool, git_write_worktree; does NOT count cargo's maybe-network as a break):** 7362/8794 (83.7%)
**Same, but also treating cargo build/test/run/install/add/update as a break (lower bound):** 7362/8794 (83.7%)

top 15 commands, 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get):
  - gh issue: 230
  - git fetch: 218
  - gh run: 187
  - git push: 143
  - gh pr: 122
  - npm ci: 71
  - gh api: 41
  - curl: 32
  - git clone: 14
  - npm install: 5
  - gh repo: 5
  - gh search: 4
  - gh auth: 4
  - npm i: 1
  - gh 2>&1: 1

top 15 commands, 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached):
  - cargo build: 4
  - cargo check: 2

top 15 commands, 2a. Writes outside workspace: explicit path outside cwd (non-temp):
  - sed: 26
  - cat: 12
  - rm: 8
  - cp: 7
  - ln: 5
  - if: 3
  - mkdir: 3
  - const: 2
  - do: 2
  - mv: 2
  - Broke: 1
  - while: 1
  - statSync(fleet().get(id).view.log).size: 1
  - plotHeight: 1
  - --s.refs: 1

top 15 commands, 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break):
  - cat: 287
  - sed: 134
  - cp: 97
  - mkdir: 87
  - rm: 54
  - gh issue: 36
  - git show: 28
  - npm test: 23
  - if: 22
  - done: 20
  - gh pr: 16
  - git ls-files: 15
  - gh run: 14
  - timeout: 13
  - tee: 13

top 15 commands, 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor):
  - gh issue: 233
  - gh run: 192
  - gh pr: 185
  - npm test: 156
  - npm run: 117
  - npm ci: 71
  - gh api: 40
  - claude: 16
  - pi: 7
  - go: 6
  - npm view: 6
  - gh repo: 5
  - gh search: 4
  - cargo build: 4
  - gh auth: 4

top 15 commands, git write subcommand (any) -- only a break if cwd is a git worktree (3.):
  - git add: 235
  - git rebase: 90
  - git commit: 61
  - git checkout: 54
  - git worktree: 34
  - git stash: 34
  - git branch: 26
  - git rm: 20
  - git mv: 6
  - git reset: 6
  - git init: 4
  - git switch: 3
  - git apply: 1
  - git merge: 1

top 15 commands, 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace):
  - git worktree: 2
  - git branch: 1

## Correction: git writes from a worktree

The "git write in worktree" column reads 0.0% because the script checks
each session's directory on disk today, and most worktrees have since been
deleted. Counting instead by a `/worktrees/` path in the session's `cwd` or
in the command: 254 of 1,962 Claude Code git writes (13%) ran from a
worktree. This is a lower bound. A worktree's `.git` is a file pointing
into the main checkout's `.git/worktrees/<name>`, so a commit there writes
outside the workspace.
