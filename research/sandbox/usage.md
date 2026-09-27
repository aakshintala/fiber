# Sandbox confinement: how often would shell calls break? (ticket #30)

Measured 2026-09-26, one pass per corpus, on this machine's real session logs. Confinement modeled: write access to the session's cwd and a temp dir, no network, no writes anywhere else.


## Heuristic limits (read before trusting the percentages)

- Classification runs on the RAW command STRING, not on what the process
  actually did. `git commit --dry-run` and `git commit` classify the same.
- A pipeline/chain (`&&`, `||`, `;`, `|`) is classified as a whole: if ANY
  segment trips a class, the whole command counts for that class, because a
  `&&` chain stops at the first blocked segment. A segment's "first
  tool+subcommand" is used for the top-15 label even when later segments
  also tripped other classes.
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
  forms like `2>&1`), a `-o`/`-O`/`--output` flag, or a first word from a
  small write-verb list (`cp mv rm mkdir touch ln chmod chown tee dd rsync
  tar unzip zip patch`; `sed`/`perl` only count with `-i`). Everything else
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
| 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get) | 3384 | 12.2% | 348 | 58.2% |
| 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached) | 0 | 0.0% | 0 | 0.0% |
| 2a. Writes outside workspace: explicit path outside cwd (non-temp) | 357 | 1.3% | 120 | 20.1% |
| 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break) | 2074 | 7.5% | 239 | 40.0% |
| 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor) | 4160 | 15.0% | 390 | 65.2% |
| git write subcommand (any) -- only a break if cwd is a git worktree (3.) | 1119 | 4.0% | 274 | 45.8% |
| 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace) | 0 | 0.0% | 0 | 0.0% |

**Would pass unharmed under 'workspace + temp write, no network' (strict: excludes network, write_outside_explicit, write_outside_tool, git_write_worktree; does NOT count cargo's maybe-network as a break):** 22845/27818 (82.1%)
**Same, but also treating cargo build/test/run/install/add/update as a break (lower bound):** 22845/27818 (82.1%)

top 15 commands, 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get):
  - gh pr: 1219
  - gh issue: 556
  - gh api: 481
  - gh run: 458
  - git fetch: 283
  - git push: 247
  - curl: 49
  - bun install: 30
  - git pull: 23
  - gh auth: 10
  - npm install: 7
  - gh label: 6
  - gh repo: 5
  - gh search: 4
  - gh variable: 2

top 15 commands, 2a. Writes outside workspace: explicit path outside cwd (non-temp):
  - sed: 77
  - cp: 69
  - rm: 51
  - grep: 27
  - cat: 26
  - mkdir: 23
  - echo: 8
  - find: 7
  - awk: 7
  - chmod: 7
  - touch: 7
  - try: 5
  - lldb: 5
  - python3: 5
  - !pane.includes("No: 3

top 15 commands, 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break):
  - cat: 615
  - zig: 187
  - rm: 164
  - mkdir: 164
  - sed: 110
  - timeout: 77
  - node: 67
  - cp: 63
  - bun test: 60
  - do: 27
  - python3: 27
  - chmod: 25
  - gh run: 25
  - sort: 24
  - npm test: 23

top 15 commands, 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor):
  - gh pr: 1273
  - bun test: 832
  - gh issue: 568
  - gh api: 482
  - gh run: 470
  - npm test: 67
  - bun run: 64
  - pi: 59
  - bun install: 30
  - npm run: 25
  - bun build: 19
  - bun 2>&1: 18
  - codex: 13
  - bun ': 13
  - bun repro-min.ts: 12

top 15 commands, git write subcommand (any) -- only a break if cwd is a git worktree (3.):
  - git add: 333
  - git branch: 246
  - git worktree: 233
  - git checkout: 99
  - git stash: 91
  - git commit: 39
  - git rebase: 33
  - git merge: 13
  - git rm: 12
  - git tag: 5
  - git mv: 5
  - git apply: 4
  - git reset: 3
  - git switch: 1
  - git init: 1

## Claude Code (main)

session files: 239, sessions with >=1 shell call: 189
total shell calls: 14260

| class | calls | % of calls | sessions | % of sessions-with-shell |
|---|---|---|---|---|
| 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get) | 3159 | 22.2% | 138 | 73.0% |
| 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached) | 47 | 0.3% | 11 | 5.8% |
| 2a. Writes outside workspace: explicit path outside cwd (non-temp) | 271 | 1.9% | 91 | 48.1% |
| 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break) | 1036 | 7.3% | 125 | 66.1% |
| 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor) | 3204 | 22.5% | 141 | 74.6% |
| git write subcommand (any) -- only a break if cwd is a git worktree (3.) | 1047 | 7.3% | 123 | 65.1% |
| 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace) | 1 | 0.0% | 1 | 0.5% |

**Would pass unharmed under 'workspace + temp write, no network' (strict: excludes network, write_outside_explicit, write_outside_tool, git_write_worktree; does NOT count cargo's maybe-network as a break):** 10544/14260 (73.9%)
**Same, but also treating cargo build/test/run/install/add/update as a break (lower bound):** 10544/14260 (73.9%)

top 15 commands, 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get):
  - gh issue: 1259
  - gh pr: 586
  - gh run: 387
  - gh api: 266
  - git push: 238
  - git fetch: 233
  - git pull: 36
  - gh label: 35
  - curl: 25
  - gh repo: 21
  - gh workflow: 14
  - gh auth: 11
  - bun install: 11
  - git clone: 11
  - npm ci: 7

