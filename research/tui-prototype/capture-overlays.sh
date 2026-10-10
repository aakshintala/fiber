#!/bin/sh
# Captures every --overlay case (#1630) in tmux, plain text and SGR.
# Writes flywheel/run/fiber-1630-captures/<case>.txt and sgr/<case>.ansi.
# keymap-narrow runs at 100 by 40, as the README says; the rest at 160 by 48.
# Each case draws one static frame and stays up, so the script quits it with
# q after the capture (which also exercises the overlay quit path).
set -eu
cd "$(dirname "$0")"
OUT=/Users/aakshintala/work/fiber-worktrees/flywheel/run/fiber-1630-captures
BIN=./target/release/tui-prototype
FIX=fixtures/session.jsonl
if [ ! -x "$BIN" ]; then
  CARGO_BUILD_JOBS=3 cargo build --release
fi
mkdir -p "$OUT/sgr"
capture() {
  tmux kill-session -t ov1630 2>/dev/null || true
  # shellcheck disable=SC2086
  tmux new-session -d -x $2 -y $3 -s ov1630 "$BIN $FIX --static --overlay $1"
  sleep 1
  tmux capture-pane -p -t ov1630 >"$OUT/$1.txt"
  tmux capture-pane -e -p -t ov1630 >"$OUT/sgr/$1.ansi"
  tmux send-keys -t ov1630 q
  sleep 0.3
}
capture keymap 160 48
capture keymap-tab 160 48
capture keymap-search 160 48
capture keymap-narrow 100 40
capture quit 160 48
capture delete 160 48
capture history 160 48
capture notice 160 48
capture close-mouse 160 48
tmux kill-session -t ov1630 2>/dev/null || true
echo "wrote $OUT"
