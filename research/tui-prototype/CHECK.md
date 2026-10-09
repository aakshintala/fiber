# Checking the prototype in Ghostty

tmux cannot show these. Run them in Ghostty itself, not inside tmux, at a window of at least 118 columns.

```sh
cd research/tui-prototype
cargo run --release -- fixtures/session.jsonl --commands /tmp/fiber-cmds.jsonl
```

`./check-wizard.sh` walks through these one run at a time, makes the Ghostty config changes two of them need and removes them afterwards, and writes each result to `/tmp/fiber-tui-check-results.env`.

Each line is one check: what to do, then what to look for.

- Stripe: look at the ▌ on the input box and the approval panel, and the ▐ on your prompts. Each should be one unbroken bar, with no gap between rows.
- Surface edges: look at any turn card or panel card. The ▄ and ▀ edges should meet the tint with no seam or gap.
- Faint text: look at a tool group's summary line and the ledger (Ctrl+O). It should read as clearly dimmer than the replies but still be legible at your `faint-opacity`.
- Glimmer: watch "Working" while the last turn runs. The band should sweep smoothly left to right and rest, without flicker elsewhere on screen.
- Reduced motion: run again with `--reduced-motion`. The word should stay still and only the elapsed seconds change.
- Keyboard detection: look at the Session card's "keys" line after start. It should say "kitty · N ms" in Ghostty; note N.
- Shift+Enter: press Ctrl+F, type `lock`, then press Enter twice and Shift+Enter once. The count should go 1, 2, 3, then back to 2.
- Cmd+F: add `keybind = super+f=unbind` to Ghostty's config and reload, then press Cmd+F. The search bar should open; without the line, Ghostty's own search opens instead.
- Search marks: search `wait_for_path`. Every match should be marked, the current one brighter, and collapsed groups with a match should open.
- Selection: drag across a wrapped paragraph of a reply. The highlight should follow the mouse, and should not spill into the side panel.
- Auto-scroll: drag from the middle of the conversation up past its top edge and hold still. The view should keep scrolling while the selection grows.
- Copy: release after a drag, then paste into another app. The text should come back unwrapped, with no margins, stripes or ▄ ▀ edges.
- OSC 52 alone: run over SSH, or with `pbcopy` moved out of PATH, and copy again. The paste should still work, through Ghostty's OSC 52 (Ghostty may ask to allow clipboard access first).
- Shift-drag: hold Shift and drag. Ghostty's own selection should take over, including the panel.
- Links: scroll to the first reply and Cmd+click `crates/log/tests/lock.rs` and the GitHub URL. Ghostty should underline each on hover and open it; note whether a plain click or Cmd+Shift+click is needed with mouse capture on.
- ⌥ keys: with a queued steering message showing, press ⌥↑, then ⌥X. The message should load into the input box, then drop. Try once with Ghostty's `macos-option-as-alt` off (the default) and once with it on, and note which works.
- Esc on an approval: press Esc at start. The approval should step aside behind "1 approval waiting", and the form should come up; click the row to bring the approval back.
- Esc in the form: press Esc on the form. It should send `reply` declined then `cancel` (shown above the input box and in `/tmp/fiber-cmds.jsonl`), end the turn "interrupted", and show "declined" under "you answered".
- Resize: drag the window narrower than 118 columns and back. The panel should give way to two status rows and return, with no garbage left behind.

Stage 3, again in Ghostty itself. `./check-wizard.sh --stage3` runs only these.

- Trackpad log: run with `--log-input /tmp/fiber-tui-input.log`, scroll up a little on the trackpad, and quit. The log should show button 64 events mixed with 66 and 67 (sideways), which the prototype now ignores. Keep the file for Claude.
- Trackpad, small movements: scroll up and down a little at a time, slowly and quickly. The view should move one way only, with the finger, and never bounce back.
- Trackpad, wide movements and a flick: the view should move smoothly, and nothing on screen should flicker, scrolling up or down.
- Mouse wheel, if you have one: one notch should move about three rows.
- Summary line: run without `--static` and watch the running group's line while calls start and finish. It should stay one row, and nothing above it should move.
- Search box: type some text in the input box, then press Ctrl+F and type `lock`. The box should float over the conversation's top-right corner with even ▄ ▀ edges and show "⌕ lock" and "1 of N". The input box should keep your text.
- Jump overlay: scroll up. A pill, " ↓ N lines below · End ", should sit centred at the bottom of the conversation. Clicking it should jump to the end and the pill should go. Nothing should appear in the input box.
- Context view: scroll up a little, then type `/context` and press Enter. The breakdown should replace the conversation, with the panel and the input box still there. Press Esc: the conversation should be back at exactly the same place. Then click the Session card's context bar: the view should open again.

