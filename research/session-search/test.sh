#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

bash -n run.sh || { echo "run.sh failed bash -n" >&2; exit 1; }

TMPDIR_TEST=$(mktemp -d)
trap 'rm -rf "${TMPDIR_TEST:-}" "${FAKE:-}"' EXIT

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

# A malformed timing read must fail the run before any row is appended
# (#1349): drive a copy of run.sh with a fake toolchain.
FAKE=$(mktemp -d)
cp run.sh residency.py "$FAKE/"
mkdir -p "$FAKE/bin"
cat > "$FAKE/bin/cargo" <<'CARGO_EOF'
#!/usr/bin/env bash
if [ "${1:-}" = build ]; then
  mkdir -p target/release
  cat > target/release/session-search <<'BIN_EOF'
#!/usr/bin/env bash
if [ "${1:-}" = gen ]; then
  mkdir -p "$2/projects"
  exit 0
fi
if [ "${SCAN_GARBAGE:-}" != "" ]; then
  echo "$SCAN_GARBAGE" >&2
fi
exit 0
BIN_EOF
  chmod +x target/release/session-search
  exit 0
fi
echo "stub cargo only builds" >&2
exit 1
CARGO_EOF
chmod +x "$FAKE/bin/cargo"

run_copy() {
  if command -v timeout >/dev/null 2>&1; then
    timeout 120 bash run.sh
  else
    bash run.sh
  fi
}

# The stub scan prints nothing: ms is empty on both platforms.
status=0
(cd "$FAKE" && export CORPUS="$FAKE/corpus" PATH="$FAKE/bin:$PATH" && run_copy >run.log 2>run.err) || status=$?
if [ "$status" -eq 0 ]; then
  echo "run.sh exited 0 with an empty scan timing (want non-zero)" >&2
  exit 1
fi
if [ -f "$FAKE/results.tsv" ] && [ "$(wc -l < "$FAKE/results.tsv" | tr -d ' ')" -gt 1 ]; then
  echo "run.sh appended a row with an empty scan timing (want none)" >&2
  exit 1
fi

if [ "$(uname -s)" = Linux ]; then
  # The stub scan writes garbage to stderr, which reaches the TIMEFILE
  # through /usr/bin/time -f, so the gnu read sees a non-number.
  status=0
  (cd "$FAKE" && rm -f results.tsv && export CORPUS="$FAKE/corpus" SCAN_GARBAGE=garbage PATH="$FAKE/bin:$PATH" && run_copy >run2.log 2>run2.err) || status=$?
  if [ "$status" -eq 0 ]; then
    echo "run.sh exited 0 with a garbage time read (want non-zero)" >&2
    exit 1
  fi
  if [ -f "$FAKE/results.tsv" ] && [ "$(wc -l < "$FAKE/results.tsv" | tr -d ' ')" -gt 1 ]; then
    echo "run.sh appended a row with a garbage time read (want none)" >&2
    exit 1
  fi
fi

echo "malformed timing read fails the run"
