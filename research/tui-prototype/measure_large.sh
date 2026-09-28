#!/bin/sh
# Regenerates the three large session fixtures for #15 (median, p90, heavy —
# see LARGE.md for what each represents) and measures the TUI prototype
# against each: time to first frame, time to load the whole file with
# --static, peak memory (macOS: /usr/bin/time -l), and CPU idle, scrolling,
# searching and during a live replay. macOS arm64, inside tmux at 160x48,
# median of N runs (default 5) over S-second windows (default 10).
#
# Usage: ./measure_large.sh [runs] [secs]
# Results: target/measure-large/*.tsv and *.mem; tables printed at the end.
set -eu
cd "$(dirname "$0")"
cargo build --release -q
runs=${1:-5}
secs=${2:-10}
warm=2
exitafter=$((warm + secs))
out=target/measure-large
bin="$PWD/target/release/tui-prototype"
rm -rf "$out"
mkdir -p "$out"

echo "Regenerating fixtures..."
./target/release/gen_large
echo

SESSIONS="large-median large-p90 large-heavy"
SESS="lm-$$"
# wheel up, SGR form, column 40 row 10 (inside the conversation column):
# ESC [ < 64 ; 40 ; 10 M. tmux send-keys -H takes one hex byte per argument.
WHEEL_UP="1b 5b 3c 36 34 3b 34 30 3b 31 30 4d"

wait_done() {
  while tmux has-session -t "$SESS" 2>/dev/null; do sleep 0.3; done
}

# ---- cold start (time to first frame) and --static load (time to load the
# whole file), both just long enough to capture the first frame
for s in $SESSIONS; do
  for i in $(seq "$runs"); do
    f="$out/$s-cold-$i.tsv"
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin fixtures/$s.jsonl --stats $f --exit-after 1"
    wait_done
  done
  for i in $(seq "$runs"); do
    f="$out/$s-loaded-$i.tsv"
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin fixtures/$s.jsonl --static --stats $f --exit-after 1"
    wait_done
  done
done

# ---- peak memory: --static, idle for the tail of the run, /usr/bin/time -l
for s in $SESSIONS; do
  for i in $(seq "$runs"); do
    f="$out/$s-mem-$i.txt"
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && /usr/bin/time -l $bin fixtures/$s.jsonl --static --exit-after $exitafter >/dev/null 2>$f"
    wait_done
  done
done

# ---- CPU: idle at the end, scrolling, searching, live replay
for s in $SESSIONS; do
  # idle: --static has nothing left to replay, so the whole warmup..exit
  # window is idle time
  for i in $(seq "$runs"); do
    f="$out/$s-idle-$i.tsv"
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin fixtures/$s.jsonl --static --stats $f --warmup $warm --exit-after $exitafter"
    wait_done
  done
  # scrolling: wheel-up bursts at 10 Hz for the measurement window
  for i in $(seq "$runs"); do
    f="$out/$s-scroll-$i.tsv"
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin fixtures/$s.jsonl --static --stats $f --warmup $warm --exit-after $exitafter"
    sleep "$warm"
    end=$(($(date +%s) + secs))
    while [ "$(date +%s)" -lt "$end" ]; do
      tmux send-keys -t "$SESS" -H $WHEEL_UP 2>/dev/null || break
      sleep 0.1
    done
    wait_done
  done
  # searching: Ctrl+F, type a word common in the fixture's filler text, then
  # jump between matches with Enter for the rest of the window
  for i in $(seq "$runs"); do
    f="$out/$s-search-$i.tsv"
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin fixtures/$s.jsonl --static --stats $f --warmup $warm --exit-after $exitafter"
    sleep "$warm"
    tmux send-keys -t "$SESS" C-f 2>/dev/null || true
    tmux send-keys -t "$SESS" "tool" 2>/dev/null || true
    end=$(($(date +%s) + secs))
    while [ "$(date +%s)" -lt "$end" ]; do
      tmux send-keys -t "$SESS" Enter 2>/dev/null || break
      sleep 0.2
    done
    wait_done
  done
  # live replay: default speed (12x), not --static
  for i in $(seq "$runs"); do
    f="$out/$s-replay-$i.tsv"
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin fixtures/$s.jsonl --stats $f --warmup $warm --exit-after $exitafter"
    wait_done
  done
done

# ---- tables
median_of() {
  # median of the values in column 2 of every "$1: key<TAB>value" match
  grep -h "^$2	" $1 2>/dev/null | cut -f2 | sort -n | awk '{a[NR]=$1} END {if (NR>0) print a[int((NR+1)/2)]; else print "n/a"}'
}
mem_median() {
  # $1: file glob, $2: field name ("maximum resident set size" or "peak memory footprint")
  for f in $1; do awk -v k="$2" '$0 ~ k {print $1}' "$f"; done | sort -n | awk '{a[NR]=$1} END {if (NR>0) print a[int((NR+1)/2)]; else print "n/a"}'
}

echo "== Time to first frame (ms), cold start, no --static"
for s in $SESSIONS; do
  echo "$s: $(median_of "$out/$s-cold-*.tsv" first_frame_ms)"
done

echo
echo "== Time to load the whole file (ms), --static (first_frame_ms includes the full fold)"
for s in $SESSIONS; do
  echo "$s: $(median_of "$out/$s-loaded-*.tsv" first_frame_ms)"
done

echo
echo "== Peak memory, --static, idle ${secs}s at the end (bytes)"
for s in $SESSIONS; do
  rss=$(mem_median "$out/$s-mem-*.txt" "maximum resident set size")
  fp=$(mem_median "$out/$s-mem-*.txt" "peak memory footprint")
  echo "$s: max RSS $rss  peak footprint $fp"
done

echo
for mode_label in "idle	idle" "scroll	scrolling (10 Hz wheel)" "search	searching (Ctrl+F, Enter every 0.2s)" "replay	live replay, default speed"; do
  mode=$(echo "$mode_label" | cut -f1)
  label=$(echo "$mode_label" | cut -f2)
  echo "== CPU while $label (median of $runs, ${secs}s window)"
  for s in $SESSIONS; do
    cpu=$(median_of "$out/$s-$mode-*.tsv" cpu_pct)
    fps=$(median_of "$out/$s-$mode-*.tsv" fps)
    csw=$(median_of "$out/$s-$mode-*.tsv" invol_csw)
    echo "$s: cpu_pct $cpu  fps $fps  invol_csw $csw"
  done
  echo
done

echo "Raw .tsv/.txt files are in $out"
