#!/bin/sh
# Peak memory of each candidate crate, alone, over the featureless baseline.
# Median of 5 runs. Prints a Markdown table.
#
# Linux reports peak RSS. macOS reports peak memory footprint (what Activity
# Monitor shows): macOS RSS also counts system framework pages shared with
# every other process, and a crate that links Security.framework shows
# ~4.5 MiB of RSS before it runs a line of code.
set -eu
cd "$(dirname "$0")"
FEATURES="serde_json ureq ratatui crossterm rusqlite mlua clap clap_complete thiserror signal-hook getrandom base64 ring rustix regex ignore search similar pulldown-cmark jsonschema syntect arborium image image-parts html5ever encoding_rs flate2 jiff lopdf"
# Every crate in the runtime table of docs/dependencies.md, built together.
RUNTIME="serde_json ureq ratatui crossterm mlua clap clap_complete thiserror signal-hook getrandom base64 ring rustix search similar html5ever encoding_rs pulldown-cmark flate2 jiff"

# crossterm needs a terminal, so every binary runs under script(1), which
# gives it a pseudo-terminal. time(1) runs inside it and measures only the
# probe binary.
peak_kib() {
  if [ "$(uname)" = Darwin ]; then
    script -q /dev/null /usr/bin/time -l "$1" </dev/null 2>&1 | tr -d '\r' | awk '/peak memory footprint/ {print int($1 / 1024)}'
  else
    script -qec "/usr/bin/time -v $1" /dev/null </dev/null 2>&1 | tr -d '\r' | awk -F': ' '/Maximum resident set size/ {print $2}'
  fi
}

median() {
  bin=$1
  for _ in 1 2 3 4 5; do peak_kib "$bin"; done | sort -n | sed -n 3p
}

build() {
  cargo build --release -q --target-dir "target/$1" ${2:+--features "$2"}
  echo "target/$1/release/dependency-rss"
}

# The image features read fixtures written by research/image-limits/gen.
[ -d ../image-limits/fixtures ] || { mkdir -p ../image-limits/fixtures; (cd ../image-limits/gen && cargo run -q --release -- ../fixtures); }
# The lopdf PDF rows read scanned-document-like fixtures written by gen/.
[ -d pdf-fixtures ] || { mkdir -p pdf-fixtures; (cd gen && cargo run -q --release -- ../pdf-fixtures); }

echo "$(uname -sm), $(rustc --version)"
base=$(median "$(build base "")")
echo
if [ "$(uname)" = Darwin ]; then metric="Peak footprint"; else metric="Peak RSS"; fi
echo "| Crate | $metric (KiB) | Over baseline (KiB) | Stripped binary (KiB) |"
echo "|---|---:|---:|---:|"
echo "| (none) | $base | 0 | $(( $(wc -c < target/base/release/dependency-rss) / 1024 )) |"
for f in $FEATURES; do
  bin=$(build "$f" "$f")
  kib=$(median "$bin")
  echo "| $f | $kib | $((kib - base)) | $(( $(wc -c < "$bin") / 1024 )) |"
done
all=$(echo $RUNTIME | tr ' ' ',')
bin=$(build runtime "$all")
kib=$(median "$bin")
echo "| all runtime crates together | $kib | $((kib - base)) | $(( $(wc -c < "$bin") / 1024 )) |"

# The lopdf PDF rows: the same binary as the `lopdf` row above, once per
# scanned-document-like fixture, cutting pages 1-20 as the image child does.
echo
echo "lopdf PDF fixtures (pages=1-20 cut, PDF_FIXTURE), $(uname -sm):"
echo "| Fixture | $metric (KiB) | Over baseline (KiB) | Stripped binary (KiB) |"
echo "|---|---:|---:|---:|"
# Built once above; the binary is the same for every fixture row.
pdfbin="target/lopdf/release/dependency-rss"
pdfsize=$(( $(wc -c < "$pdfbin") / 1024 ))
for f in pdf-fixtures/scan-30p.pdf pdf-fixtures/scan-60p.pdf pdf-fixtures/scan-100p.pdf pdf-fixtures/scan-120p.pdf pdf-fixtures/scan-240p.pdf pdf-fixtures/scan-480p.pdf pdf-fixtures/scan-960p.pdf; do
  pages=$(basename "$f" .pdf | sed 's/scan-//;s/p$//')
  mib=$(awk "BEGIN {printf \"%.1f\", $(wc -c < "$f") / 1048576}")
  export PDF_FIXTURE="$f"
  kib=$(median "$pdfbin")
  unset PDF_FIXTURE
  echo "| lopdf $mib MiB, $pages pages | $kib | $((kib - base)) | $pdfsize |"
done
