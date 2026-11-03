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
| iTerm2 | line_start | `iterm2_line_start.bin` (`\x01`) | Ctrl+A: the readline `beginning-of-line (C-a)` default (GNU Bash manual, Commands For Moving); `001 SOH` per `man ascii(7)`; passes through unmapped (factory global keymap has no plain Ctrl+letter entries) | unknown (iTerm2 docs and app state no single version) |
| iTerm2 | line_end | `iterm2_line_end.bin` (`\x05`) | Ctrl+E: the readline `end-of-line (C-e)` default (GNU Bash manual, Commands For Moving); `005 ENQ` per `man ascii(7)`; passes through unmapped (same keymap) | unknown (iTerm2 docs and app state no single version) |
| iTerm2 | delete_to_line_start | `iterm2_delete_to_line_start.bin` (`\x15`) | Ctrl+U: the readline `unix-line-discard (C-u)` default (GNU Bash manual, Commands For Killing); `025 NAK` per `man ascii(7)`; passes through unmapped (same keymap) | unknown (iTerm2 docs and app state no single version) |
| iTerm2 | delete_word | `iterm2_delete_word.bin` (`\x17`) | Ctrl+W: the readline `unix-word-rubout (C-w)` default (GNU Bash manual, Commands For Killing); `027 ETB` per `man ascii(7)`; Option+Backspace is not `ESC \x7f` by default (factory option acts as Normal; `+Esc` is opt-in per the Keys docs), so the default word key is Ctrl+W | unknown (iTerm2 docs and app state no single version) |
| Terminal.app | line_start | `terminal_app_line_start.bin` (`\x01`) | Ctrl+A: the readline `beginning-of-line (C-a)` default (GNU Bash manual, Commands For Moving); `001 SOH` per `man ascii(7)`; Terminal.app declares `xterm-256color` by default and forwards unmapped control keys as C0 bytes | unknown (Apple guide spans macOS releases) |
| Terminal.app | line_end | `terminal_app_line_end.bin` (`\x05`) | Ctrl+E: the readline `end-of-line (C-e)` default (GNU Bash manual, Commands For Moving); `005 ENQ` per `man ascii(7)`; same declaration and forwarding | unknown (Apple guide spans macOS releases) |
| Terminal.app | delete_to_line_start | `terminal_app_delete_to_line_start.bin` (`\x15`) | Ctrl+U: Apple's Terminal User Guide ("Correct typing errors") says "Erase the line: Press Control-U"; `025 NAK` per `man ascii(7)`; same declaration and forwarding | unknown (Apple guide spans macOS releases) |
| Terminal.app | delete_word | `terminal_app_delete_word.bin` (`\x17`) | Ctrl+W: the readline `unix-word-rubout (C-w)` default (GNU Bash manual, Commands For Killing); `027 ETB` per `man ascii(7)`; "Use Option as Meta key" is off by default, so Option+Backspace is not `ESC \x7f` and the default word key is Ctrl+W | unknown (Apple guide spans macOS releases) |
| Linux | line_start | `linux_line_start.bin` (`\x01`) | Ctrl+A: the readline `beginning-of-line (C-a)` default (GNU Bash manual, Commands For Moving); `001 SOH` per `man ascii(7)` (Linux man-pages 6.19); xterm forwards 7-bit C0 controls by default | Linux man-pages 6.19 (2025-10-05) for the C0 table; readline and xterm docs state no single version |
| Linux | line_end | `linux_line_end.bin` (`\x05`) | Ctrl+E: the readline `end-of-line (C-e)` default (GNU Bash manual, Commands For Moving); `005 ENQ` per `man ascii(7)` (Linux man-pages 6.19); same forwarding | Linux man-pages 6.19 (2025-10-05) for the C0 table; readline and xterm docs state no single version |
| Linux | delete_to_line_start | `linux_delete_to_line_start.bin` (`\x15`) | Ctrl+U: the readline `unix-line-discard (C-u)` default (GNU Bash manual, Commands For Killing); `025 NAK` per `man ascii(7)` (Linux man-pages 6.19); same forwarding | Linux man-pages 6.19 (2025-10-05) for the C0 table; readline and xterm docs state no single version |
| Linux | delete_word | `linux_delete_word.bin` (`\x17`) | Ctrl+W: the readline `unix-word-rubout (C-w)` default (GNU Bash manual, Commands For Killing); `027 ETB` per `man ascii(7)` (Linux man-pages 6.19); same forwarding | Linux man-pages 6.19 (2025-10-05) for the C0 table; readline and xterm docs state no single version |

Omitted: Ghostty `delete_word` has no entry. #3754 records only
`super+backspace=text:\x15`; its Option+Backspace step records no bytes.
Ghostty >= 1.2 sends Option+Backspace as plain `0x7f` (`text:\x7f`) by
default (PR #7320, discussion #9560), and `ESC 0x7f`
(`text:\x1b\x7f`) is a user keybind; 1.2 removed the builtin
opt+backspace word delete (discussion #8677: only one character deleted
after). No Ghostty default states `ESC \x7f`, so no fixture.

## Source notes

- iTerm2 factory key handling (read 2026-11-03 at `gnachman/iTerm2`
  `89f43db4`): the Keys profile docs say selecting `+Esc` for the option
  keys is the recommended opt-in for meta keys, so factory builds send no
  `ESC`-prefixed Alt bytes; `plists/DefaultGlobalKeyMap.plist` (14 factory
  entries binding arrows, Home/End, PageUp/PageDown, Tab and Ctrl+Shift+Y to
  app actions) holds no plain Ctrl+letter entry, so Ctrl+A/Ctrl+E/Ctrl+U/Ctrl+W
  pass through; `sources/Keyboard/iTermStandardKeyMapper.m` maps control keys to
  their C0 codes (e.g. Control-backspace to `^H`, 0x08) and passes
  Normal-option keys through unchanged.
- Terminal.app: `Declare terminal as` defaults to `xterm-256color` (the Lion
  default, kept since); Apple's Terminal User Guide documents Control-U as
  the line-erase key; `zsh` (the default macOS shell) and readline keep
  C-a/C-e/C-w as their line/word defaults.
- GNU Bash manual sections Commands For Moving and Commands For Killing
  (read 2026-11-03; the manual states no version on those pages) document
  `beginning-of-line (C-a)`, `end-of-line (C-e)`, `unix-line-discard (C-u)`
  and `unix-word-rubout (C-w)` as the bound-by-default keys.
