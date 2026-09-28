#!/usr/bin/env bash
# Regenerates the fixtures and prints the results table for
# research/session-listing/README.md (issue #15).
#
# Eight runs, one-factor-at-a-time from a shared pivot (N=600 sessions,
# 60 KiB preamble, 1,749 KiB total file size — the real p90 of the owner's
# pooled Claude Code + pi session files), plus one run with every swept value
# at its heaviest:
#   - N swept at 200 / 600 / 2,000, preamble and total size held at the pivot
#   - preamble swept at 20 / 60 / 150 KiB, N and total size held at the pivot
#   - total file size swept at the real median / p90 / max, N and preamble
#     held at the pivot
#   - worst case: N=2,000, preamble=150 KiB, total size=max, together
# See README.md, "Method", for why these are the values.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release

BIN=./target/release/session-listing
RUNS=5
OUT=results.tsv

echo -e "label\tn\tpreamble_kib_target\ttotal_kib_target\ttotal_kib_actual\ta_ms\ta_bytes\tb_line_ms\tb_line_bytes\tb_prefix_ms\tb_prefix_bytes\tc_line_tail_ms\tc_line_tail_bytes\tc_prefix_tail_ms\tc_prefix_tail_bytes" > "$OUT"

run() { "$BIN" "$1" "$2" "$3" "$4" "$RUNS" | tee -a "$OUT"; }

# N sweep (pivot: preamble 60 KiB, total 1,749 KiB)
run n200   200  60 1749
run n600   600  60 1749   # also the pivot row for the other two sweeps
run n2000 2000  60 1749

# Preamble sweep (pivot: N 600, total 1,749 KiB)
run p20    600  20 1749
run p150   600 150 1749

# Total file size sweep (pivot: N 600, preamble 60 KiB)
run s_median 600 60   326
run s_max    600 60 15313

# Worst case: every swept value at its heaviest, together.
run worst 2000 150 15313

echo
echo "Results written to $OUT"
