#!/bin/sh
# Builds four profiles and runs every case in each. Output on stdout.
set -e
cd "$(dirname "$0")"
for p in dev release dev-unwind release-unwind; do
  cargo build --profile "$p" -q
done
for d in debug release dev-unwind release-unwind; do
  ./target/$d/containment run
done
