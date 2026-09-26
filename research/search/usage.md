# Search: built-in tools or the shell? (ticket #54)

Measured 2026-09-26. One process, one pass over each session log, counts and shares only (no timings).

pi sessions with any tool call: 645

Claude Code main sessions with any tool call: 191

Claude Code subagent sessions with any tool call: 153


Shell-search detection: a bash/Bash command is split on `&& || ; |`; each segment's first word (after stripping env-var assignments) is checked against rg/grep/git grep/find/fd/ls -R/tree/ag/ack. Follow-up is a rough proxy: the very next tool call in the transcript is a read whose path string appears in the search result text.


## pi

sessions with any tool call: 645

tool names matching grep/find/glob/fff: ['fffind', 'ffgrep', 'find', 'grep']  (excluded as web search, not code search: ['web_search'])

**1. Dedicated search tool calls**

| tool | calls | sessions using | share of sessions |
|---|---|---|---|
| grep | 1291 | 132 | 20.5% |
| ffgrep | 1057 | 116 | 18.0% |
| find | 150 | 44 | 6.8% |
| fffind | 101 | 54 | 8.4% |

**2. Shell search calls**

shell (bash/Bash) calls: 27823, containing a search command: 12546 (45.1%)
| shell search command | calls |
|---|---|
| grep | 11841 |
| find | 387 |
| git grep | 302 |
| rg | 154 |
| ls -R | 39 |
| tree | 1 |

of all search actions (15145): 82.8% via shell, 17.2% via a dedicated tool

**3. Result sizes**

dedicated tools: n=2599 p50=1551 p90=14097 p99=51292 max=171473  >8K 17.4% >16K 8.4% >50K 1.4%
shell search: n=12546 p50=876 p90=3671 p99=12041 max=51342  >8K 2.0% >16K 0.5% >50K 0.1%

**4. Errors**

dedicated tools: 55/2599 (2.12%)
  - other: 27
  - path not found: 13
  - invalid regex: 13
  - search path not a directory: 2
shell search: 295/12546 (2.35%) nonzero-exit/isError
  - exit 1 (rg/grep convention: no match, or real error): 143
  - blocked (sandbox): 27
  - no matches (exit 1, empty output): 27
  - permission denied: 8
  - (no output yet): 8
  - Blocked: sleep 90. A fixed `sleep N` to wait wastes time and leaves a : 7
  - Command timed out after 120 seconds: 2
  - Command aborted: 2
  - Blocked: sleep 2. A fixed `sleep N` to wait wastes time and leaves a j: 2
  - Blocked: sleep 240. A fixed `sleep N` to wait wastes time and leaves a: 2

**5. Arguments of dedicated tools**

fffind (n=101):
  - pattern: 100.0% (101)
  - path: 75.2% (76)
  - limit: 43.6% (44)
ffgrep (n=1057):
  - pattern: 100.0% (1057)
  - path: 95.6% (1011)
  - limit: 82.1% (868)
  - context: 58.9% (623)
  - exclude: 2.6% (28)
  - glob: 1.4% (15)
  - cursor: 0.4% (4)
  - caseSensitive: 0.3% (3)
  - literal: 0.2% (2)
  - ignoreCase: 0.1% (1)
find (n=150):
  - pattern: 100.0% (150)
  - path: 97.3% (146)
  - limit: 42.0% (63)
grep (n=1291):
  - pattern: 100.0% (1291)
  - path: 99.5% (1285)
  - limit: 68.9% (889)
  - context: 51.1% (660)
  - glob: 16.0% (206)
  - ignoreCase: 6.7% (86)
  - literal: 6.5% (84)
  - headLimit: 0.3% (4)
  - outputMode: 0.3% (4)
  - head_limit: 0.2% (2)
  - exclude: 0.1% (1)
  - -i: 0.1% (1)

shell rg/grep/git-grep flags, top 20:
  - -n: 13439
  - -r: 4997
  - -v: 2014
  - -E: 1943
  - -i: 1173
  - --include: 947
  - -l: 754
  - -c: 742
  - -A: 731
  - -o: 315
  - -a: 254
  - -B2: 180
  - --: 161
  - -e: 146
  - -h: 104
  - -B3: 95
  - -B5: 80
  - -A8: 72
  - -A12: 64
  - -A25: 62

**6. Truncation signals**

dedicated-tool results with a truncation/limit notice: 523/2599 (20.1%)

**7. Follow-up read of a path from the search result**

dedicated tools: next call is a read, path from result: 31/587 (5.3%)
shell search: next call is a read, path from result: 82/948 (8.6%)

## Claude Code (main)

sessions with any tool call: 191

tool names matching grep/find/glob/fff: (none)  (excluded as web search, not code search: ['ToolSearch', 'WebSearch'])

**1. Dedicated search tool calls**

| tool | calls | sessions using | share of sessions |
|---|---|---|---|
| (none in this corpus) | | | |

**2. Shell search calls**

