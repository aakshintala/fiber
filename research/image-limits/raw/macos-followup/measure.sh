#!/bin/sh
# Peak memory footprint on macOS (median of 5), dependency-rss probe.
set -eu
OUT="$(cd "$(dirname "$0")" && pwd)"
cd "$OUT/../../../dependency-rss"
sw_vers >"$OUT/sw_vers.txt"
rustc --version >"$OUT/rustc.txt"

peak_kib() {
  script -q /dev/null /usr/bin/time -l "$1" </dev/null 2>&1 | tr -d '\r' | awk '/peak memory footprint/ {print int($1 / 1024)}'
}

median() {
  bin=$1
  for _ in 1 2 3 4 5; do peak_kib "$bin"; done | sort -n | sed -n 3p
}

build() {
  name=$1
  feat=$2
  if [ -n "$feat" ]; then
    cargo build --release -q --target-dir "target/$name" --features "$feat"
  else
    cargo build --release -q --target-dir "target/$name"
  fi
  echo "target/$name/release/dependency-rss"
}

[ -d ../image-limits/fixtures ] || { mkdir -p ../image-limits/fixtures; (cd ../image-limits/gen && cargo run -q --release -- ../fixtures); }

BASE=$(build base "")
BASE_MED=$(median "$BASE")
echo "empty_baseline_kib=$BASE_MED" | tee "$OUT/baseline.txt"

for f in image image-parts image-fir image-header; do
  bin=$(build "$f" "$f")
  kib=$(median "$bin")
  bytes=$(wc -c <"$bin" | tr -d ' ')
  echo "$f all_fixtures peak_kib=$kib over=$((kib - BASE_MED)) binary_bytes=$bytes" | tee -a "$OUT/summary-all.txt"
  cargo tree -e normal --features "$f" --prefix none >"$OUT/tree_$f.txt"
  awk '{print $1}' "$OUT/tree_$f.txt" | sort -u | wc -l | tr -d ' ' >"$OUT/crates_unique_$f.txt"
  wc -l <"$OUT/tree_$f.txt" | tr -d ' ' >"$OUT/crates_lines_$f.txt"
done

for feat in image image-parts image-fir; do
  bin=$(build "${feat}-per" "$feat")
  for fix in photo-4000x3000.jpg shot-4000x3000.png flat-9000x9000.png; do
    key="${feat}__${fix}"
    export IMAGE_ONLY="$fix"
    kib=$(median "$bin")
    echo "$key peak_kib=$kib over=$((kib - BASE_MED))" | tee -a "$OUT/summary-per-fixture.txt"
  done
done
