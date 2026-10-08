#!/usr/bin/env bash
# Times session_search's scan (log::SessionScan::scan) over synthetic Fiber
# homes of three sizes, for research/session-search/README.md (#578).
#
# For each corpus (300, 1,300 and 4,000 MiB of logs with one tool output in
# 25 also saved as a 64 KiB artifact, and 1,300 MiB with no artifacts) and
# each query (one with many hits, one with none), five warm runs after one
# warm-up, and, where the page cache can be dropped without root, five cold runs,
# each after evicting every file of the corpus. One scan per process.
# Each row is the median of its five runs.
#
# Cold runs need GNU dd (`iflag=nocache`, Linux). Eviction is confirmed with
# `fincore` when it is installed. Elsewhere the cold rows are skipped.
#
# CORPUS (default: $TMPDIR/fiber-session-search) holds the generated homes,
# kept between runs; delete it when done. Needs about 9 GB of free disk.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release
BIN=$PWD/target/release/session-search
CORPUS=${CORPUS:-${TMPDIR:-/tmp}/fiber-session-search}
RUNS=5
OUT=results.tsv
PLATFORM="$(uname -s) $(uname -m)"
MANY="retry budget"
NONE="zqxv no such text"

cold=no
if dd --version >/dev/null 2>&1; then
  cold=yes
fi

evict() {
  find "$1" -type f -print0 |
    xargs -0 -n 64 sh -c 'for f; do dd if="$f" iflag=nocache count=0 status=none; done' _
}

# Bytes of the corpus still in the page cache, or "unconfirmed".
resident() {
  if command -v fincore >/dev/null 2>&1; then
    find "$1" -type f -print0 | xargs -0 fincore --bytes --noheadings --output RES |
      awk '{ total += $1 } END { print total + 0 }'
  else
    echo unconfirmed
  fi
}

median() { sort -n | awk '{ v[NR] = $1 } END { print v[int((NR + 1) / 2)] }'; }

if [ ! -f "$OUT" ]; then
  printf 'date\tplatform\tcorpus_mib\tartifact_every\tlog_mib\tartifact_mib\tsessions\tquery\tcache\tresident_bytes\tmedian_ms\truns_ms\thits\n' > "$OUT"
fi

for corpus in 300:25 1300:25 4000:25 1300:0; do
  mib=${corpus%:*}
  every=${corpus#*:}
  home="$CORPUS/home-$mib-$every"
  if [ ! -d "$home" ]; then
    "$BIN" gen "$home" "$mib" "$every"
  fi
  sessions=$(find "$home/projects" -name events.jsonl | wc -l | tr -d ' ')
  log_mib=$(find "$home/projects" -name events.jsonl -print0 | xargs -0 du -k | awk '{ t += $1 } END { print int(t / 1024) }')
  artifact_mib=$(find "$home/projects" -path '*/artifacts/*' -type f -print0 | xargs -0 du -k /dev/null | awk '{ t += $1 } END { print int(t / 1024) }')
  for query in "$MANY" "$NONE"; do
    caches=warm
    if [ "$cold" = yes ]; then caches="warm cold"; fi
    for cache in $caches; do
      times=()
      hits=
      left=
      if [ "$cache" = warm ]; then "$BIN" scan "$home" "$query" >/dev/null; fi
      for _ in $(seq "$RUNS"); do
        if [ "$cache" = cold ]; then
          evict "$home"
          left=$(resident "$home")
        fi
        read -r ms hits _ < <("$BIN" scan "$home" "$query")
        times+=("$ms")
      done
      med=$(printf '%s\n' "${times[@]}" | median)
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$(date +%F)" "$PLATFORM" "$mib" "$every" "$log_mib" "$artifact_mib" "$sessions" \
        "$query" "$cache" "${left:-}" "$med" "$(IFS=,; echo "${times[*]}")" "$hits" |
        tee -a "$OUT"
    done
  done
done

echo
echo "Results appended to $OUT"
