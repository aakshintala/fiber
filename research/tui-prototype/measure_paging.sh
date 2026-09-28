#!/bin/sh
# Measures paged history (--paged) against the whole-file mode for #15, on the
# median, p90 and heavy fixtures: see PAGING.md. macOS arm64, inside tmux at
# 160x48, median of N runs (default 5), with measure_large.sh's methods.
#
# 1. --paging-bench: open pass, count pass, resize, page loads, search, and the
#    scroll bar's estimate simulation. No terminal.
# 2. Scrolling to the top with Page Up at 20 Hz: peak memory (/usr/bin/time -l)
#    and, from --stats, the time from the key to the frame for frames that
#    loaded pages. Whole-file mode, and paged with windows of 0, 1 and 4
#    screens; on the heavy fixture also pages of 16 and 256 lines.
# 3. Searching: Ctrl+F, a word typed a key at a time, then Enter to jump.
# 4. Selection across pages: a drag from the bottom to the top edge, held for
#    4 seconds of auto-scroll, with --verify-copy.
#
# Usage: ./measure_paging.sh [runs]
# Results: target/measure-paging/; tables printed at the end.
set -eu
cd "$(dirname "$0")"
cargo build --release -q
runs=${1:-5}
out=target/measure-paging
bin="$PWD/target/release/tui-prototype"
rm -rf "$out"
mkdir -p "$out"
[ -f fixtures/large-heavy.jsonl ] || ./target/release/gen_large

SESSIONS="large-median large-p90 large-heavy"
SESS="mp-$$"
wait_done() {
  while tmux has-session -t "$SESS" 2>/dev/null; do sleep 0.3; done
}
hex() {
  printf '%s' "$1" | od -An -tx1
}
# SGR mouse: button 0 press at column 10 row 30, drag to row 1 (the top edge), release
PRESS=$(hex "$(printf '\033[<0;10;30M')")
DRAG=$(hex "$(printf '\033[<32;10;1M')")
RELEASE=$(hex "$(printf '\033[<0;10;1m')")

# ---- 1. bench
for s in $SESSIONS; do
  for i in $(seq "$runs"); do
    "$bin" "fixtures/$s.jsonl" --paging-bench >"$out/$s-bench-$i.tsv"
  done
done

# ---- 2. scrolling to the top: memory and time to frame
scroll_run() { # $1 session, $2 label, rest: flags
  s=$1
  label=$2
  shift 2
  for i in $(seq "$runs"); do
    f="$out/$s-scroll-$label-$i"
    tmux new-session -d -s "$SESS" -x 160 -y 48 \
      "cd $PWD && /usr/bin/time -l $bin fixtures/$s.jsonl $* --stats $f.tsv --exit-after 8 >/dev/null 2>$f.mem"
    sleep 1
    n=0
    while [ $n -lt 100 ]; do
      tmux send-keys -t "$SESS" PageUp 2>/dev/null || break
      sleep 0.05
      n=$((n + 1))
    done
    wait_done
  done
}
for s in $SESSIONS; do
  scroll_run "$s" whole --static
  scroll_run "$s" w0 --paged --window 0
  scroll_run "$s" w1 --paged --window 1
  scroll_run "$s" w4 --paged --window 4
done
scroll_run large-heavy p16 --paged --window 1 --page-lines 16
scroll_run large-heavy p256 --paged --window 1 --page-lines 256

# ---- 3. searching: type the word a key at a time, then jump to the first match
for s in $SESSIONS; do
  for mode in whole paged; do
    flags="--static"
    [ $mode = paged ] && flags="--paged --window 1"
    for w in tool flaky; do
      for i in $(seq "$runs"); do
        f="$out/$s-search-$mode-$w-$i"
        tmux new-session -d -s "$SESS" -x 160 -y 48 \
          "cd $PWD && /usr/bin/time -l $bin fixtures/$s.jsonl $flags --stats $f.tsv --exit-after 5 >/dev/null 2>$f.mem"
        sleep 1
        tmux send-keys -t "$SESS" C-f
        for c in $(echo $w | fold -w1); do
          tmux send-keys -t "$SESS" "$c"
          sleep 0.2
        done
        sleep 0.5
        tmux send-keys -t "$SESS" Enter
        wait_done
      done
    done
  done
done

