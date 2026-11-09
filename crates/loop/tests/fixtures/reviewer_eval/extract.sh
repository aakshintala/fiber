#!/usr/bin/env bash
# Rebuilds calls.jsonl from the committed synthetic fixture dirs plus
# seeded.jsonl, then labels first-family verdicts and scans the set clean.
#
#   fixtures/reviewer_eval/extract.sh [--out <file>]
#
# Every value the recipe needs is a committed byte or a literal flag:
# the converter reads the `reviewer_consent/` sessions and the
# `review_person_input/` session with an empty scrub context, the python
# step labels synthetic lines by fixture session (`control-*` sessions
# block, every other synthetic session passes; `family2` stays null), and
# `seeded.jsonl` lines keep their block verdicts. The committed
# `calls.jsonl` is the scan's stdout; any `SCAN-DROP <matcher-class>
# id=<id>` stderr line aborts the rebuild, so a silently shrunk set fails
# loudly. Rerunning this script reproduces `calls.jsonl` byte-for-byte.
#
# The committed synthetic labels (family1 authored by the session map
# below, family2 null on every line) await owner review.

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

DIRS=()
while IFS= read -r d; do DIRS+=("$d"); done < <(printf '%s\n' "$FIX_DIR"/../reviewer_consent/*/ | sort)
DIRS+=("$FIX_DIR/../review_person_input")

FLAGS=()
for d in "${DIRS[@]}"; do FLAGS+=(--synthetic "$d"); done
if ! ./target/debug/examples/review "${FLAGS[@]}" >"$TMP/synthetic.jsonl" 2>"$TMP/drops.txt"; then
  cat "$TMP/drops.txt" >&2
  exit 1
fi
cat "$TMP/drops.txt" >&2

python3 - "$TMP/synthetic.jsonl" "$SEEDED" "$TMP/labelled.jsonl" <<'PYEOF'
import json
import sys

synthetic_path, seeded_path, out_path = sys.argv[1:4]


def emit(obj, kind):
    labels = obj.get("labels") or {}
    if kind == "synthetic":
        if obj.get("source", {}).get("kind") != "synthetic":
            raise SystemExit("synthetic input holds kind %r" % (obj.get("source", {}).get("kind"),))
        if labels.get("family1") is not None or labels.get("family2") is not None:
            raise SystemExit("synthetic line %s already labelled" % obj.get("id"))
        session = obj.get("source", {}).get("session", "")
        labels["family1"] = "block" if session.startswith("control-") else "pass"
        labels["family2"] = None
    else:
        if obj.get("source", {}).get("kind") != "seeded":
            raise SystemExit("seeded input holds kind %r" % (obj.get("source", {}).get("kind"),))
        if labels.get("family1") != "block" or labels.get("family2") is not None:
            raise SystemExit("seeded line %s lost its block verdict" % obj.get("id"))
    obj["labels"] = labels
    return json.dumps(obj, separators=(",", ":"))


seen = set()
count = 0
with open(out_path, "w", encoding="utf-8") as out:
    for path, kind in ((synthetic_path, "synthetic"), (seeded_path, "seeded")):
        with open(path, encoding="utf-8") as fh:
            for raw in fh:
                raw = raw.rstrip("\n")
                if raw:
                    obj = json.loads(raw)
                    if obj["id"] in seen:
                        raise SystemExit("duplicate id %s" % obj["id"])
                    seen.add(obj["id"])
                    out.write(emit(obj, kind) + "\n")
                    count += 1
print("labelled %d" % count, file=sys.stderr)
PYEOF

./target/debug/examples/review --scan "$TMP/labelled.jsonl" >"$OUT" 2>"$TMP/scan.txt"
cat "$TMP/scan.txt" >&2
if grep -q "SCAN-DROP" "$TMP/scan.txt"; then
  echo "extract.sh: the scan dropped lines; refusing to write a shrunk set" >&2
  exit 1
fi
wc -l "$OUT"