The rail (#692), at a window of at least 150 columns so rail, conversation and panel all fit.

- Variants: B is the default (three cards); click the chips in the rail's bottom label to compare (`medium` for the bar row, `compact` for flat, `full` for the 5-row card), and the `A` and `C` chips for the other designs. Each design should carry its highlighted chip, A a 22-column list, B rich cards grouped by project in start order, C one tab row with its own chips.
- Cards (three): number, glyph, state word on the left; coarse elapsed, spend and coloured percentage right-aligned; no bar. Bold name; wait reason or branch. States across variants: spinning WORKING (blue), orange RETRYING (pi-rig's second card), `!` NEEDS INPUT, `✓` READY (was IDLE), `✗` CRASHED (was STOPPED, the lsp card) with a red ✕ at row 1's right end. Headers carry a `+` chip (starts a session there) and clickable "N done" (opens the project's list). Clicking the ✕ should dismiss the card leaving its number's gap; clicking the card should show a "→ would send resume" row and turn it WORKING. Narrow the window until the rail is under 30 columns: the bar should go, keeping spend and percentage.
- Width: drag the rail's right edge and the panel's left edge, marked by a dim ⋮ grip. Each should follow the pointer as a share, show it while dragging ("rail 18% · 31 cols"), brighten its handle with a col-resize pointer on hover or drag, and keep it across a terminal resize; the conversation should never go under its minimum. `--rail-share` and `--panel-share` should set the start. Drag the rail below 22 columns: it should hide completely, leaving a 1-column ⋮ handle; dragging the handle out should restore it. Press ⌥R: the rail should hide and return.
- Waiting: watch a `!` glyph. It and its stripe should pulse slowly between two colours, and stay still, bold, under `--reduced-motion`.
- ⌥N: with Ghostty's `macos-option-as-alt` on, press ⌥3. The on-screen tint should move to the third session in start order while the conversation stays the replayed session, scrolling its card into view. Press ⌥A: the marker should jump to the oldest waiting session. Elapsed times should read coarse and single-unit (`2m`, not `2m 23s`).
- Hidden: with the rail hidden, "N waiting" should join the status rows (narrow) or the panel's Session card (clicking it brings the rail back).
- Scope: in A, click "+1 other · show all". The long-named pi-rig session should appear under an "other projects" divider, truncated with …, and the row should offer "show less". B shows every project with no scope line.
- Hover: run with `--hover` and hover a B card. The whole card should brighten; in compact a dim footer at the rail's bottom should name the full session, workspace, model and spend. In A one tooltip line should show the full name, workspace and spend.

Home (#1628), one `cargo run --release -- fixtures/session.jsonl --static --home <case>` per case, in Ghostty itself at 160 by 48.

- empty: the logo should read as pixel letters four rows tall (⌇ in accent, the name in the accent gradient, `0.0.1` dim on the last row); under it the large input box with `/? for shortcuts`, the chip row and `enter starts a session`; under the box one dim `No sessions yet` line; the key hint at the foot.
- sessions: six exited rows, each `○ name · spend`, the three outside the launch project with their workspace's last segment; the long pi-rig name should fit without pushing the spend off the row.
- hover-workspace, hover-model, hover-thinking: the one chip should sit lighter than its neighbours while keeping its own text colour; hover-worktree: the switch chip lighter with its ● still blue.
- worktree-on: the switch should read `[● new worktree]` in blue; worktree-off: `[○ new worktree]` dim.
- picker-recent: the picker should float over home with even ▄ ▀ edges, four recent workspaces, the first row marked with ▌ on the lighter tint.
- picker-typed: the typed row should read `› ~/work/fi█` with `fiber` and `fiber-worktrees` under it, the first marked; the recents below dimmed.
