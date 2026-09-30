#!/bin/sh
set -eu
OUT="$(cd "$(dirname "$0")" && pwd)"
ROOT="$OUT/../../../../dependency-rss"
FIXTURES="$OUT/../../../fixtures"
PROBE="$ROOT"
mkdir -p "$OUT/converted"

convert_heic() {
  src=$1
  dst=$2
  if [ ! -f "$dst" ]; then
    sips -s format jpeg "$src" --out "$dst" >/dev/null
    echo "converted: $src -> $dst" >>"$OUT/conversion.log"
  fi
}

convert_heic "/System/Library/Desktop Pictures/.wallpapers/Sonoma Horizon/Sonoma Horizon.heic" \
  "$OUT/converted/Sonoma-Horizon.jpg"
convert_heic "/System/Library/Desktop Pictures/Sonoma.heic" \
  "$OUT/converted/Sonoma.jpg"
convert_heic "/System/Library/Desktop Pictures/iMac Green.heic" \
  "$OUT/converted/iMac-Green.jpg"

MANIFEST="$OUT/manifest.tsv"
sed \
  -e "s|ABS_FIXTURE|$FIXTURES|g" \
  -e "s|CONVERTED|$OUT/converted|g" \
  "$OUT/sources.txt" | grep -v '^#' | grep -v '^$' >"$MANIFEST"

cd "$PROBE"
cargo build --release -q --target-dir target/png-photos --features image-fir
IMAGE_PNG_PHOTOS=1 IMAGE_PHOTO_MANIFEST="$MANIFEST" \
  target/png-photos/release/dependency-rss >"$OUT/results.txt" 2>&1

sw_vers >"$OUT/sw_vers.txt"
sysctl -n machdep.cpu.brand_string >"$OUT/cpu.txt"
