#!/bin/sh
set -eu
OUT="$(cd "$(dirname "$0")" && pwd)"
ROOT="$OUT/../../../../dependency-rss"
cd "$ROOT"
[ -d ../image-limits/fixtures ] || (cd ../image-limits/gen && cargo run -q --release -- ../fixtures)

for feat in image image-fir; do
  cargo build --release -q --target-dir "target/timing-$feat" --features "$feat"
  bin="target/timing-$feat/release/dependency-rss"
  IMAGE_TIMING=1 "$bin" >"$OUT/timing-$feat.txt" 2>&1
  IMAGE_SPAWN_BENCH=1 "$bin" >"$OUT/spawn-$feat.txt" 2>&1
done

sw_vers >"$OUT/sw_vers.txt"
sysctl -n machdep.cpu.brand_string >"$OUT/cpu.txt"
