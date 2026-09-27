# Fiber agent instructions

Fiber is a terminal coding agent harness written in Rust. Read the file that
matches the work before starting it.

- How a ticket becomes a merged pull request: `docs/workflow.md`. Every code
  change passes `scripts/check` before a pull request opens.
- Vocabulary: `CONTEXT.md`. Use its terms in code, docs and issues.
- What each area does: `docs/<area>.md`. The crates and their boundaries:
  `docs/architecture.md`. Decisions: `docs/adr/`.
- Tests: `docs/testing.md`. Code rules: `docs/code-quality.md`. Crates and
  toolchain: `docs/dependencies.md`. CI: `docs/ci.md`. Budgets:
  `docs/performance.md`.

Docs state what is true now. When code and a doc disagree, the doc wins and
the code changes. When a doc cannot be met as written, or building it
shows a problem or a simpler design, follow `docs/workflow.md`, "When a doc
should change".

Plans, specs and backlogs live on GitHub Issues, never in files in the
repository.
