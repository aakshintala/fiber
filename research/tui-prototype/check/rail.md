# Rail cards (#1632)

One run per case: `--rail-case CASE`.

- all-states: two project groups, the launch project first, in start order; six cards reading WORKING, NEEDS INPUT with its orange wait reason, RETRYING, CRASHED with the red ✕, READY and IDLE; the on-screen card brighter; every row 1 keeps its state word and percentage while the elapsed and spend give way to the 24 columns.
- hovered: the needs-input card brightened across its whole surface, visible in SGR only; its text reads the same as the unhovered rail.
- context-bars: 30%, 65% and 90% each drawn twice, once with the panel's ▆ track and once with the ░ track; the fills blue, orange and red; pick one.
- floor-ceiling: the 22-column rail keeps number, glyph, state word, name, branch and spend-plus-percentage with no bar; the 48-column rail shows the full card with its bar; nothing clipped on either.
- hidden: a 1-column handle with the dim ⋮ grip at the screen's left edge, and the Session card ending `! 2 waiting · rail hidden …` in orange: the `· show` hint clips at the default panel width, as on screen.
