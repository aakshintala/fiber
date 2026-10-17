# Home (#2424)

One run per case: `--home CASE`.

- full-sessions: the four-row logo top left with `0.0.1` and the coloured counts beside it; one section per project, launch project first, each headed `name +`, dim `N live · $spend` and orange `N waiting on you`; two-line live rows with `⌥N`, the glyph, the name, the state word and the elapsed, then dim `id · turns · ⎇ branch · spend · context%` with the full wait in orange; dim `exited · N`, up to three dim `○` rows and `… N more · /resume lists all`; the first live row on the selection bar; the prompt box at the bottom and the key hint at the foot.
- full-empty: the header's second line dim `no sessions yet`; the middle dim `No sessions yet.` and `Type below and press Enter to start one.`, centred; the prompt box and the hint as usual, no sections.
- full-focus-exited: the first exited row on the selection bar; live rows unmarked.
- full-closing: the first live row with dim `CLOSING` where the state word sits, its other cells as a live row.
- full-picker: the picker centred over the fleet with `Workspaces` bold accent; the typed row `› ~/work/fiber/█` with the launch directory's listing, the first `›` on the bar; a blank row, dim `── recents ──` and the four recents dim, the launch directory first; the bold-key legend foot with `Esc closes`.
- full-remote-empty: the dim `▣ no workspace` chip; the open `Workspaces` picker with the typed row `› ~/█` and the hub's home listing, the first `›` on the bar; no divider and no recent rows; the footer `↑↓ move · Enter open`, without `Esc closes`.
- full-remote-picker: the dim `▣ no workspace` chip; the open `Workspaces` picker with the typed row `› ~/█` and the hub's home listing, the first `›` on the bar; a blank row, dim `── recents ──` and the hub's four recents dim; the footer `↑↓ move · Enter open`, without `Esc closes`.
- rail-focused: the rail left with the needs-input card lifted across its whole surface; the dimmed conversation behind; the input box with a dim stripe and dim text; the foot hint with bold keys.
- live: not a still frame: `--home` with no case runs the event loop, and this name only names it in `--help`.
