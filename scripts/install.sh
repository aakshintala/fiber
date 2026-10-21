#!/bin/sh
# Installs Fiber from a GitHub release (docs/releasing.md, "Installing").
#
# Usage: sh install.sh
#
# Reads FIBER_INSTALL_DIR (default ~/.local/bin; empty means unset),
# FIBER_VERSION (default the newest release; one leading `v` is allowed)
# and FIBER_CHANNEL (default stable; empty means unset). It installs the
# binary, then runs it once with the internal release install step, which
# puts that release's docs and first-party extensions in
# Fiber home.
#
# `--channel stable|nightly` picks the release channel, beating
# FIBER_CHANNEL; an explicit FIBER_VERSION beats both. The stable channel
# installs from `latest/download`, which never sees pre-releases. The
# nightly channel reads the release list and installs the newest nightly.
#
# `--base-url <url>` replaces https://github.com/aakshintala/fiber/releases
# and is for tests only: under it the list is read from
# `$base/releases.json` in the same shape.
set -eu

fail() {
  printf 'fiber: %s\n' "$1" >&2
  exit "${2:-1}"
}

base=https://github.com/aakshintala/fiber/releases
base_arg=
channel=${FIBER_CHANNEL:-}
while [ $# -gt 0 ]; do
  case $1 in
    --base-url)
      [ $# -ge 2 ] || fail "--base-url needs a URL." 2
      base_arg=$2
      base=${2%/}
      shift 2
      ;;
    --channel)
      [ $# -ge 2 ] || fail "--channel needs stable or nightly." 2
      channel=$2
      shift 2
      ;;
    *) fail "install.sh does not take $1." 2 ;;
  esac
done

case $channel in
  '' | stable) channel=stable ;;
  nightly) ;;
  *) fail "--channel $channel is not stable or nightly." 2 ;;
esac

for tool in curl tar mktemp uname; do
  command -v "$tool" >/dev/null 2>&1 || fail "install.sh needs $tool."
done
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  fail "install.sh needs sha256sum or shasum."
fi

# The file's SHA-256 as it prints it, digest first.
digest() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi
}

version=${FIBER_VERSION:-}
if [ -n "$version" ]; then
  version=${version#v}
  # Three dot-separated numbers: digits and dots only, no empty part.
  case $version in
    *[!0-9.]* | .* | *. | *..* | *.*.*.*) version= ;;
    *.*.*) ;;
    *) version= ;;
  esac
  [ -n "$version" ] || fail "FIBER_VERSION=$FIBER_VERSION is not a version such as 0.3.0." 2
fi

os=$(uname -s)
cpu=$(uname -m)
target=
case $os/$cpu in
  Darwin/arm64) target=aarch64-apple-darwin ;;
  # A shell under Rosetta reports x86_64 on an arm64 Mac.
  Darwin/x86_64)
    if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
      target=aarch64-apple-darwin
    fi
    ;;
  Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-musl ;;
  Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
esac
[ -n "$target" ] || fail "$os $cpu has no release."

dest=${FIBER_INSTALL_DIR:-}
if [ -z "$dest" ]; then
  [ -n "${HOME:-}" ] || fail "HOME is not set, so set FIBER_INSTALL_DIR."
  dest=$HOME/.local/bin
fi

if [ -n "$version" ]; then
  from=$base/download/v$version
