#!/bin/bash
# Exercised inside the landlock-probe's exec'd bash, i.e. already restricted.
# $1: the directory the probe granted full read-write access to.
set -u
ALLOWED_DIR="$1"

echo "=== write inside allowed dir (expect ok) ==="
echo hello > "$ALLOWED_DIR/ok.txt"
echo "exit=$?"
cat "$ALLOWED_DIR/ok.txt" 2>&1

echo "=== write to \$HOME/x (expect EACCES) ==="
echo hello > "$HOME/x"
echo "exit=$?"

echo "=== echo > /tmp/x (expect ok) ==="
echo hello > /tmp/x
echo "exit=$?"
cat /tmp/x 2>&1

echo "=== cat /etc/hostname (expect ok) ==="
cat /etc/hostname
echo "exit=$?"

echo "=== curl -sS https://example.com (expect fail if net ABI enforced) ==="
curl -sS --max-time 5 https://example.com
echo "exit=$?"
