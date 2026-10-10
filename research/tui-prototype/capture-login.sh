#!/bin/sh
# Captures every --login case (#1736) in tmux, plain text and SGR.
# Writes flywheel/run/fiber-1736-captures/<case>.txt and sgr/<case>.ansi.
# waiting-narrow and key-narrow are the same cases at 100 by 40; the rest
# run at 160 by 48. Each case draws one static frame and stays up, so the
# script quits it with q after the capture (which also exercises the login
# quit path).
set -eu
cd "$(dirname "$0")"
OUT=/Users/aakshintala/work/fiber-worktrees/flywheel/run/fiber-1736-captures
BIN=./target/release/tui-prototype
FIX=fixtures/session.jsonl
if [ ! -x "$BIN" ]; then
  CARGO_BUILD_JOBS=3 cargo build --release
fi
mkdir -p "$OUT/sgr"
capture() {
  tmux kill-session -t login1736 2>/dev/null || true
  # shellcheck disable=SC2086
  tmux new-session -d -x $3 -y $4 -s login1736 "$BIN $FIX --static --login $1"
  for _ in $(seq 1 50); do
    if tmux capture-pane -p -t login1736 2>/dev/null | grep -q "Esc "; then
      break
    fi
  done
  tmux capture-pane -p -t login1736 >"$OUT/$2.txt"
  tmux capture-pane -e -p -t login1736 >"$OUT/sgr/$2.ansi"
  tmux send-keys -t login1736 q
}
capture providers providers 160 48
capture waiting waiting 160 48
capture key key 160 48
capture done done 160 48
capture failed failed 160 48
capture waiting waiting-narrow 100 40
capture key key-narrow 100 40
tmux kill-session -t login1736 2>/dev/null || true
echo "wrote $OUT"
