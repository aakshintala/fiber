#!/bin/bash
# Linux run for fiber#130. Run from the repository root; writes everything under out/.
# probe.yml is the workflow that ran it (copy it to .github/workflows/ on a throwaway branch).
set -u
ROOT=$PWD
OUT=$ROOT/out
mkdir -p $OUT
{ uname -a; nproc; lscpu | grep -E 'Model name|^CPU\(s\)'; rustc --version; } > $OUT/host.txt 2>&1
mkdir -p research/image-limits/fixtures
(cd research/image-limits/gen && cargo run -q --release -- ../fixtures && cargo run -q --release -- try ../fixtures/{cmyk.jpg,corrupt.jpg,trunc.jpg,deep16.png,small.gif,small.webp}) > $OUT/try.txt 2>&1
cd research/dependency-rss
peak() { IMAGE_ONLY=$2 /usr/bin/time -v $1 2>&1 >/dev/null | awk -F': ' '/Maximum resident set size/ {print $2}'; }
median() { for _ in 1 2 3 4 5; do peak "$1" "$2"; done | sort -n | sed -n 3p; }
cargo build --release -q --target-dir target/base || echo "build failed base" >> $OUT/errors.txt
echo "base $(median target/base/release/dependency-rss nothing) KiB, binary $(stat -c %s target/base/release/dependency-rss)" > $OUT/rss.txt
for f in image image-parts image-fir image-header; do
  cargo build --release -q --features $f --target-dir target/$f || { echo "build failed $f" >> $OUT/errors.txt; continue; }
  B=target/$f/release/dependency-rss
  echo "## $f binary=$(stat -c %s $B)" >> $OUT/rss.txt
  for o in "" photo shot small.gif small.webp flat-9000; do echo "only=$o peak_rss_kib=$(median $B "$o")" >> $OUT/rss.txt; done
  IMAGE_ONLY= $B >> $OUT/determinism_$f.txt 2>&1
  cargo tree --features $f -e normal --prefix none | grep -v '^dependency-rss' | sed 's/ (\*)//' | sort -u > $OUT/tree_$f.txt
done