shell (bash/Bash) calls: 14137, containing a search command: 5308 (37.5%)
| shell search command | calls |
|---|---|
| grep | 5000 |
| find | 222 |
| rg | 157 |
| git grep | 71 |
| ls -R | 20 |
| tree | 7 |
| fd | 1 |
| ag | 1 |

of all search actions (5308): 100.0% via shell, 0.0% via a dedicated tool

**3. Result sizes**

dedicated tools: n=0
shell search: n=5308 p50=929 p90=3936 p99=11085 max=29954  >8K 2.3% >16K 0.4% >50K 0.0%

**4. Errors**

dedicated tools: no calls
shell search: 101/5308 (1.90%) nonzero-exit/isError
  - exit 1 (rg/grep convention: no match, or real error): 71
  - blocked (sandbox): 5
  - permission denied: 4
  - The user doesn't want to proceed with this tool use. The tool use was : 3
  - no matches (exit 1, empty output): 2
  - <tool_use_error>Blocked: sleep 30 followed by: ls -la /private/tmp/cla: 1
  - ./--Users-aakshintala-work-fiber--/2026-09-01T17-01-23-616Z_01a05deb-1: 1
  - <tool_use_error>Blocked: sleep 30 followed by: gh pr checks 228 -R aak: 1
  - claude-sonnet-5[1m] is temporarily unavailable (overloaded), so auto m: 1
  - === any remaining refs to old resume flags anywhere in src/ ===: 1

**5. Arguments of dedicated tools**

(no dedicated-tool calls in this corpus)

shell rg/grep/git-grep flags, top 20:
  - -n: 5152
  - -r: 2068
  - -E: 1684
  - -i: 1261
  - -v: 1110
  - -l: 451
  - -c: 350
  - --include: 324
  - -o: 324
  - -A: 205
  - -B2: 98
  - -A6: 73
  - -a: 72
  - --: 70
  - -A12: 66
  - -h: 65
  - -A3: 57
  - -A8: 54
  - -q: 46
  - -B3: 43

**6. Truncation signals**

no dedicated-tool calls in this corpus

**7. Follow-up read of a path from the search result**

dedicated tools: no next-call-is-read cases
shell search: next call is a read, path from result: 10/79 (12.7%)

## Claude Code (subagents)

sessions with any tool call: 153

tool names matching grep/find/glob/fff: (none)  (excluded as web search, not code search: ['ToolSearch', 'WebSearch'])

**1. Dedicated search tool calls**

| tool | calls | sessions using | share of sessions |
|---|---|---|---|
| (none in this corpus) | | | |

**2. Shell search calls**

shell (bash/Bash) calls: 7340, containing a search command: 3869 (52.7%)
| shell search command | calls |
|---|---|
| grep | 3718 |
| find | 208 |
| ls -R | 19 |
| git grep | 2 |
| fd | 2 |
| rg | 1 |

of all search actions (3869): 100.0% via shell, 0.0% via a dedicated tool

**3. Result sizes**

dedicated tools: n=0
shell search: n=3869 p50=848 p90=4559 p99=15394 max=29743  >8K 3.8% >16K 0.9% >50K 0.0%

**4. Errors**

dedicated tools: no calls
shell search: 55/3869 (1.42%) nonzero-exit/isError
  - exit 1 (rg/grep convention: no match, or real error): 33
  - This agent is isolated in the worktree /Users/aakshintala/work/fiber/.: 13
  - no matches (exit 1, empty output): 3
  - <tool_use_error>Blocked: sleep 60 followed by: cat /private/tmp/claude: 2
  - This agent is isolated in the worktree /Users/aakshintala/work/ClaudeB: 2
  - permission denied: 1
  - ls: /Users/aakshintala/.cache/nvim/: No such file or directory: 1

**5. Arguments of dedicated tools**

(no dedicated-tool calls in this corpus)

shell rg/grep/git-grep flags, top 20:
  - -n: 2937
  - -E: 1263
  - -r: 1064
  - -v: 880
  - -i: 344
  - -l: 252
  - --include: 171
  - -c: 156
  - -o: 60
  - -A: 56
  - -A12: 56
  - -A3: 52
  - -A8: 52
  - -A40: 50
  - -A30: 44
  - -e: 43
  - -A25: 38
  - -A15: 34
  - -B2: 32
  - -a: 30

**6. Truncation signals**

no dedicated-tool calls in this corpus

**7. Follow-up read of a path from the search result**

dedicated tools: no next-call-is-read cases
shell search: next call is a read, path from result: 12/128 (9.4%)

## Check: grep as a search or as a filter

A separate count over pi's shell calls (2026-09-26) split commands on `&&`, `||`, `;` and `|`. In 9,473 calls grep or rg appears at the start of a segment not preceded by a pipe, which means it searches files or input it names. In 2,517 more it appears only after a pipe, filtering another command's output. The shell counts above include both, so they overstate file search by about a fifth. The shell still carries most of pi's search.
