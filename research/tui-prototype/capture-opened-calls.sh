#!/bin/sh
# Captures the opened-call cases (#2240) in tmux, plain text and SGR.
# Usage: OUT=<dir> ./capture-opened-calls.sh
# Writes <dir>/<case>-160x48.txt and <dir>/<case>-100x40.txt with sgr/
# beside them, every case at both sizes as the README says. Each case draws
# one static frame and stays up, so the script quits it with q after the
# capture (which also exercises the views quit path).
set -eu
cd "$(dirname "$0")"
OUT=${OUT:-/tmp/fiber-2240-captures}
BIN=./target/release/tui-prototype
FIX=fixtures/session.jsonl
if [ ! -x "$BIN" ]; then
  CARGO_BUILD_JOBS=3 cargo build --release
fi
mkdir -p "$OUT/sgr"
capture() {
  tmux kill-session -t oc2240 2>/dev/null || true
  # shellcheck disable=SC2086
  tmux new-session -d -x $2 -y $3 -s oc2240 "$BIN $FIX --static --view-case $1"
  sleep 1
  tmux capture-pane -p -t oc2240 >"$OUT/$1-$2x$3.txt"
  tmux capture-pane -e -p -t oc2240 >"$OUT/sgr/$1-$2x$3.ansi"
  tmux send-keys -t oc2240 q
  sleep 0.3
}
for c in opened-edit opened-read opened-search opened-shell opened-mcp cut-shell changed-files; do
  capture "$c" 160 48
  capture "$c" 100 40
done
tmux kill-session -t oc2240 2>/dev/null || true
echo "wrote $OUT"
