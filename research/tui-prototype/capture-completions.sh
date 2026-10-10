#!/bin/sh
# Captures every --completions case (#1631) in tmux, plain text and SGR.
# Writes flywheel/run/fiber-1631-captures/<case>.txt and sgr/<case>.ansi.
# narrow-slash and narrow-at run at 100 by 40, as their checks say; the rest
# at 160 by 48. Each case starts with its query already typed and stays up,
# so the script kills the session after the capture.
set -eu
cd "$(dirname "$0")"
OUT=/Users/aakshintala/work/fiber-worktrees/flywheel/run/fiber-1631-captures
BIN=./target/release/tui-prototype
FIX=fixtures/session.jsonl
if [ ! -x "$BIN" ]; then
  CARGO_BUILD_JOBS=3 cargo build --release
fi
mkdir -p "$OUT/sgr"
capture() {
  tmux kill-session -t comp1631 2>/dev/null || true
  # shellcheck disable=SC2086
  tmux new-session -d -x $2 -y $3 -s comp1631 "$BIN $FIX --static --completions $1"
  sleep 1
  tmux capture-pane -p -t comp1631 >"$OUT/$1.txt"
  tmux capture-pane -e -p -t comp1631 >"$OUT/sgr/$1.ansi"
  tmux kill-session -t comp1631 2>/dev/null || true
}
capture slash 160 48
capture slash-filtered 160 48
capture slash-hint 160 48
capture at 160 48
capture at-empty 160 48
capture narrow-slash 100 40
capture narrow-at 100 40
echo "wrote $OUT"
