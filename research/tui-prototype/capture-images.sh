#!/bin/sh
# Captures every --image-case case (#1637) in tmux, plain text and SGR.
# Writes flywheel/run/fiber-1637-captures/<case>.txt and sgr/<case>.ansi,
# all at 160 by 48 as the README says. The title cases also record their
# OSC 2 string under osc/<case>.raw (pane capture drops it: script(1)
# gives the run a pty and a piped q quits it, keeping the raw output).
# Each case draws one static frame and stays up, so the script quits it with
# q after the capture (which also exercises the images quit path).
set -eu
cd "$(dirname "$0")"
OUT=/Users/aakshintala/work/fiber-worktrees/flywheel/run/fiber-1637-captures
BIN=./target/release/tui-prototype
FIX=fixtures/session.jsonl
if [ ! -x "$BIN" ]; then
  CARGO_BUILD_JOBS=3 cargo build --release
fi
mkdir -p "$OUT/sgr" "$OUT/osc"
capture() {
  tmux kill-session -t im1637 2>/dev/null || true
  # shellcheck disable=SC2086
  tmux new-session -d -x $2 -y $3 -s im1637 "$BIN $FIX --static --image-case $1"
  sleep 1
  tmux capture-pane -p -t im1637 >"$OUT/$1.txt"
  tmux capture-pane -e -p -t im1637 >"$OUT/sgr/$1.ansi"
  tmux send-keys -t im1637 q
  sleep 0.3
}
capture tool_image 160 48
capture fallback 160 48
capture pasted 160 48
capture links 160 48
capture screen_reader 160 48
capture title_approval 160 48
capture title_question 160 48
capture title_waiting 160 48
capture title_finished 160 48
tmux kill-session -t im1637 2>/dev/null || true
for c in title_approval title_question title_waiting title_finished; do
  # shellcheck disable=SC2086
  printf 'q' | script -q "$OUT/osc/$c.raw" $BIN $FIX --static --image-case $c >/dev/null 2>&1 || true
done
# The flat replay: --screen-reader --static with no case draws the fixture
# flat instead of the conversation.
tmux kill-session -t im1637 2>/dev/null || true
tmux new-session -d -x 160 -y 48 -s im1637 "$BIN $FIX --static --screen-reader"
sleep 1
tmux capture-pane -p -t im1637 >"$OUT/screen_reader_flat.txt"
tmux capture-pane -e -p -t im1637 >"$OUT/sgr/screen_reader_flat.ansi"
tmux send-keys -t im1637 q
sleep 0.3
tmux kill-session -t im1637 2>/dev/null || true
echo "wrote $OUT"
