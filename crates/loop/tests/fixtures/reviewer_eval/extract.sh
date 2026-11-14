#!/usr/bin/env bash
# Rebuilds the committed reviewer-eval set from retained sampled lines,
# committed synthetic fixture dirs, and seeded.jsonl; labels synthetic
# lines and scans the complete set clean.
#
#   fixtures/reviewer_eval/extract.sh [--out <file>]
#
# Sampled calls are lane-authored rewrites; raw sessions are not committed.
# The recipe retains those rows, regenerates synthetic rows from committed
# fixtures, appends the seeded tail, and verifies the resulting set.
# Any SCAN-DROP <matcher-class> id=<id> stderr line aborts the rebuild.

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

# Preserve the 194 reviewed sample rows exactly; the raw source sessions are
# deliberately absent. Reject drift rather than silently changing membership.
python3 - "$FIX_DIR/calls.jsonl" "$TMP/sampled.jsonl" <<'PYEOF'
import json
import sys

source, target = sys.argv[1:3]
rows = []
with open(source, encoding="utf-8") as fh:
    for raw in fh:
        if not raw.strip():
            continue
        obj = json.loads(raw)
        if obj["source"]["kind"] == "sampled":
            if not obj.get("reason") or obj["labels"].get("family1") is None:
                raise SystemExit("sampled line %s lost its reason or verdict" % obj["id"])
            rows.append(raw.rstrip("\n"))
if len(rows) != 194:
    raise SystemExit("expected 194 retained sampled lines, found %d" % len(rows))
with open(target, "w", encoding="utf-8") as fh:
    for row in rows:
        fh.write(row + "\n")
PYEOF

DIRS=()
while IFS= read -r d; do DIRS+=("$d"); done < <(printf '%s\n' "$FIX_DIR"/../reviewer_consent/*/ | sort)
DIRS+=("$FIX_DIR/contradictory-consent")
DIRS+=("$FIX_DIR/../review_person_input")
FLAGS=()
for d in "${DIRS[@]}"; do FLAGS+=(--synthetic "$d"); done
if ! ./target/debug/examples/review "${FLAGS[@]}" >"$TMP/synthetic.jsonl" 2>"$TMP/drops.txt"; then
  cat "$TMP/drops.txt" >&2
  exit 1
fi
cat "$TMP/drops.txt" >&2

python3 - "$TMP/sampled.jsonl" "$TMP/synthetic.jsonl" "$SEEDED" "$TMP/labelled.jsonl" <<'PYEOF'
import json
import sys

sampled_path, synthetic_path, seeded_path, out_path = sys.argv[1:5]


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
    elif kind == "seeded":
        if obj.get("source", {}).get("kind") != "seeded":
            raise SystemExit("seeded input holds kind %r" % (obj.get("source", {}).get("kind"),))
        if labels.get("family1") != "block" or labels.get("family2") is not None:
            raise SystemExit("seeded line %s lost its block verdict" % obj.get("id"))
    else:
        raise SystemExit("unknown row kind %s" % kind)
    obj["labels"] = labels
    return json.dumps(obj, separators=(",", ":"))


seen = set()
count = 0
with open(out_path, "w", encoding="utf-8") as out:
    for path, kind in ((sampled_path, "sampled"), (synthetic_path, "synthetic"), (seeded_path, "seeded")):
        with open(path, encoding="utf-8") as fh:
            for raw in fh:
                raw = raw.rstrip("\n")
                if raw:
                    obj = json.loads(raw)
                    if obj["id"] in seen:
                        raise SystemExit("duplicate id %s" % obj["id"])
                    seen.add(obj["id"])
                    out.write(raw if kind == "sampled" else emit(obj, kind))
                    out.write("\n")
                    count += 1
print("assembled %d lines" % count, file=sys.stderr)
PYEOF

./target/debug/examples/review --scan "$TMP/labelled.jsonl" >"$OUT" 2>"$TMP/scan.txt"
cat "$TMP/scan.txt" >&2
if grep -q "SCAN-DROP" "$TMP/scan.txt"; then
  echo "extract.sh: the scan dropped lines; refusing to write a shrunk set" >&2
  exit 1
fi
"$FIX_DIR/check_confidential.sh" --project QuotaBar --project pi-extensions "$OUT" "$SEEDED"
wc -l "$OUT"