elif [ "$channel" = nightly ]; then
  # GitHub's `releases/latest` skips pre-releases, so the nightly channel
  # reads the release list instead: a GitHub API call, held to its limit
  # of 60 requests an hour for each address.
  if [ -n "$base_arg" ]; then
    list_url=$base/releases.json
  else
    list_url="https://api.github.com/repos/aakshintala/fiber/releases?per_page=100"
  fi
  # The newest entry by numeric (base version tuple, date) comparison,
  # with `"prerelease": true`, `"draft": false` and a nightly tag. A
  # tied entry loses to the later one, so a repeated list entry changes
  # nothing. The list is pretty-printed JSON, one field per line, as the
  # API writes it: an object opens and closes on a line of its own.
  list=$(curl -fsSL --proto-redir =https "$list_url") ||
    fail "the nightly release list could not be downloaded from $list_url."
  tag=$(printf '%s' "$list" | tr ',' '\n' | awk '
    /^[[:space:]]*\{[[:space:]]*$/ { tag = ""; pre = ""; draft = ""; next }
    /"tag_name"/ {
      line = $0
      sub(/.*"tag_name"[[:space:]]*:[[:space:]]*"/, "", line)
      sub(/".*/, "", line)
      tag = line
    }
    /"prerelease"[[:space:]]*:/ { pre = ($0 ~ /:[[:space:]]*true/) ? "true" : "false" }
    /"draft"[[:space:]]*:/ { draft = ($0 ~ /:[[:space:]]*true/) ? "true" : "false" }
    /^[[:space:]]*\}[,]?[[:space:]]*$/ {
      if (tag ~ /^v[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]$/ && pre == "true" && draft == "false") {
        ver = tag
        sub(/^v/, "", ver)
        split(ver, parts, /[.\-]/)
        key = sprintf("%020d.%020d.%020d.%s", parts[1], parts[2], parts[3], parts[5])
        if (best == "" || key > best) { best = key; best_tag = tag }
      }
      tag = ""; pre = ""; draft = ""
    }
    END { if (best_tag != "") print best_tag }
  ')
  [ -n "$tag" ] || fail "no nightly release found."
  from=$base/download/$tag
else
  from=$base/latest/download
fi
archive=fiber-$target.tar.gz

tmp=
stage=
cleanup() {
  if [ -n "$tmp" ]; then rm -rf "$tmp"; fi
  if [ -n "$stage" ]; then rm -rf "$stage"; fi
}
trap cleanup EXIT
trap 'exit 1' INT TERM
tmp=$(mktemp -d)

for file in "$archive" "$archive.sha256"; do
  curl -fsSL --proto-redir =https -o "$tmp/$file" "$from/$file" ||
    fail "$file could not be downloaded from $from."
done

# The checksum file's first whitespace-separated field, which must be 64 hex
# digits in either case.
want=$(tr '\r' ' ' <"$tmp/$archive.sha256" | awk 'NF { print $1; exit }')
case $want in
  '' | *[!0-9a-fA-F]*) want= ;;
esac
[ ${#want} -eq 64 ] || fail "$archive.sha256 holds no SHA-256."
want=$(printf '%s' "$want" | tr 'A-F' 'a-f')
got=$(digest "$tmp/$archive" | awk '{ print $1 }')
[ "$got" = "$want" ] || fail "$archive does not match its .sha256 file."

# Extract beside the destination and rename into place, so a failed install
# never leaves half a binary.
mkdir -p "$dest"
stage=$(mktemp -d "$dest/.fiber-install.XXXXXX")
tar -xzf "$tmp/$archive" -C "$stage"
if [ ! -f "$stage/fiber" ] || [ -h "$stage/fiber" ]; then
  fail "$archive holds no fiber binary."
fi
chmod 755 "$stage/fiber"
mv -f "$stage/fiber" "$dest/fiber"
rm -rf "$stage"
stage=

case :${PATH:-}: in
  *:"$dest":*) ;;
  *) printf 'fiber: %s is not on PATH. Add it to use fiber.\n' "$dest" >&2 ;;
esac

again="installed $dest/fiber, but its docs and extensions were not installed. Run install.sh again."
if [ -z "$version" ]; then
  # `fiber 0.3.0 (4f2a9c1)` gives 0.3.0.
  line=$("$dest/fiber" --version) || fail "$again"
  version=${line#fiber }
  version=${version%% *}
fi
if [ -n "$base_arg" ]; then
  set -- --base-url "$base_arg"
else
  set --
fi
"$dest/fiber" release-install "$version" "$@" || fail "$again"
printf 'fiber: installed %s/fiber %s.\n' "$dest" "$version" >&2
