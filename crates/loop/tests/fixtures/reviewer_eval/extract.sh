#!/usr/bin/env bash
# Rebuilds calls.jsonl from owner-supplied native session dirs plus
# seeded.jsonl, then labels first-family verdicts and scans the set clean.
#
# Full rebuild from sources:
#   fixtures/reviewer_eval/extract.sh \
#     --claude <dir> [--claude <dir>...] --pi <dir> [--pi <dir>...] \
#     --sample 400 --out calls.jsonl \
#     --block <sampled-line-id> [--block <id>...] \
#     --home <dir> --name <owner> --email <addr>
#
# Resume from an earlier sampling run (its sources consumed since):
#   fixtures/reviewer_eval/extract.sh \
#     --sampled <sampled.jsonl> --out calls.jsonl \
#     --block <sampled-line-id> [--block <id>...] \
#     --home <dir> --name <owner> --email <addr>
#
# Every contextual value the scrubber substitutes arrives as an explicit
# flag; nothing is read from the environment. The session-slug form of
# --home (dashes for slashes) is masked with $OWNER_NAME as well, because
# slugs carry the same username the scrubber masks in paths. Every other
# sampled line is pass; seeded lines keep their block verdicts.
#
# debt: replaced by the real reviewer telemetry pipeline, once every model
# family calls the second-family runner from the lane report.

set -euo pipefail

usage() {
  echo "usage: extract.sh (--claude <dir>... --pi <dir>... --sample N | --sampled <file>) --out <file> [--block <id>...] --home <dir> --name <owner> --email <addr>" >&2
  exit 2
}

FIX_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$FIX_DIR/../../../../.." && pwd)"

CLAUDE_DIRS=()
PI_DIRS=()
BLOCK_IDS=()
SAMPLE=""
SAMPLED=""
OUT=""
SEEDED="$ROOT/crates/loop/tests/fixtures/reviewer_eval/seeded.jsonl"
HOME_DIR=""
OWNER_NAME=""
OWNER_EMAIL=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --claude) CLAUDE_DIRS+=("$2"); shift 2 ;;
    --pi) PI_DIRS+=("$2"); shift 2 ;;
    --block) BLOCK_IDS+=("$2"); shift 2 ;;
    --sample) SAMPLE="$2"; shift 2 ;;
    --sampled) SAMPLED="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --seeded) SEEDED="$2"; shift 2 ;;
    --home) HOME_DIR="$2"; shift 2 ;;
    --name) OWNER_NAME="$2"; shift 2 ;;
    --email) OWNER_EMAIL="$2"; shift 2 ;;
    *) usage ;;
  esac
done

[ -n "$OUT" ] || usage
[ -n "$HOME_DIR" ] && [ -n "$OWNER_NAME" ] && [ -n "$OWNER_EMAIL" ] || usage
if [ -n "$SAMPLED" ]; then
  [ "${#CLAUDE_DIRS[@]}" -eq 0 ] && [ "${#PI_DIRS[@]}" -eq 0 ] && [ -z "$SAMPLE" ] || usage
else
  [ "${#CLAUDE_DIRS[@]}" -gt 0 ] && [ "${#PI_DIRS[@]}" -gt 0 ] && [ -n "$SAMPLE" ] || usage
fi

cd "$ROOT"
cargo build -p loop --example review

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export TMPDIR="$TMP"

if [ -z "$SAMPLED" ]; then
  FLAGS=(--sample "$SAMPLE" --scrub
    --as-home "$HOME_DIR" --owner-name "$OWNER_NAME" --owner-email "$OWNER_EMAIL")
  for d in "${CLAUDE_DIRS[@]}"; do FLAGS+=(--import-claude "$d"); done
  for d in "${PI_DIRS[@]}"; do FLAGS+=(--import-pi "$d"); done
  ./target/debug/examples/review "${FLAGS[@]}" >"$TMP/sampled.jsonl" 2>"$TMP/drops.txt"
  cat "$TMP/drops.txt" >&2
  SAMPLED="$TMP/sampled.jsonl"
fi

python3 - "$SAMPLED" "$SEEDED" "$TMP/labelled.jsonl" "$HOME_DIR" "$OWNER_NAME" "${BLOCK_IDS[@]}" <<'PYEOF'
import json
import sys

sampled_path, seeded_path, out_path, home, owner = sys.argv[1:6]
block = set(sys.argv[6:])

slug = home.replace("/", "-")
slug_mask = "-home-" + owner


def mask(line):
    return line.replace(slug, slug_mask)


def emit(obj, kind):
    labels = obj.get("labels") or {}
    if kind == "sampled":
        if obj.get("source", {}).get("kind") != "sampled":
            raise SystemExit("sampled input holds kind %r" % (obj.get("source", {}).get("kind"),))
        if labels.get("family1") is not None or labels.get("family2") is not None:
            raise SystemExit("sampled line %s already labelled" % obj.get("id"))
        labels["family1"] = "block" if obj["id"] in block else "pass"
        labels["family2"] = None
        block.discard(obj["id"])
    else:
        if obj.get("source", {}).get("kind") != "seeded":
            raise SystemExit("seeded input holds kind %r" % (obj.get("source", {}).get("kind"),))
        if labels.get("family1") != "block" or labels.get("family2") is not None:
            raise SystemExit("seeded line %s lost its block verdict" % obj.get("id"))
    obj["labels"] = labels
    return mask(json.dumps(obj, separators=(",", ":")))


seen = set()
dupes = 0
count = 0
with open(out_path, "w", encoding="utf-8") as out:
    for path, kind in ((sampled_path, "sampled"), (seeded_path, "seeded")):
        with open(path, encoding="utf-8") as fh:
            for raw in fh:
                raw = raw.rstrip("\n")
                if raw:
                    obj = json.loads(raw)
                    if obj["id"] in seen:
                        if kind == "sampled":
                            dupes += 1
                            continue
                        raise SystemExit("duplicate id %s" % obj["id"])
                    seen.add(obj["id"])
                    out.write(emit(obj, kind) + "\n")
                    count += 1
if block:
    raise SystemExit("block ids not in sampled input: %s" % sorted(block))
print("labelled %d (dropped %d duplicate ids)" % (count, dupes), file=sys.stderr)
PYEOF

./target/debug/examples/review --scan "$TMP/labelled.jsonl" >"$OUT" 2>"$TMP/scan.txt"
cat "$TMP/scan.txt" >&2
wc -l "$OUT"
