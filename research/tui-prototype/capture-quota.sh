#!/bin/sh
# Captures every --quota-case case (#2526) in tmux, plain text and SGR.
# Writes flywheel/run/fiber-2526-captures/<case>.txt and sgr/<case>.ansi.
# Each case draws one static frame and stays up, so the script quits it with
# q after the capture (which also exercises the quota quit path).
set -eu
cd "$(dirname "$0")"
OUT=/home/aakshintala/work/fiber-worktrees/flywheel/run/fiber-2526-captures
BIN=./target/release/tui-prototype
FIX=fixtures/session.jsonl
if [ ! -x "$BIN" ]; then
  CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 cargo build --release
fi
mkdir -p "$OUT/sgr"
capture() {
  tmux kill-session -t quota2526 2>/dev/null || true
  # shellcheck disable=SC2086
  tmux new-session -d -x $2 -y $3 -s quota2526 "$BIN $FIX --static --quota-case $1"
  sleep 1
  tmux capture-pane -p -t quota2526 >"$OUT/$1.txt"
  tmux capture-pane -e -p -t quota2526 >"$OUT/sgr/$1.ansi"
  tmux send-keys -t quota2526 q
  sleep 0.3
}
capture quota 160 48
capture quota-empty 160 48
tmux kill-session -t quota2526 2>/dev/null || true
echo "wrote $OUT"
