#!/bin/sh
# Measures the Lua ledger-row renderer (#163, SEAMS.md) on the heavy fixture:
# built-in rows only, the Lua renderer cached by content and width, and the
# Lua renderer called for every visible row on every frame. macOS arm64,
# inside tmux at 160x48, median of N runs (default 5) over S-second windows
# (default 10).
#
# Usage: ./measure_seams.sh [runs] [secs]
# Results: target/measure-seams/*.tsv and *.mem; tables printed at the end.
set -eu
cd "$(dirname "$0")"
cargo build --release -q
runs=${1:-5}
secs=${2:-10}
warm=2
exitafter=$((warm + secs))
out=target/measure-seams
bin="$PWD/target/release/tui-prototype"
fx=fixtures/large-heavy.jsonl
rm -rf "$out"
mkdir -p "$out"
SESS="ls-$$"
# wheel up, SGR form, column 40 row 10: ESC [ < 64 ; 40 ; 10 M
WHEEL_UP="1b 5b 3c 36 34 3b 34 30 3b 31 30 4d"

wait_done() {
  while tmux has-session -t "$SESS" 2>/dev/null; do sleep 0.3; done
}
flags() {
  case $1 in
    builtin) echo "" ;;
    cached) echo "--lua-renderer lua/shell_row.lua" ;;
    uncached) echo "--lua-renderer lua/shell_row.lua --lua-uncached" ;;
  esac
}
CONFIGS="builtin cached uncached"

for c in $CONFIGS; do
  fl=$(flags "$c")
  # load: --static, just long enough for the first frame
  for i in $(seq "$runs"); do
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin $fx --static $fl --stats $out/$c-load-$i.tsv --exit-after 1"
    wait_done
  done
  # scrolling with every ledger open: Ctrl+O before the window, then wheel-up
  # at 10 Hz; /usr/bin/time -l gives the peak memory footprint of the same run
  for i in $(seq "$runs"); do
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && /usr/bin/time -l $bin $fx --static $fl --stats $out/$c-scroll-$i.tsv --warmup $warm --exit-after $exitafter 2>$out/$c-scroll-$i.mem"
    sleep 1
    tmux send-keys -t "$SESS" C-o
    sleep 1
    end=$(($(date +%s) + secs))
    while [ "$(date +%s)" -lt "$end" ]; do
      tmux send-keys -t "$SESS" -H $WHEEL_UP 2>/dev/null || break
      sleep 0.1
    done
    wait_done
  done
  # searching with every ledger open: Ctrl+O, then Ctrl+F "tool" and Enter
  # every 0.2 s, as measure_large.sh does
  for i in $(seq "$runs"); do
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin $fx --static $fl --stats $out/$c-search-$i.tsv --warmup $warm --exit-after $exitafter"
    sleep 1
    tmux send-keys -t "$SESS" C-o
    sleep 1
    tmux send-keys -t "$SESS" C-f 2>/dev/null || true
    tmux send-keys -t "$SESS" "tool" 2>/dev/null || true
    end=$(($(date +%s) + secs))
    while [ "$(date +%s)" -lt "$end" ]; do
      tmux send-keys -t "$SESS" Enter 2>/dev/null || break
      sleep 0.2
    done
    wait_done
  done
  # live replay at the default 12x speed: every event rebuilds the conversation
  for i in $(seq "$runs"); do
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && $bin $fx $fl --stats $out/$c-replay-$i.tsv --warmup $warm --exit-after $exitafter"
    wait_done
  done
done

med() {
  # median of the values of key $2 in files $1
  grep -h "^$2	" $1 2>/dev/null | cut -f2 | sort -n | awk '{a[NR]=$1} END {if (NR>0) print a[int((NR+1)/2)]; else print "n/a"}'
}
memmed() {
  for f in $1; do awk -v k="$2" '$0 ~ k {print $1}' "$f"; done | sort -n | awk '{a[NR]=$1} END {if (NR>0) printf "%.2f MiB\n", a[int((NR+1)/2)]/1048576; else print "n/a"}'
}
for c in $CONFIGS; do
  echo "== $c"
  echo "load ms (--static, first frame)  $(med "$out/$c-load-*.tsv" first_frame_ms)"
  echo "peak footprint (scroll run)      $(memmed "$out/$c-scroll-*.mem" "peak memory footprint")"
  echo "cpu % scrolling                  $(med "$out/$c-scroll-*.tsv" cpu_pct)  fps $(med "$out/$c-scroll-*.tsv" fps)"
  echo "cpu % searching                  $(med "$out/$c-search-*.tsv" cpu_pct)  fps $(med "$out/$c-search-*.tsv" fps)"
  echo "cpu % live replay                $(med "$out/$c-replay-*.tsv" cpu_pct)  fps $(med "$out/$c-replay-*.tsv" fps)"
  echo "lua calls in replay window       $(med "$out/$c-replay-*.tsv" lua_calls_window)  us/call $(med "$out/$c-replay-*.tsv" lua_us_per_call)"
  echo "lua calls at load               $(med "$out/$c-load-*.tsv" lua_calls)  us/call $(med "$out/$c-load-*.tsv" lua_us_per_call)  in render $(med "$out/$c-load-*.tsv" lua_us_in_render)"
  echo "lua calls in scroll window       $(med "$out/$c-scroll-*.tsv" lua_calls_window)  us/call $(med "$out/$c-scroll-*.tsv" lua_us_per_call)"
  echo "lua calls in search window       $(med "$out/$c-search-*.tsv" lua_calls_window)  us/call $(med "$out/$c-search-*.tsv" lua_us_per_call)"
  echo "lua state after load             $(med "$out/$c-load-*.tsv" lua_mem_bytes) bytes"
  echo
done
echo "Raw files are in $out"
