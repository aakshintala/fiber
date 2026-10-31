# Testing screens

How a terminal UI screen is tested. It is part of `docs/testing.md`.

## Screens


The terminal UI is tested by feeding a sequence of events to its drawing code
and comparing the whole in-memory screen (ratatui's `TestBackend`) against a
stored snapshot. CI never writes a snapshot: a changed screen fails and shows
the difference, and an accepted change appears in the pull request.

Input is tested the same way: key presses are fed to the TUI's input handling,
and the test asserts the commands it sends to the session.

A few tests run the real binary in a pseudo-terminal, for what memory cannot
show: raw mode, resize, the terminal restored on exit, and seven journeys that
cross features. Two are in `crates/main/tests/terminal.rs`: from a prompt
through an approval, a resize and a quit, and from a turn through a quit, a
resume and a new answer. Four are in `crates/main/tests/journeys.rs`: an image
pasted, answered, closed and resumed; the model picker matched by display name,
switched and answered on the new model; a question form answered, then the
session's second form declined; and an interrupted turn followed by a turn
whose failed model call Fiber retries. The journey in
`crates/main/tests/inline_images.rs` scrolls and clicks an inline PNG, opens
the viewer, and asserts one transmit and place before a session-change delete,
with no later image writes. The 255-read journey past the placeholder range,
whose past-range caption click opens its viewer with no placeholder grid above
it, runs in `crates/main/tests/main_journeys.rs` after merge.

The journeys in `journeys.rs` snapshot the settled grid with `insta` at named
moments, as its text rows and cursor state. The cursor line holds the
coordinates when the cursor is visible and `cursor=hidden` otherwise, because a
hidden cursor's parked coordinate is not user-visible and varies between
identical frames. They never snapshot colour, which stays with the in-process
snapshots and the look tests. Their panel shows no cards, so no panel figure or
temp path reaches a snapshot. Before comparing, filters replace session ids,
clock times, the working line's spinner and elapsed time, closing-row
durations, tool-group spinner and duration, and provider-heading freshness
status, so no unfiltered live value reaches a snapshot. The snapshots live in
`crates/main/tests/snapshots/`, and CI never writes one.

The binary-level tests in `crates/main/tests` that assert on the terminal
UI's screen use one driver, `crates/main/tests/support/pty.rs`. It feeds
the bytes it reads into a `vt100` parser, which rebuilds the screen as a
grid, and the test asserts on that grid: its text, its rows, each cell's
colours and attributes, the cursor position, and the alternate-screen and
hidden-cursor flags. Each wait is a predicate over the grid with one named
deadline; a wait the terminal ends before failing shows the last grid.
The driver takes a grid snapshot for a wait only when no synchronized
output block (mode 2026) is open, so a wait sees whole frames and never a
mix of the old frame and the new one; a stream without mode 2026 snapshots
after each read.
The terminal is 120 by 32 unless a test picks the size its layout needs
(the look tests use 160 by 48). The driver pins `TERM` to
`xterm-256color` and answers the binary's capability queries for a fixed
dark terminal.

`scripts/look-diff` compares one look test's final screen to a prototype
capture: it runs the named look test with a switch that dumps the output
through the last grid wait the test completes, replays the dump through the same `vt100`
parser into a grid, and compares the grid to the capture row by row with
trailing spaces trimmed, plus each cell's colours and attributes when
the `.ansi` capture is given. It prints one verdict line: `look: match
<test>`, `look: differs <test> rows=<n> cells=<m> diff=<path>`, or
`look: error <test> <reason>` for a missing capture, a failing test or
a size mismatch.

Bytes that never reach the cells, such as the window title, the OSC 9
desktop notification, a bell and the mode-enable sequences, are waited
for on the raw output instead. Such a wait searches from an offset the
test took before the input that causes the bytes, never from an earlier
match, so it does not depend on the order in which two markers arrive.

A screen predicate checks every cell property its assertions read, so a
partial redraw can't satisfy it. A test sends input only after output the
terminal already emits once it is ready for that input, such as the
mode-enable sequences or the end of the first frame, never after a grid
match alone. That signal is existing output: the shipped binary gets no
test-only switch (`docs/testing.md`, "Levels").

A test reads the grid outside a wait only when the frame is settled: the wait just before it checked every cell property the read asserts, with no input in between, or the test has seen the last output that can change those cells and the grid shows its effect. A text-only wait, a poll, a sleep or a raw-byte marker does not settle a frame. Each such read names its signal in a `// settled:` comment.

The bare screen-call ratchet counts unmarked `.screen()` call lines per file under `crates/main/tests/`, outside `crates/main/tests/support/`. A source line counts when it holds the text `.screen()`, even inside a string literal or a block comment. A line whose trimmed start begins `//` never counts. The literal text `// settled:` on the call line or the line before it exempts the line, even inside a string literal: the count is a conservative upper bound, so over-counting a `.screen()` inside a string, or exempting on marker text inside a string, never lets a real bare read slip past. A `.screen` field access without the call parens is not a call. Each file holds at most its inline maximum in `scripts/check`; a file missing from that table holds none, and each file's maximum is independent.

A pointer event targets a row whose position the test has made final with a
signal, not by matching rows drawn so far. A row that background data can
still move, such as the panel while usage folds in, is clicked only after
the test has seen the session event that carries the last such data, and
the grid shows that event's effect. Adding one more "this row is drawn"
condition to a wait is not a readiness signal.

The driver reads the terminal on a thread from the first frame to end of
file, and takes the markers it waits for over a channel, as `watch` in
`crates/tui/src/pty_watch.rs` does. The pair comes from
`fakes::pty::open`. A reader that stops after a marker
lets the terminal's output queue fill, so the code under test blocks
writing a frame and the test hangs on a wait it caused itself. On quit
the driver reaps the binary, then reads to end of file, so every byte
written before the exit is read.
