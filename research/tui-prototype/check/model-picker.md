# Model picker (#1629)

One run per case: `--picker CASE`.

- list: three providers with a dozen models between them, roles on the rows, `● current` on claude-opus-5-5, its level chips on the row below; a centred panel with ▄ ▀ edges and the ▌ stripe, dim providers with a blank row between sections, the focused model `›` on a full-width accent bar with its rebuild cost right-aligned, and a bold-key legend foot.
- levels: the current model's thinking chips focused (`[high]`), the rest dim; same panel, bar and legend.
- scoped: five models only, a `scoped · 5 of 12` chip and a `[show all]` toggle; same panel, bar and legend.
- scoped-all: all twelve models, the scoped five marked `· scoped`; same panel, bar and legend.
- refreshing: openai-codex reads `⟳ refreshing` with a still spinner glyph, the other two `updated … ago`, and a `⟳ refresh all` button sits at the controls row's right end; same panel, bar and legend.
- session-only: claude-sonnet-5-5 `›` on the accent bar with `ⓢ this session only · nothing saved` under it and its rebuild cost on its row; same panel and legend.
- filtered: the query `mini` in bold after `›` with a block cursor, four models across openai-codex and google, the matched id chars underlined and bold on and off the accent bar, a `4 of 12 models` chip; same panel and legend.
- filtered-empty: the query `zzz` in bold after `›` with a block cursor, one muted `No models match` line and no provider sections, a `0 of 12 models` chip; same panel and legend.
