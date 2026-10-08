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
# Cold runs need Linux and python3; the helper residency.py evicts each corpus
# file and checks that no page of it is resident; a sample whose eviction is
# not confirmed is not scanned, and the script fails after 3 attempts.
#
# Each scan runs under `/usr/bin/time` so its peak RSS lands in the trailing
# `peak_rss_bytes` column (median of the five runs' peaks, in bytes): GNU
# `time -f %M` on Linux (KiB, times 1024), BSD `time -l`'s maximum resident
# set size on macOS. The script needs `/usr/bin/time` and exits without it.
# The wrapper reports on stderr, so the scan's own `ms` timing on stdout is
# unchanged. The column is appended at the end, so earlier rows without it
# are untouched. The script prints the core count (`getconf
# _NPROCESSORS_ONLN`) once at the start for the log.
#
# CORPUS (default: $TMPDIR/fiber-session-search) holds the generated homes,
# kept between runs; delete it when done. Needs about 9 GB of free disk.
set -euo pipefail
cd "$(dirname "$0")"
RESIDENCY="$PWD/residency.py"

cargo build --release
BIN=$PWD/target/release/session-search
CORPUS=${CORPUS:-${TMPDIR:-/tmp}/fiber-session-search}
RUNS=5
OUT=results.tsv
PLATFORM="$(uname -s) $(uname -m)"
MANY="retry budget"
NONE="zqxv no such text"

cold=no
if [ "$(uname -s)" = Linux ] && command -v python3 >/dev/null 2>&1; then
  cold=yes
fi

echo "nproc: $(getconf _NPROCESSORS_ONLN)"

command -v /usr/bin/time >/dev/null || { echo "needs /usr/bin/time" >&2; exit 1; }
case "$(uname -s)" in
  Linux) rss_mode=gnu ;;
  Darwin) rss_mode=bsd ;;
  *) echo "needs Linux or Darwin" >&2; exit 1 ;;
esac
TIMEFILE=$(mktemp)
trap 'rm -f "$TIMEFILE"' EXIT

evict() {
  python3 -I "$RESIDENCY" evict "$1"
}

resident() {
  python3 -I "$RESIDENCY" resident "$1"
}

evict_confirmed() {
  for _ in 1 2 3; do
    evict "$1"
    if [ "$(resident "$1")" = 0 ]; then
      return 0
    fi
  done
  return 1
}

median() { sort -n | awk '{ v[NR] = $1 } END { print v[int((NR + 1) / 2)] }'; }

if [ ! -f "$OUT" ]; then
  printf 'date\tplatform\tcorpus_mib\tartifact_every\tlog_mib\tartifact_mib\tsessions\tquery\tcache\tresident_bytes\tmedian_ms\truns_ms\thits\tpeak_rss_bytes\n' > "$OUT"
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
      rss_values=()
      hits=
      left=
      if [ "$cache" = warm ]; then "$BIN" scan "$home" "$query" >/dev/null; fi
      for _ in $(seq "$RUNS"); do
        if [ "$cache" = cold ]; then
          evict_confirmed "$home" || { echo "eviction not confirmed: $home" >&2; exit 1; }
          left=confirmed
        fi
        case "$rss_mode" in
          gnu)
            read -r ms hits _ < <(/usr/bin/time -f '%M' "$BIN" scan "$home" "$query" 2>"$TIMEFILE")
            rss=$(( $(cat "$TIMEFILE") * 1024 ))
            ;;
          bsd)
            read -r ms hits _ < <(/usr/bin/time -l "$BIN" scan "$home" "$query" 2>"$TIMEFILE")
            rss=$(awk '/maximum resident set size/ { print $1 }' "$TIMEFILE")
            ;;
        esac
        case "$rss" in
          ''|*[!0-9]*) echo "could not read peak RSS" >&2; exit 1 ;;
        esac
        times+=("$ms")
        rss_values+=("$rss")
      done
      med=$(printf '%s\n' "${times[@]}" | median)
      rss_med=$(printf '%s\n' "${rss_values[@]}" | median)
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$(date +%F)" "$PLATFORM" "$mib" "$every" "$log_mib" "$artifact_mib" "$sessions" \
        "$query" "$cache" "${left:-}" "$med" "$(IFS=,; echo "${times[*]}")" "$hits" "$rss_med" |
        tee -a "$OUT"
    done
  done
done

echo
echo "Results appended to $OUT"
