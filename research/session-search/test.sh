#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

bash -n run.sh || { echo "run.sh failed bash -n" >&2; exit 1; }

TMPDIR_TEST=$(mktemp -d)
trap 'rm -rf "$TMPDIR_TEST"' EXIT

head -c 1048576 /dev/urandom > "$TMPDIR_TEST/blob"
cat "$TMPDIR_TEST/blob" > /dev/null

out=$(python3 -I residency.py resident "$TMPDIR_TEST") \
  || { echo "resident failed on warm file" >&2; exit 1; }
case "$out" in
  ''|*[!0-9]*) echo "resident did not print a number: $out" >&2; exit 1 ;;
esac
if [ "$out" -le 0 ]; then
  echo "resident printed 0 for a recently read file" >&2
  exit 1
fi

if [ "$(uname -s)" = Linux ]; then
  python3 -I residency.py evict "$TMPDIR_TEST" \
    || { echo "evict failed" >&2; exit 1; }
  out=$(python3 -I residency.py resident "$TMPDIR_TEST") \
    || { echo "resident failed after evict" >&2; exit 1; }
  if [ "$out" != 0 ]; then
    echo "resident printed $out after evict, want 0" >&2
    exit 1
  fi
else
  echo "eviction check skipped: not Linux"
fi

echo "residency ok"
