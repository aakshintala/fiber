# Terminal byte provenance (#4097)

Each `.bin` holds the exact bytes one terminal sends by default for one
row of the `main_journeys.rs` terminal table, loaded with
`include_bytes!` (compile-time; `docs/ci.md` forbids runtime reads of
repo files). A test feeds one file's bytes in one `Run::write` call.

`unknown` as a version means the cited source states none: the #3751
and #3754 bug reports record Ghostty's bytes without naming a Ghostty
version.

| terminal | row | file | source | version |
| --- | --- | --- | --- | --- |
| Ghostty | line_start | `ghostty_line_start.bin` (`\x01`) | Recorded in #3751: `ghostty +list-keybinds --default` maps `super+arrow_left` to `text:\x01` | unknown |
| Ghostty | line_end | `ghostty_line_end.bin` (`\x05`) | Recorded in #3751: `ghostty +list-keybinds --default` maps `super+arrow_right` to `text:\x05` | unknown |
| Ghostty | delete_to_line_start | `ghostty_delete_to_line_start.bin` (`\x15`) | Recorded in #3754: `ghostty +list-keybinds --default` maps `super+backspace` to `text:\x15` | unknown |
| Ghostty | delete_word | `ghostty_delete_word.bin` (`ESC \x7f`) | #3754's test step presses Option+Backspace; `ESC 0x7f` is Alt+Backspace per `crates/tui/src/keys.rs` (`alt_stroke`) | unknown |
