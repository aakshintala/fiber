# Model picker (#1629)

One run per case: `--picker CASE`.

- list: three providers with a dozen models between them, roles on the rows, `● current` on claude-opus-5-5, its level chips on the row below; every row with its input and output price dim after the name and its rebuild cost right-aligned labelled `switch …`, the current model keeping `—`; a centred panel with ▄ ▀ edges, dim providers with a blank row between sections, the focused model `›` on a full-width accent bar, and a bold-key legend foot.
- levels: the current model's thinking chips focused (`[high]`), the rest dim; same panel, bar and legend.
- scoped: five models only, a `scoped · 5 of 12` chip and a `[show all]` toggle; same panel, bar and legend.
- scoped-all: all twelve models, the scoped five marked `· scoped`; same panel, bar and legend.
- refreshing: openai-codex reads `⟳ refreshing` with a still spinner glyph, the other two `updated … ago`, and a `⟳ refresh all` button sits at the controls row's right end; same panel, bar and legend.
- session-only: claude-sonnet-5-5 `›` on the accent bar with `ⓢ this session only · nothing saved` under it, its prices and its labelled switch cost on its row; same panel and legend.
- filtered: the query `mini` in bold after `›` with a block cursor, four models across openai-codex and google, the matched id chars underlined and bold on and off the accent bar, a `4 of 12 models` chip; same panel and legend.
- filtered-empty: the query `zzz` in bold after `›` with a block cursor, one muted `No models match` line and no provider sections, a `0 of 12 models` chip; same panel and legend.
- checklist: all twelve models, each with a `[x]` or `[ ]` mark before its id, five marked, a ` 5 of 12 marked ` count row and no show-all toggle; same panel, bar and legend.
- checklist-filtered: the query `mini` in bold after `›` with a block cursor, four models each with its mark, a ` 4 of 12 models · 5 marked ` count row; same panel, bar and legend.
- checklist-empty: the query `zzz` in bold after `›` with a block cursor, one muted `No models match` line and no provider sections; same panel and legend.
- prices: every row with its input and output price per million dim after the name, the rebuild cost right-aligned labelled `switch …`, the current model keeping `—`, and the tiered row at its base rates below its tier; same panel, bar and legend.
- prices-empty: every row with its prices but no switch cost anywhere, the tiered row at its base rates; same panel, bar and legend.
- prices-free: gemma-3-27b `›` on the accent bar reading `free` for its prices, the rest priced; same panel, bar and legend.
- prices-noprice: gpt-6-nano `›` on the accent bar with no price text, only its labelled switch cost, the rest priced; same panel, bar and legend.
- prices-tiered: claude-sonnet-5-5 `›` on the accent bar at its tiered rates with the `tier 200k+` mark, the rest at their own rates; same panel, bar and legend.
