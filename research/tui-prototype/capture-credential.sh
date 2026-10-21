#!/bin/sh
# Capture the three static credential looks at 160 by 48, in text and SGR.
set -eu
cd "$(dirname "$0")"
OUT=${OUT:-/home/aakshintala/work/fiber-worktrees/flywheel/run/fiber-724-captures}
CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 cargo build --release
mkdir -p "$OUT/sgr"
SESSION="credential724-$$"
trap 'tmux kill-session -t "$SESSION" 2>/dev/null || true' EXIT HUP INT TERM
for case in credentials credentials-reading credentials-none; do
  tmux new-session -d -x 160 -y 48 -s "$SESSION" "./target/release/tui-prototype fixtures/session.jsonl --static --credential-case $case"
  python3 - "$SESSION" "$OUT" "$case" <<'PY'
import pathlib
import subprocess
import sys
import time

session, directory, case = sys.argv[1:]
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    text = subprocess.check_output(["tmux", "capture-pane", "-p", "-t", session])
    if b"Esc close" in text:
        output = pathlib.Path(directory)
        (output / (case + ".txt")).write_bytes(text)
        sgr = subprocess.check_output(["tmux", "capture-pane", "-e", "-p", "-t", session])
        (output / "sgr" / (case + ".ansi")).write_bytes(sgr)
        break
else:
    raise SystemExit("credential capture timed out waiting for the footer: " + case)
PY
  tmux send-keys -t "$SESSION" q
  tmux kill-session -t "$SESSION" 2>/dev/null || true
done
printf 'wrote %s\n' "$OUT"