# ---- 4. selection across pages, heavy, window 0 so the drag's first pages are dropped
for i in $(seq "$runs"); do
  f="$out/select-$i"
  tmux new-session -d -s "$SESS" -x 160 -y 48 \
    "cd $PWD && $bin fixtures/large-heavy.jsonl --paged --window 0 --verify-copy --stats $f.tsv --exit-after 8"
  sleep 1
  tmux send-keys -t "$SESS" -H $PRESS
  tmux send-keys -t "$SESS" -H $DRAG
  sleep 4
  tmux send-keys -t "$SESS" -H $RELEASE
  sleep 0.5
  pbpaste >"$f.clip"
  wait_done
done

# ---- tables
median_of() { # $1 glob, $2 key
  grep -h "^$2	" $1 2>/dev/null | cut -f2 | sort -n | awk '{a[NR]=$1} END {if (NR>0) print a[int((NR+1)/2)]; else print "n/a"}'
}
mem_median() { # $1 glob, $2 field; printed in MiB
  for f in $1; do awk -v k="$2" '$0 ~ k {print $1}' "$f"; done | sort -n | awk '{a[NR]=$1} END {if (NR>0) printf "%.2f", a[int((NR+1)/2)]/1048576; else print "n/a"}'
}

echo "== Bench (ms, macOS arm64, warm, median of $runs)"
for s in $SESSIONS; do
  echo "$s:"
  for k in lines pages total_rows page_rows_median page_rows_max open_index_ms open_count_ms resize_ms all_open_ms page_load_median_ms page_load_p90_ms page_load_max_ms whole_load_ms \
    search_tool_ms search_tool_hits search_tool_hit_bytes search_tool_jump_ms search_flaky_ms search_flaky_hits search_flaky_jump_ms search_flaky_jump_pages \
    whole_search_tool_ms whole_search_flaky_ms rows_match_whole; do
    echo "  $k $(median_of "$out/$s-bench-*.tsv" $k)"
  done
  grep -h "^thumb_" "$out/$s-bench-1.tsv" | sed 's/^/  /'
done

echo
echo "== Scrolling to the top: peak footprint / max RSS (MiB), first frame, frames that loaded pages"
for s in $SESSIONS; do
  for label in whole w0 w1 w4 p16 p256; do
    ls $out/$s-scroll-$label-*.tsv >/dev/null 2>&1 || continue
    g="$out/$s-scroll-$label-*"
    echo "$s $label: footprint $(mem_median "$g.mem" "peak memory footprint")  rss $(mem_median "$g.mem" "maximum resident set size")  first_frame $(median_of "$g.tsv" first_frame_ms)  open_index $(median_of "$g.tsv" open_index_ms)  open_count $(median_of "$g.tsv" open_count_ms)  resident_max $(median_of "$g.tsv" resident_max)  load_frames $(median_of "$g.tsv" load_frames)  vis_miss $(median_of "$g.tsv" vis_miss_frames)  load_max $(median_of "$g.tsv" load_max_ms)  ev_to_flush_median $(median_of "$g.tsv" ev_to_flush_median_ms)  ev_to_flush_max $(median_of "$g.tsv" ev_to_flush_max_ms)"
  done
done
echo "worst ev_to_flush_max over every paged scrolling run: $(grep -h '^ev_to_flush_max_ms' $out/*-scroll-w*.tsv $out/*-scroll-p*.tsv | cut -f2 | sort -n | tail -1)"

echo
echo "== Searching: peak footprint (MiB), scans, slowest scan, frames that loaded pages, key to frame"
for s in $SESSIONS; do
  for mode in whole paged; do
    for w in tool flaky; do
      g="$out/$s-search-$mode-$w-*"
      echo "$s $mode $w: footprint $(mem_median "$g.mem" "peak memory footprint")  scans $(median_of "$g.tsv" scans)  scan_max $(median_of "$g.tsv" scan_max_ms)  load_frames $(median_of "$g.tsv" load_frames)  ev_to_flush_max $(median_of "$g.tsv" ev_to_flush_max_ms)"
    done
  done
done

echo
echo "== Selection across pages (heavy, window 0)"
for i in $(seq "$runs"); do
  f="$out/select-$i"
  same=no
  cmp -s "$f.clip" "$f.tsv.copied" && same=yes
  echo "run $i: matches whole $(grep '^copy_matches_whole' "$f.tsv" | cut -f2)  lines $(wc -l <"$f.tsv.copied" | tr -d ' ')  resident_max $(grep '^resident_max' "$f.tsv" | cut -f2)  clipboard same $same"
done
echo "Raw files are in $out"
