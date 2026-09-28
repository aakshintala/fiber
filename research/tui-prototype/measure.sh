#!/bin/sh
# Measures the working line's redraw cost inside tmux at 160x48.
# Usage: ./measure.sh [runs] [seconds]   Results: target/measure/<mode>-<n>.tsv
set -eu
cd "$(dirname "$0")"
cargo build --release -q
runs=${1:-5}
secs=${2:-20}
out=target/measure
mkdir -p "$out"
bin="$PWD/target/release/tui-prototype"
for mode in glimmer reduced idle audit; do
  case $mode in
    glimmer) args="fixtures/session.jsonl --static" ;;
    reduced) args="fixtures/session.jsonl --static --reduced-motion" ;;
    idle) args="fixtures/idle.jsonl --static" ;;
    audit) args="fixtures/session.jsonl --static --diff-audit" ;;
  esac
  n=$runs
  [ "$mode" = audit ] && n=1
  for i in $(seq "$n"); do
    f="$PWD/$out/$mode-$i.tsv"
    rm -f "$f"
    tmux new-session -d -s measure -x 160 -y 48 \
      "cd $PWD && $bin $args --stats $f --warmup 2 --exit-after $((secs + 2))"
    while tmux has-session -t measure 2>/dev/null; do sleep 1; done
  done
done
# medians
for mode in glimmer reduced idle; do
  echo "== $mode (median of $runs)"
  for k in fps bytes_per_s bytes_per_frame cpu_pct vol_csw invol_csw idle_wakeups interrupt_wakeups; do
    v=$(grep -h "^$k	" "$out/$mode"-*.tsv | cut -f2 | sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')
    printf '%s\t%s\n' "$k" "$v"
  done
done
echo "== diff audit"
cat "$out/audit-1.tsv"
