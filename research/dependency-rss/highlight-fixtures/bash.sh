#!/bin/bash
# Nightly rollup: archive each session log older than 7 days, then vacuum.
set -euo pipefail

LOG_DIR="${1:-$HOME/.fiber/sessions}"
ARCHIVE_DIR="$LOG_DIR/archive"
mkdir -p "$ARCHIVE_DIR"

archived=0
while IFS= read -r -d '' log; do
  base="$(basename "$log" .jsonl)"
  if gzip -c "$log" > "$ARCHIVE_DIR/$base.jsonl.gz"; then
    rm "$log"
    archived=$((archived + 1))
  else
    echo "warn: failed to archive $log" >&2
  fi
done < <(find "$LOG_DIR" -maxdepth 1 -name '*.jsonl' -mtime +7 -print0)

echo "archived $archived session log(s)"

total_bytes=$(du -sb "$ARCHIVE_DIR" | cut -f1)
if [ "$total_bytes" -gt 1073741824 ]; then
  echo "warn: archive over 1 GiB ($total_bytes bytes)" >&2
fi

# Keep the 20 newest archives; delete the rest.
ls -t "$ARCHIVE_DIR"/*.jsonl.gz 2>/dev/null | tail -n +21 | xargs -r rm --
echo "rollup done at $(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Report free space; fail the rollup when the disk is nearly full.
free_pct=$(df -P "$ARCHIVE_DIR" | awk 'NR==2 {gsub(/%/,"",$5); print $5}')
if [ "$free_pct" -ge 95 ]; then
  echo "error: disk $free_pct% full, keeping unarchived logs" >&2
  exit 1
fi

# fsync the archive dir so a crash cannot lose the fresh archives.
python3 -c 'import os,sys; fd = os.open(sys.argv[1], os.O_DIRECTORY); os.fsync(fd)' "$ARCHIVE_DIR"
