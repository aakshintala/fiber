# Checking the prototype in Ghostty

tmux cannot show these. Run them in Ghostty itself, not inside tmux, at a window of at least 118 columns.

```sh
cd research/tui-prototype
cargo run --release -- fixtures/session.jsonl --commands /tmp/fiber-cmds.jsonl
```

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
