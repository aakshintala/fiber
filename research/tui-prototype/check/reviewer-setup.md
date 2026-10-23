# Reviewer setup (#2480)

One run per case: `--reviewer-setup CASE`.

- first: topic `4 of 5 · Routine actions` with its question (`What routine actions …`), the `gh pr merge … --squash` escalation count as evidence and the `[per-project]` line barred; a centred panel with ▄ ▀ edges, `Reviewer setup` bold accent with a dim ✕, and a bold-key legend foot.
- outside: topic `2 of 5 · Internal hosts` with its question (`Which internal hosts …`) and the person's answer as evidence and the `[global]` line barred, with no line about a project; the same panel and legend.
- rerun: topic `1 of 5 · Source control`, two kept `✓` earlier lines under dim `Earlier lines`, then the evidence and the new proposal barred under dim `New proposals`, with no question text; the same panel with a keep-and-strike legend.
- struck: topic `4 of 5 · Routine actions`, then the dim `struck — kept out of the notes` line, then the struck line with strikethrough on the bar, and nothing else; the same panel with a keep-or-back legend.
- flipped: the topic with its question and the same line as `first` barred with the `[global]` tag, under a dim `was per-project` line; the same panel and legend.
- diff: each layer's notes as a diff against the file as it is now with the file named, the topics' heading lines as saved (`### Source control`), kept bullets as context, struck bullets with `-` in the removed colour, added bullets and new headings with `+` in the added colour, `(no changes)` where the layer is untouched, and a dim reload line; the same panel with an approve-or-cancel legend.
- notice: the notice text naming `/reviewer-setup`, wrapped to the panel; the same panel with an any-key-closes legend.
- question: topic `1 of 5 · Source control` with its question (`Which accounts …`) and a one-line answer field with a block cursor, no layer tag; the same panel with an answer-or-back legend.
- waiting: topic `1 of 5 · Source control` and a dim `Proposing lines…` row; the same panel with a back-only legend.
- saved: each file named with whether it was saved: the global file saved with a dim sync error beside it, the per-project file saved; a bold-key reload-now-or-next-session legend.
