# Owner's real project-instruction files: a measurement

Scope: every pi session under `~/.pi/agent/sessions/**/*.jsonl` (689) and every
Claude Code session under `~/.claude/projects/**/*.jsonl`, split into main
sessions (236) and subagent sessions (153, path contains `/subagents/`). Two
Claude Code lines had no `cwd` field and are excluded from cwd-level analysis.

Scripts: `collect_sessions.py` (step 1 + step 5), `scan_instruction_files.py`
(steps 2-4 raw file discovery), `classify_and_report.py` (step 4 aggregation).
Raw intermediates: `sessions_by_cwd.json`, `cwd_instructions.json`,
`classification_report.json`. They list local paths, so they are not
committed; the scripts regenerate them.

## Corrections

Checked by hand after the run, on September 26, 2026:

- The scan read the shared checkout of `~/work/fiber`, whose local `main` was
  behind. `origin/main` has had `AGENTS.md` (912 bytes) and a `CLAUDE.md` whose
  whole content is `@AGENTS.md` since September 25, 2026. So `@path` imports do
  occur, and fiber is the second repository, with lens, whose `CLAUDE.md` only
  points at `AGENTS.md`. The "no project instruction file" bucket counts
  fiber's sessions from before that date correctly, but it describes the
  repository's past, not its present.
- The "nested subdirectory instruction files" row (181 sessions, 16.8%) counts
  root files by mistake. No repository on this machine has an instruction file
  below its root. The owner's work monorepo, which is not in this data, does.
- The sizes below describe these repositories only. Instruction files can be
  much larger elsewhere, so the design sets no size assumption.

## 1. Distinct working directories

64 distinct cwds across both tools. Top by total session count:

| cwd | pi | cc main | cc sub |
|---|---|---|---|
| `~/work/fiber` | 489 | 110 | 32 |
| `~/work/pi-extensions` | 60 | 20 | 82 |
| `~/work/fiber-design` | 1 | 50 | 2 |
| `~/work` (bare) | 25 | 0 | 0 |
| `~/work/fiber-ci` | 0 | 15 | 7 |
| `~/work/cursor-delegate` | 13 | 5 | 0 |
| `~/work/ClaudeBar` | 7 | 6 | 4 |

The remaining 57 cwds are single- or few-session: per-session pi sandboxes
under `/var/folders/.../T/pi-agent-*` (24 of these, 1 session each — these are
pi's ephemeral worktree copies, not the owner's real checkout), Claude Code
scratchpad dirs under `/private/tmp/claude-501/...`, and several fiber git
worktrees under `.claude/worktrees/agent-*` (each a one-shot delegated-PR
sandbox, 1-5 sessions).

**24 of the 64 cwds still exist on disk.** The other 40 are ephemeral sandboxes
(pi's `/var/folders` temp worktrees, Claude Code scratchpads, deleted git
worktrees) that were never meant to persist.

## 2-3. Instruction files found

Across every cwd that still exists, every ancestor up to `$HOME`, every git
root, and nested files (`git ls-files`, capped depth 6): only three filenames
ever appear anywhere in this dataset — **`AGENTS.md`, `CLAUDE.md`, and
`~/.claude/CLAUDE.md`** (the global one, which is also an ancestor of every
cwd under `$HOME`). `CLAUDE.local.md`, `AGENTS.override.md`, `.cursorrules`,
`.github/copilot-instructions.md`, and `GEMINI.md` were **not found once** in
any of the owner's real repos. No ancestor directory strictly between a cwd
and `$HOME` ever carried an instruction file — every file found is either
exactly at the project root or at `$HOME` (the global file).

Project-root instruction files found, largest first:

| repo | file | bytes | lines | ~tokens |
|---|---|---|---|---|
| ClaudeBar | `CLAUDE.md` | 14,154 | 382 | 3,538 |
| lens | `AGENTS.md` | 2,859 | 53 | 715 |
| lens | `CLAUDE.md` | 1,766 | 35 | 442 |
| pi-extensions | `AGENTS.md` | 1,502 | 40 | 376 |
| fiber (main, current) | *(none)* | 0 | 0 | 0 |

Global files:

| file | bytes | lines | ~tokens |
|---|---|---|---|
| `~/.claude/CLAUDE.md` | 1,128 | 23 | 282 |
| `~/.pi/agent/AGENTS.md` | 456 | 11 | 114 |
| `~/.codex/AGENTS.md` | does not exist | — | — |

`~/.codex/` has no AGENTS.md-equivalent; Codex's persistent memory instead
lives as free text in `~/.codex/memories/{MEMORY,raw_memories,memory_summary}.md`,
a different mechanism (retrieved memory, not a fixed system-prompt file).

