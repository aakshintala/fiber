#!/bin/sh
# Captures every --picker case (#1629, #1791) in tmux at 160 by 48, plain text and SGR.
# Writes flywheel/run/fiber-1629-captures/<case>.txt and sgr/<case>.ansi.
# Each case starts with the model picker open and stays up, so the script
# quits it with q after the capture (Esc would only close the picker).
set -eu
cd "$(dirname "$0")"
OUT=/Users/aakshintala/work/fiber-worktrees/flywheel/run/fiber-1629-captures
BIN=./target/release/tui-prototype
FIX=fixtures/session.jsonl
if [ ! -x "$BIN" ]; then
  CARGO_BUILD_JOBS=3 cargo build --release
fi
mkdir -p "$OUT/sgr"
for c in list levels scoped scoped-all refreshing session-only filtered filtered-empty checklist checklist-filtered checklist-empty; do
  tmux kill-session -t pick1629 2>/dev/null || true
  # shellcheck disable=SC2086
  tmux new-session -d -x 160 -y 48 -s pick1629 "$BIN $FIX --static --picker $c"
  sleep 1
  tmux capture-pane -p -t pick1629 >"$OUT/$c.txt"
  tmux capture-pane -e -p -t pick1629 >"$OUT/sgr/$c.ansi"
  tmux kill-session -t pick1629 2>/dev/null || true
done
tmux kill-session -t pick1629 2>/dev/null || true
echo "wrote $OUT"
