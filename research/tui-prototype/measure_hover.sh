#!/bin/sh
# Measures what hover costs inside tmux at 160x48, on the settled idle session.
# Usage: ./measure_hover.sh [runs] [seconds] [rate]   Results: target/measure/hover-<mode>-<n>.tsv
#   still  --hover, no motion
#   sweep  --hover, motion reports sweeping the screen diagonally, so the target changes often
#   one    --hover, motion reports inside one group summary line, so the target never changes
#   off    no --hover (modes 1000, 1002, 1006), the sweep's reports injected anyway
# Motion is sent through one tmux control-mode client, `send-keys -H` per report, at `rate` a second.
set -eu
cd "$(dirname "$0")"
cargo build --release -q
runs=${1:-5}
secs=${2:-20}
rate=${3:-150}
out=target/measure
mkdir -p "$out"
bin="$PWD/target/release/tui-prototype"
inject() {
  python3 - "$1" "$2" "$3" <<'EOF'
import subprocess, sys, time
pattern, secs, rate = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
p = subprocess.Popen(["tmux", "-C", "attach", "-t", "hover"], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
t0 = time.monotonic()
i = 0
try:
    while time.monotonic() - t0 < secs:
        if pattern == "one":
            # the second tool group's summary line, row 16, inside its text
            x, y = 3 + i % 110, 16
        else:
            # a diagonal over the conversation and the panel, one row down per report
            x, y = 1 + (i * 7) % 160, 1 + i % 48
        h = " ".join("%02x" % b for b in b"\x1b[<35;%d;%dM" % (x, y))
        p.stdin.write(("send-keys -t hover -H %s\n" % h).encode())
        p.stdin.flush()
        i += 1
        d = t0 + i / rate - time.monotonic()
        if d > 0:
            time.sleep(d)
    p.stdin.write(b"detach\n")
    p.stdin.flush()
except BrokenPipeError:
    pass
p.wait()
EOF
}
for mode in still sweep one off; do
  case $mode in
    off) args="fixtures/idle.jsonl --static" ;;
    *) args="fixtures/idle.jsonl --static --hover" ;;
  esac
  for i in $(seq "$runs"); do
    f="$PWD/$out/hover-$mode-$i.tsv"
    rm -f "$f"
    tmux new-session -d -s hover -x 160 -y 48 \
      "cd $PWD && $bin $args --stats $f --warmup 2 --exit-after $((secs + 2))"
    sleep 1
    case $mode in
      still) ;;
      one) inject one "$((secs + 1))" "$rate" ;;
      *) inject sweep "$((secs + 1))" "$rate" ;;
    esac
    while tmux has-session -t hover 2>/dev/null; do sleep 1; done
  done
done
# medians
for mode in still sweep one off; do
  echo "== $mode (median of $runs)"
  for k in motions hover_changes_per_s frames fps bytes bytes_per_frame cpu_pct invol_csw idle_wakeups interrupt_wakeups; do
    v=$(grep -h "^$k	" "$out/hover-$mode"-*.tsv | cut -f2 | sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')
    printf '%s\t%s\n' "$k" "$v"
  done
done
