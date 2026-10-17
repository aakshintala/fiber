#!/bin/sh
# Captures every --home still frame (#2424) in tmux at 160 by 48, plain text
# and SGR. Writes flywheel/run/fiber-1657-captures/<case>.txt and
# sgr/<case>.ansi. Each case draws one static frame and stays up, so the
# script quits it with q after the capture (which also exercises the home
# quit path). `live` is not captured: it runs the event loop.
set -eu
cd "$(dirname "$0")"
OUT=/Users/aakshintala/work/fiber-worktrees/flywheel/run/fiber-1657-captures
BIN=./target/release/tui-prototype
FIX=fixtures/session.jsonl
if [ ! -x "$BIN" ]; then
  CARGO_BUILD_JOBS=3 cargo build --release
fi
mkdir -p "$OUT/sgr"
for c in full-sessions full-empty full-focus-exited full-closing full-picker full-remote-empty full-remote-picker rail-focused; do
  tmux kill-session -t home1657 2>/dev/null || true
  # shellcheck disable=SC2086
  tmux new-session -d -x 160 -y 48 -s home1657 "$BIN $FIX --static --home $c"
  sleep 1
  tmux capture-pane -p -t home1657 >"$OUT/$c.txt"
  tmux capture-pane -e -p -t home1657 >"$OUT/sgr/$c.ansi"
  tmux send-keys -t home1657 q
  sleep 0.3
done
tmux kill-session -t home1657 2>/dev/null || true
echo "wrote $OUT"
