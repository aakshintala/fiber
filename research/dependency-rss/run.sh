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
FEATURES="serde_json ureq ratatui crossterm rusqlite mlua clap clap_complete thiserror signal-hook getrandom base64 ring rustix regex ignore search similar pulldown-cmark jsonschema syntect arborium image image-parts html5ever encoding_rs flate2 jiff lopdf qrcode brush-parser"
# Every crate in the runtime table of docs/dependencies.md, built together.
RUNTIME="serde_json ureq ratatui crossterm mlua clap clap_complete thiserror signal-hook getrandom base64 ring rustix search similar html5ever encoding_rs pulldown-cmark flate2 jiff qrcode"
# #3027's Linux shell sandbox. The builds are attempted on every runner and
# the outcome recorded per row below; they link into the shipped Linux
# binary, so Linux's together build includes them and macOS's does not.
SANDBOX_FEATURES="landlock seccompiler"
if [ "$(uname)" != Darwin ]; then
  RUNTIME="$RUNTIME landlock seccompiler"
fi

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

# Median of 5 runs of $1 (a probe binary) with the remaining arguments as
# VAR=value environment assignments (HL_LANGS, HL_REPEAT). Same peak method.
hl_median() {
  bin=$1
  shift
  for _ in 1 2 3 4 5; do
    if [ "$(uname)" = Darwin ]; then
      script -q /dev/null /usr/bin/time -l env "$@" "$bin" </dev/null 2>&1 | tr -d '\r' | awk '/peak memory footprint/ {print int($1 / 1024)}'
    else
      script -qec "env $* /usr/bin/time -v $bin" /dev/null </dev/null 2>&1 | tr -d '\r' | awk -F': ' '/Maximum resident set size/ {print $2}'
    fi
  done | sort -n | sed -n 3p
}

hl_row() {
  label=$1
  shift
  kib=$(hl_median "$hlbin" "$@")
  echo "| $label | $kib | $((kib - base)) |"
}

build() {
  cargo build --release -q --target-dir "target/$1" ${2:+--features "$2"}
  echo "target/$1/release/dependency-rss"
}

# One sandbox row: builds the feature and measures it like every other row.
# When the build fails, as a Linux-only crate's does on macOS, the row
# records what the attempt establishes instead of a figure.
sandbox_row() {
  f=$1
  mkdir -p target
  if bin=$(build "$f" "$f" 2>"target/$f-build-err.txt"); then
    kib=$(median "$bin")
    echo "| $f | $kib | $((kib - base)) | $(( $(wc -c < "$bin") / 1024 )) |"
  else
    err=$(grep -m1 "^error" "target/$f-build-err.txt" 2>/dev/null | tr -d '|' | cut -c1-160 || true)
    echo "| $f | does not build on $(uname -sm): ${err:-build failed} | n/a | n/a |"
  fi
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
for f in $SANDBOX_FEATURES; do
  sandbox_row "$f"
done
all=$(echo $RUNTIME | tr ' ' ',')
bin=$(build runtime "$all")
kib=$(median "$bin")
echo "| all runtime crates together | $kib | $((kib - base)) | $(( $(wc -c < "$bin") / 1024 )) |"
# tree-sitter-bash is not a feature of this manifest: tree-sitter 0.27 and
# arborium-tree-sitter both declare links = "tree-sitter", which cargo
# refuses to resolve in one manifest. shell-ts/ is a standalone crate with
# the identical workload, measured with the same method.
tsbin=$(cd shell-ts && cargo build --release -q && echo "$PWD/target/release/shell-ts")
kib=$(median "$tsbin")
echo "| tree-sitter-bash (shell-ts/) | $kib | $((kib - base)) | $(( $(wc -c < "$tsbin") / 1024 )) |"

# Arborium highlighting per language: one arborium build (all 18 grammars
# linked), HL_LANGS/HL_REPEAT selecting fixture blocks at run time. Each
# block is a realistic ~40-line snippet (highlight-fixtures/). Same peak
# method, median of 5, over the same baseline.
echo
echo "arborium highlighting, $(uname -sm):"
echo "| Workload | $metric (KiB) | Over baseline (KiB) |"
echo "|---|---:|---:|"
hlbin=$(build arborium arborium)
hl_row "linked, highlight nothing" "HL_LANGS="
for lang in bash c cpp css diff go html java javascript json markdown python rust sql toml tsx typescript yaml; do
  hl_row "$lang, one block" "HL_LANGS=$lang" "HL_REPEAT=1"
done
hl_row "rust, one block" "HL_LANGS=rust" "HL_REPEAT=1"
hl_row "rust, 10 blocks" "HL_LANGS=rust" "HL_REPEAT=10"
hl_row "rust, 100 blocks" "HL_LANGS=rust" "HL_REPEAT=100"
hl_row "rust+python+javascript, one block each" "HL_LANGS=rust,python,javascript" "HL_REPEAT=1"
hl_row "all 18 languages, one block each" "HL_LANGS=all" "HL_REPEAT=1"

# The lopdf PDF rows: the same binary as the `lopdf` row above, once per
# scanned-document-like fixture, cutting pages 1-20 as the image child does.
echo
echo "lopdf PDF fixtures (pages=1-20 cut, PDF_FIXTURE), $(uname -sm):"
echo "| Fixture | $metric (KiB) | Over baseline (KiB) | Stripped binary (KiB) |"
echo "|---|---:|---:|---:|"
# Built once above; the binary is the same for every fixture row.
pdfbin="target/lopdf/release/dependency-rss"
pdfsize=$(( $(wc -c < "$pdfbin") / 1024 ))
for f in pdf-fixtures/scan-30p.pdf pdf-fixtures/scan-60p.pdf pdf-fixtures/scan-100p.pdf pdf-fixtures/scan-120p.pdf; do
  pages=$(basename "$f" .pdf | sed 's/scan-//;s/p$//')
  mib=$(awk "BEGIN {printf \"%.1f\", $(wc -c < "$f") / 1048576}")
  export PDF_FIXTURE="$f"
  kib=$(median "$pdfbin")
  unset PDF_FIXTURE
  echo "| lopdf $mib MiB, $pages pages | $kib | $((kib - base)) | $pdfsize |"
done
# The cap-sized scanned-like fixture: exactly 104,857,600 bytes, so its peak
# says what a PDF at the cap costs. The many-small-objects fixture packs
# ~200 annotation-like objects per page into Flate object streams with a
# cross-reference stream, so its peak comes from object count and
# decompression, not raw bytes. Both cut pages 1-20 like the rows above.
for f in pdf-fixtures/scan-cap.pdf pdf-fixtures/manyobjs-100p.pdf; do
  mib=$(awk "BEGIN {printf \"%.1f\", $(wc -c < "$f") / 1048576}")
  case "$f" in
    *scan-cap.pdf) label="$mib MiB, 191 pages, cap-sized";;
    *manyobjs-100p.pdf) label="$mib MiB, 100 pages, many small objects";;
  esac
  export PDF_FIXTURE="$f"
  kib=$(median "$pdfbin")
  unset PDF_FIXTURE
  echo "| lopdf $label | $kib | $((kib - base)) | $pdfsize |"
done
