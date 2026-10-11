# Swapped views (#1633)

One run per case: `--view-case CASE`.

- delegate-view: the breadcrumb `main › ◆ review: …`, a status card naming harness, model, calls, elapsed, session and the stop target, the transcript in turn cards, and the input box sending steering to the delegate.
- job-pty: the live 80x24 screen from attach with applied output, the job's status with its stop target, and the input box typing into the job.
- running: two rows per running delegate and one row per running job, in start order, each opening that item's view.
- usage: usage broken down by turn, by model and by delegate, each with tokens by kind, billed and subscription cost, and the budget left.
- changed-files: the changed files with the chosen file's hunks as a diff against HEAD, the untracked file wholly added, removals red.
- search-results: every match one row each with the lines around it, the current match brighter, and the jump hint.
- tools: tools by source with state and size, two on/off switches per MCP or extension tool, built-ins without a switch, and the reload cost.
- rules: global rules then this project's, each with its prefix, when and from which session it was added, and its red ✕.
- settings: the keys with their effective values and the layer each comes from, the reload cost, and the file hint.
- skills: one row per skill with its source, visibility, shadowed skill and switches, and the text and file hints.