Note: `~/work/fiber` (this repo) currently has **zero** instruction files on
its `main` branch — `AGENTS.md`/`CLAUDE.md` only exist on the
`docs/system-prompt` branch (this worktree), added as part of this repo's own
subject matter. Historical sessions that `cat`'d `AGENTS.md` in this repo saw
a file that later moved/changed; this scan reflects **current on-disk state
only**, not what a given historical session actually loaded — repo content
changes over 3+ weeks of sessions and this is a real limitation of the method.

**lens is the one repo with both `AGENTS.md` and `CLAUDE.md` at the same
directory.** They are not identical, not symlinked, and neither uses an `@`
import. `CLAUDE.md` is 35 lines and opens with "Read `AGENTS.md` first — it
holds the shared rules... This file adds Claude-only rules," i.e. a plain-text
cross-reference (delegation policy, model routing) layered on a shared base
(product description, severity taxonomy, performance rules). This is the only
observed instance of file-splitting by tool in the whole dataset.

**No `CLAUDE.md` anywhere uses an `@path` import.** Checked every file found
(ClaudeBar, lens x2, pi-extensions, both global files) — zero `@`-prefixed
lines.

## 4. Session classification (session-weighted, 1,076 sessions with a cwd)

| bucket | sessions | % |
|---|---|---|
| cwd no longer exists on disk (global-only load) | 139 | 12.9% |
| cwd exists, no project instruction file at all | 751 | 69.8% |
| only `AGENTS.md` at project root | 162 | 15.1% |
| only `CLAUDE.md` at project root | 22 | 2.0% |
| both `AGENTS.md` and `CLAUDE.md` at project root | 2 | 0.2% |
| — with files in a true ancestor dir (strictly between cwd and `$HOME`) | 0 | 0% |
| — with nested subdirectory instruction files (`git ls-files`) | 181 | 16.8% |

**82.7% of real sessions ran with no project-level instruction file at all**
(751 "no file" + 139 "cwd gone" combined), leaving only the global default
(`~/.pi/agent/AGENTS.md` or `~/.claude/CLAUDE.md`) in play. That 82.7% is
dominated by one cwd: `~/work/fiber` main-branch sessions (631 of the 1,076),
which currently carries no instruction file at all.

Size distribution of total instruction text a session loads at start (global
+ project-root file it actually reads — pi is modeled as reading the
`AGENTS.md` family only, since pi's own sessions explicitly `cat AGENTS.md`;
Claude Code is modeled as reading the `CLAUDE.md` family only, its documented
default):

| source | n | min tok | median tok | p90 tok | max tok |
|---|---|---|---|---|---|
| pi | 689 | 114 | 114 | 114 | 829 |
| Claude Code (main+sub) | 387 | 282 | 282 | 282 | 3,820 |
| combined | 1,076 | 114 | 114 | 282 | 3,820 |

The median session of either tool loads **only the global default file**
(114 or 282 tokens) — project instructions are the exception, not the rule,
in the owner's real usage. The max (3,820 tokens, ~15KB) is a single repo
(ClaudeBar) whose `CLAUDE.md` is 20x the size of the next-largest project file
found.

## 5. Instruction files edited mid-session

Detected by scanning each session's tool-use log for an `Edit`/`Write`
(Claude Code) or `edit`/`write` (pi) call whose target basename is
`AGENTS.md`, `CLAUDE.md`, `CLAUDE.local.md`, or `AGENTS.override.md`:

| source | sessions that edited an instruction file | of |
|---|---|---|
| pi | 5 | 689 (0.7%) |
| Claude Code main | 7 | 236 (3.0%) |
| Claude Code subagent | 1 | 153 (0.65%) |

Editing your own instruction file mid-session is rare across both tools (well
under 5%), and slightly more common in Claude Code main sessions than pi or
subagent sessions — consistent with instruction-file maintenance being a
deliberate, occasional owner action rather than something agents do
routinely as part of task work.

## Caveats

- pi's per-session `cwd` is whatever sandbox that session actually ran in,
  including ephemeral `/var/folders` worktree copies pi creates per run — most
  of the 40 "gone" cwds are exactly this, by design, not data loss.
- File-content scanning reflects **today's** on-disk state, not what a
  session from weeks ago actually saw; repos (especially `fiber`) have added,
  moved, and removed instruction files over the measured period.
- "Nested subdirectory instruction files" were found via `git ls-files`
  (capped at depth 6 from cwd) only for cwds that are git repos; the 16.8%
  figure is a lower bound for non-repo cwds (there were none among the
  existing cwds with any content, so this doesn't undercount here).