top 15 commands, 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached):
  - cargo build: 25
  - cargo test: 15
  - cargo check: 3
  - cargo run: 2
  - cargo add: 1
  - cargo install: 1

top 15 commands, 2a. Writes outside workspace: explicit path outside cwd (non-temp):
  - sed: 56
  - cat: 42
  - rm: 19
  - cp: 18
  - mkdir: 17
  - ln: 12
  - echo: 10
  - grep: 9
  - mv: 7
  - codex: 7
  - do: 5
  - find: 4
  - gh issue: 4
  - printf: 3
  - unzip: 3

top 15 commands, 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break):
  - cat: 361
  - echo: 79
  - gh issue: 58
  - rm: 56
  - mkdir: 36
  - cp: 33
  - sed: 33
  - zig: 30
  - tee: 25
  - gh api: 19
  - timeout: 16
  - codex: 13
  - ": 12
  - do: 10
  - ': 10

top 15 commands, 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor):
  - gh issue: 1280
  - gh pr: 707
  - gh run: 418
  - gh api: 269
  - bun test: 101
  - codex: 70
  - gh label: 37
  - npm ci: 36
  - claude: 31
  - pi: 27
  - cargo build: 23
  - npm run: 20
  - gh repo: 20
  - gh workflow: 16
  - gh auth: 11

top 15 commands, git write subcommand (any) -- only a break if cwd is a git worktree (3.):
  - git worktree: 294
  - git add: 221
  - git checkout: 163
  - git commit: 121
  - git branch: 100
  - git switch: 51
  - git stash: 31
  - git rebase: 17
  - git rm: 13
  - git merge: 10
  - git tag: 7
  - git reset: 6
  - git init: 5
  - git mv: 4
  - git revert: 2

top 15 commands, 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace):
  - git switch: 1

## Claude Code (subagents)

session files: 177, sessions with >=1 shell call: 174
total shell calls: 8758

| class | calls | % of calls | sessions | % of sessions-with-shell |
|---|---|---|---|---|
| 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get) | 1026 | 11.7% | 123 | 70.7% |
| 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached) | 6 | 0.1% | 3 | 1.7% |
| 2a. Writes outside workspace: explicit path outside cwd (non-temp) | 137 | 1.6% | 51 | 29.3% |
| 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break) | 1154 | 13.2% | 133 | 76.4% |
| 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor) | 984 | 11.2% | 107 | 61.5% |
| git write subcommand (any) -- only a break if cwd is a git worktree (3.) | 560 | 6.4% | 98 | 56.3% |
| 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace) | 5 | 0.1% | 1 | 0.6% |

**Would pass unharmed under 'workspace + temp write, no network' (strict: excludes network, write_outside_explicit, write_outside_tool, git_write_worktree; does NOT count cargo's maybe-network as a break):** 7356/8758 (84.0%)
**Same, but also treating cargo build/test/run/install/add/update as a break (lower bound):** 7356/8758 (84.0%)

top 15 commands, 1. Network (definite: git push/pull/fetch/clone, gh, curl, wget, package-manager install/add, ssh/scp, brew, docker pull, go get):
  - gh issue: 218
  - git fetch: 212
  - gh run: 197
  - git push: 129
  - gh pr: 106
  - npm ci: 69
  - gh api: 40
  - curl: 29
  - git clone: 6
  - npm install: 5
  - gh repo: 5
  - gh auth: 4
  - gh search: 3
  - npm i: 1
  - gh 2>&1: 1

top 15 commands, 1b. Network, MAYBE (cargo build/test/run/install/add/update -- only needs network if crates are not already cached):
  - cargo build: 4
  - cargo check: 2

top 15 commands, 2a. Writes outside workspace: explicit path outside cwd (non-temp):
  - sed: 55
  - find: 16
  - cat: 11
  - rm: 8
  - cp: 7
  - mkdir: 6
  - ln: 5
  - grep: 4
  - if: 3
  - echo: 3
  - const: 2
  - do: 2
  - mv: 2
  - footer.*tokens\b": 1
  - unchanged: 1

top 15 commands, 2b. Writes to /tmp or $TMPDIR (allowed under this confinement -- reported, not a break):
  - cat: 262
  - sed: 198
  - cp: 92
  - mkdir: 80
  - rm: 49
  - gh issue: 35
  - git show: 28
  - npm test: 23
  - do: 16
  - gh pr: 15
  - git ls-files: 15
  - gh run: 13
  - tee: 13
  - find: 12
  - timeout: 11

top 15 commands, 2c. Writes outside workspace: tool with known out-of-tree state (cargo/npm/pip/go/brew/gh/claude/codex/pi/editor):
  - gh issue: 221
  - gh run: 206
  - npm test: 145
  - gh pr: 133
  - npm run: 116
  - npm ci: 69
  - gh api: 39
  - claude: 13
  - pi: 6
  - npm view: 5
  - gh repo: 5
  - cargo build: 4
  - gh auth: 4
  - npm install: 3
  - gh search: 3

top 15 commands, git write subcommand (any) -- only a break if cwd is a git worktree (3.):
  - git add: 212
  - git rebase: 91
  - git commit: 78
  - git checkout: 50
  - git worktree: 33
  - git stash: 33
  - git branch: 26
  - git rm: 17
  - git reset: 6
  - git mv: 5
  - git init: 4
  - git switch: 3
  - git apply: 1
  - git merge: 1

top 15 commands, 3. Git writes where cwd is a worktree (.git is a file pointing outside cwd; local git write goes outside the workspace):
  - git add: 3
  - git branch: 1
  - git worktree: 1
