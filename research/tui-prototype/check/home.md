# Home (#1628)

One run per case: `--home CASE`.

- empty: the pixel logo four rows tall with `0.0.1` dim on the last row; under it the large input box with `/? for shortcuts`, the chip row and `enter starts a session`; under the box one dim `No sessions yet` line and no headers; the key hint at the foot.
- sessions: a bold `Live sessions` header with `2 waiting on you` in orange, then five two-line live rows; one blank row, then a bold `Past sessions` header with `/resume lists all`, then six two-line exited rows; line one is the glyph, the name or first prompt, its age and its verb, line two dim `id · turns · branch · spend` with the workspace segment and the wait.
- live-only: the `Live sessions` header and its five two-line rows; no `Past sessions` header.
- past-only: the `Past sessions` header and its six two-line rows; no `Live sessions` header.
- selected: the first live row with the blue `▸` marker, its prompt bold and its verb blue; the other rows unmarked.
- live: not a still frame: `--home` with no case runs the event loop, and this name only names it in `--help`.
- hover-workspace: a still frame of the hover tint: the one chip should sit lighter than its neighbours while keeping its own text colour.
- hover-worktree: a still frame of the hover tint: the switch chip should sit lighter with its ● still blue.
- hover-model: a still frame of the hover tint: the one chip should sit lighter than its neighbours while keeping its own text colour.
- hover-thinking: a still frame of the hover tint: the one chip should sit lighter than its neighbours while keeping its own text colour.
- worktree-on: the switch should read `[● new worktree]` in blue.
- worktree-off: the switch should read `[○ new worktree]` dim.
- picker-recent: the picker should float centred over home with ▄ ▀ edges and the ▌ stripe; `Workspaces` bold accent; four recent workspaces, the first `›` on a full-width accent bar; a bold-key legend foot.
- picker-typed: the typed row should read `› ~/work/fi█` with `fiber` and `fiber-worktrees` under it, the first `›` on the accent bar; the recents below dimmed; same frame and legend foot.
- focus-entry: the entry bar focused, today's look: no chip in the hover lift tint and no row marked with `▸`.
- focus-chip-workspace: the chip row focused: the workspace chip in the hover look (the lift tint with its own text colour, as hover-workspace) and no row marked.
- focus-chip-model: the chip row focused after → twice: the model chip in the hover look (the lift tint keeping its cyan, as hover-model), the other chips untinted.
- focus-chip-picker: workspace chip Enter: the picker-recent frame with chip 0 still in the hover lift tint behind the picker.
- focus-chip-typing: typing after a chip was focused: `fix` in the entry bar, the chip row untinted and no row marked; typing cleared chip focus into the entry bar.
- focus-session: the second ↓: the first live row with the blue `▸` marker as selected, the chips untinted.
