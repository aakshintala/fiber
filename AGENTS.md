# Fiber agent instructions

Fiber is a terminal coding agent harness written in Rust. Read the file that
matches the work before starting it.

- How a ticket becomes a merged pull request: `docs/workflow.md`. Every code
  change passes `scripts/check` before a pull request opens.
- Vocabulary: `GLOSSARY.md`. Use its terms in code, docs and issues.
- Pages for a person using Fiber: `docs/user/`. Each links into the area docs
  rather than restating them, and is written only once its area is built.
- What each area does: `docs/<area>.md`. The crates and their boundaries:
  `docs/architecture.md`. Decisions: `docs/adr/`.
- Tests: `docs/testing.md`. Code rules: `docs/code-quality.md`. Crates and
  toolchain: `docs/dependencies.md`. CI: `docs/ci.md`. Budgets:
  `docs/performance.md`.

Docs state what is true now. When code and a doc disagree, the doc wins and
the code changes. When a doc cannot be met as written, or building it
shows a problem or a simpler design, follow `docs/workflow.md`, "When a doc
should change".

Area docs and `GLOSSARY.md` describe only Fiber. What other tools do, and
measurements of how the owner works, are evidence: they live in
`research/<topic>/` or an ADR, and the doc points at them in one line where
the reason matters. A fact Fiber's own behaviour depends on, such as a flag
Fiber passes to another harness, stays in the area doc.

Plans, specs and backlogs live on GitHub Issues, never in files in the
repository.

## Agent skills

### Issue tracker

GitHub Issues on `aakshintala/fiber`, through `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

The default role names, except `needs-info` and `ready-for-human`, which both use `needs-owner`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the root. See `docs/agents/domain.md`.

### Workflow

`docs/workflow.md`. It wins wherever it speaks; skills use their own defaults where it is silent. See `docs/agents/workflow.md`.
