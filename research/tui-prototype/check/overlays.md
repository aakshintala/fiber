# Overlays (#1630)

One run per case: `--overlay CASE`.

- keymap: the key map should dock at the bottom full width with ▄ ▀ edges; `Key map` bold accent with a dim ✕, a muted purpose and `34 actions, 24 with other paths`; the All tab inverse with the rest muted; a muted search line with an empty query; group, action and keys columns starting together on three fixed columns with other paths dim in the keys; the first row `›` on a full-width accent bar; a `↓` arrow in the gutter whenever rows hide below (as in keymap-narrow); the foot a bold-key legend naming no body pair; nothing clipped.
- keymap-tab: the Session tab inverse with only Session rows under it, the first `›` on the accent bar; no arrow, everything fits; same columns and legend.
- keymap-search: `session` typed after the muted search line narrows the rows to the five bindings naming a session, across groups; the first `›` on the accent bar; no arrow; same columns and legend.
- keymap-narrow: the same panel at 100 columns, rows wrapped, opened scrolled, with an `↑ N more · ↓ M more` indicator on its last line; the three columns keep their starts.
- quit: the question should read `2 sessions working` muted under a bold accent `Quit`; `Enter` marked `›` with its key bold on a full-width accent bar and no `· default`; the foot sits left with the body and names no choice.
- delete: the question should name `docs: rail spec` and its spend, and what `--cascade` would add, under a bold accent title; padding all round; the foot a bold-key legend naming no body pair.
- history: the typed `back` should read bold after `›`, every hit in the three matches marked, the first row `›` on a full-width accent bar; centred with padding; the foot a bold-key legend naming no body pair.
- history-one: one matching prompt: the typed `back` bold after `›`, `1 match · newest first`, that match `›` on a full-width accent bar; the same panel and footer.
- history-none: a typed query matching nothing: the query row, then dim `no matching prompts` in place of the count row, nothing barred; the same panel and footer.
- notice: the whole `key_clash` text should read wrapped to the panel, padded, nothing clipped; the foot a bold-key legend naming no body pair.
- notices: four notices newest first, each a paragraph `code · text` with the code dim, a blank row between paragraphs and none after the last, wrapped to the inner width at the 71-column preference; the title `Notices` bold accent with a dim ✕; the foot `Esc closes`.
- close-mouse: the ✕ should read lighter than its neighbours, the foot should read `click ✕ or outside to close` left-aligned, and a dim dotted outline should mark the click-outside target.
