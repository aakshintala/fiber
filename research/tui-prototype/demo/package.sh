#!/usr/bin/env bash
#
# Assembles the folder sent to a friend for the TUI prototype demo:
# dist/fiber-tui-demo/, then dist/fiber-tui-demo.zip.
#
# The macOS arm64 binary is built locally. The two Linux binaries come from
# wherever `gh run download` put the tui-demo.yml workflow's artifacts —
# pass that directory as the one argument.
#
#   gh workflow run tui-demo.yml
#   gh run download <run-id> -D /tmp/tui-demo-artifacts
#   ./package.sh /tmp/tui-demo-artifacts

set -euo pipefail

usage() {
  cat >&2 <<USAGE
usage: $0 <linux-artifacts-dir>

<linux-artifacts-dir> is what 'gh run download <run-id> -D <dir>' wrote for
a tui-demo.yml run: it must contain tui-prototype-linux-x86_64/ and
tui-prototype-linux-arm64/ artifact folders, each holding the binary of the
same name.
USAGE
  exit 1
}

[[ $# -eq 1 ]] || usage
linux_dir=$1
[[ -d "$linux_dir" ]] || { echo "error: not a directory: $linux_dir" >&2; exit 1; }

cd "$(dirname "$0")"
proto=".."
dist_root="dist"
dist="$dist_root/fiber-tui-demo"

rm -rf "$dist_root"
mkdir -p "$dist/bin" "$dist/fixtures"

echo "Building the macOS arm64 binary..."
( cd "$proto" && cargo build --release -q )
macos_bin="$proto/target/release/tui-prototype"
if [[ ! -f "$macos_bin" ]]; then
  echo "error: macOS binary not found at $macos_bin after 'cargo build --release'" >&2
  exit 1
fi
cp "$macos_bin" "$dist/bin/tui-prototype-macos-arm64"

for arch in x86_64 arm64; do
  name="tui-prototype-linux-$arch"
  src="$linux_dir/$name/$name"
  if [[ ! -f "$src" ]]; then
    echo "error: missing Linux binary: $src" >&2
    echo "  expected an artifact folder named '$name' under $linux_dir," >&2
    echo "  containing a file also named '$name' (from tui-demo.yml)." >&2
    exit 1
  fi
  cp "$src" "$dist/bin/$name"
done

chmod +x "$dist"/bin/*

cp "$proto/fixtures/session.jsonl" "$dist/fixtures/"
cp "$proto"/fixtures/large-*.jsonl "$dist/fixtures/"

cp feedback-wizard.sh "$dist/"
chmod +x "$dist/feedback-wizard.sh"
cp README.txt "$dist/"

( cd "$dist_root" && rm -f fiber-tui-demo.zip && zip -rq fiber-tui-demo.zip fiber-tui-demo )

echo "Built $dist_root/fiber-tui-demo.zip"
