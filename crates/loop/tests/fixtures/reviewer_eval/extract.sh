#!/usr/bin/env bash
# Verifies the committed reviewer-eval set: the built `review --scan` over
# the file is clean (stdout equals the file, no SCAN-DROP), the seeded tail
# equals seeded.jsonl byte-for-byte, the confidentiality script stays
# silent, and counts print by tool, label and kind.
#
#   fixtures/reviewer_eval/extract.sh [--out <file>]
#
# The sampled lines are lane-authored rewrites, not regenerated: raw
# sessions never enter the repository, so no committed recipe rebuilds
# them. The byte-repro rule survives only for the seeded tail.
#
# The committed `calls.jsonl` is the scan's stdout; any `SCAN-DROP
# <matcher-class> id=<id>` stderr line aborts the verify, so a silently
# shrunk set fails loudly.
#
# Bash plus built binaries and standard Unix tools only (no `rg`/`jq`
# on runners, per the current header).

set -euo pipefail
export LC_ALL=C

usage() {
  echo "usage: extract.sh [--out <file>]" >&2
  exit 2
}

FIX_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$FIX_DIR/../../../../.." && pwd)"

OUT="$FIX_DIR/calls.jsonl"
SEEDED="$FIX_DIR/seeded.jsonl"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    *) usage ;;
  esac
done

cd "$ROOT"
cargo build -p loop --example review

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export TMPDIR="$TMP"

./target/debug/examples/review --scan "$OUT" >"$TMP/scanned.jsonl" 2>"$TMP/scan.txt"
cat "$TMP/scan.txt" >&2
if grep -q "SCAN-DROP" "$TMP/scan.txt"; then
  echo "extract.sh: the scan dropped lines; refusing a shrunk set" >&2
  exit 1
fi
if ! cmp -s "$OUT" "$TMP/scanned.jsonl"; then
  echo "extract.sh: the scan stdout differs from the committed file" >&2
  exit 1
fi
SEEDED_COUNT="$(wc -l < "$SEEDED" | tr -d ' ')"
tail -n "$SEEDED_COUNT" "$OUT" > "$TMP/tail.jsonl"
if ! cmp -s "$TMP/tail.jsonl" "$SEEDED"; then
  echo "extract.sh: the seeded tail differs from seeded.jsonl" >&2
  exit 1
fi
"$FIX_DIR/check_confidential.sh" "$OUT" "$SEEDED"

python3 - "$OUT" <<'PYEOF'
import json
import sys

tools = {}
labels = {}
kinds = {}
total = 0
with open(sys.argv[1], encoding="utf-8") as fh:
    for raw in fh:
        raw = raw.strip()
        if not raw:
            continue
        obj = json.loads(raw)
        tools[obj["tool"]] = tools.get(obj["tool"], 0) + 1
        label = obj["labels"]["family1"]
        labels[label] = labels.get(label, 0) + 1
        kinds[obj["source"]["kind"]] = kinds.get(obj["source"]["kind"], 0) + 1
        total += 1
print("lines %d tools %s labels %s kinds %s" % (total, tools, labels, kinds))
PYEOF

wc -l "$OUT"
