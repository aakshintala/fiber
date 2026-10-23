# Owner's answers behind the setup-written notes (#2482)

The five setup questions are quoted verbatim from `Topic::question` in
`crates/contract/src/notes.rs`. Each answer below is the owner's
approved note, split into one line per topic. The assembled notes are in
`notes.md`.

## 1. Source control

Question: "Which accounts, organisations and repositories are your own
or your organisation's?"

Answer (one line):

- github.com/aakshintala is my account, and its repositories, including aakshintala/fiber, are mine.

## 2. Internal hosts

Question: "Which internal hosts, registries and services count as
inside?"

Answer: none. The approved note names no internal host, registry or
service, so the setup writes no "Internal hosts" heading.

## 3. Internal tools

Question: "Which internal tools do you run, and what does running each
one do?"

Answer: none. The approved note names no internal tool, so the setup
writes no "Internal tools" heading.

## 4. Routine actions

Question: "What routine actions, such as squash-merging your own pull
requests once CI passes, should the reviewer allow?"

Answer (one line per bullet of the note's routine-actions list):

- committing, and pushing any branch except main to origin on my repositories
- opening, updating and commenting on pull requests and issues in my repositories, and squash-merging my own pull requests once CI passes
- running and rerunning GitHub Actions workflows on my repositories
- writing an agent's memory, handoff and notes files under ~/.claude, ~/work/fiber-worktrees and /tmp, including the memory files in ~/.claude/projects/*/memory/
- sending code and diffs to review models through review-pr
- killing processes the agent started, by explicit PID
- rebasing or amending a local branch that has not been merged

## 5. What must never happen

Question: "What must never happen, such as touching infra/prod?"

Answer: the note's sentence "What must never happen: pushing to main,
force-pushing, pattern kills such as pkill or killall, and sending the
text of my session logs off this machine. Aggregate counts from my
session logs may be published; their text never may.", split at its
commas into one line per rule, plus the aggregate-counts sentence as
its own line:

- pushing to main
- force-pushing
- pattern kills such as pkill or killall
- sending the text of my session logs off this machine
- Aggregate counts from my session logs may be published; their text never may.

## Not carried

See the "Two parts of the hand-written note are not carried" paragraph
in `../README.md`: the opening prose and the routine-actions
permission qualifier have no slot in the setup's template.
