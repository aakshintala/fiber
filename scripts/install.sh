#!/bin/sh
# Installs Fiber from a GitHub release (docs/releasing.md, "Installing").
#
# Usage: sh install.sh [--dir <path>] [--path | --no-path] [--no-setup] [-- setup-args...]
#
# Reads FIBER_INSTALL_DIR (default ~/.local/bin; empty means unset),
# FIBER_VERSION (default the newest release; one leading `v` is allowed)
# and FIBER_CHANNEL (default stable; empty means unset). It installs the
# binary, then runs it once with the internal release install step, which
# puts that release's docs and first-party extensions in
# Fiber home. Finally it runs `fiber setup`, which asks the rest.
#
# `--channel stable|nightly` picks the release channel, beating
# FIBER_CHANNEL; an explicit FIBER_VERSION beats both. The stable channel
# installs from `latest/download`, which never sees pre-releases. The
# nightly channel reads the release list and installs the newest nightly.
#
# `--dir <path>` picks the install directory, beating FIBER_INSTALL_DIR.
# `--path` and `--no-path` answer the `PATH` question. `--no-setup`
# skips the `fiber setup` step. Everything after a second `--` reaches
# `fiber setup` unchanged.
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
dir=
want_path=
no_setup=
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
    --dir)
      [ $# -ge 2 ] || fail "--dir needs a directory." 2
      dir=$2
      shift 2
      ;;
    --path)
      want_path=yes
      shift
      ;;
    --no-path)
      want_path=no
      shift
      ;;
    --no-setup)
      no_setup=yes
      shift
      ;;
    --)
      shift
      break
      ;;
    *) fail "install.sh does not take $1." 2 ;;
  esac
done
# Every argument left reaches `fiber setup` verbatim.

case $channel in
  '' | stable) channel=stable ;;
  nightly) ;;
  *) fail "--channel $channel is not stable or nightly." 2 ;;
esac

for tool in curl tar mktemp uname grep; do
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

dest=${dir:-}
if [ -z "$dest" ]; then
  dest=${FIBER_INSTALL_DIR:-}
  if [ -z "$dest" ]; then
    [ -n "${HOME:-}" ] || fail "HOME is not set, so set FIBER_INSTALL_DIR."
    dest=$HOME/.local/bin
  fi
fi

# The questions read /dev/tty, so they work under `curl | sh`: without
# one the defaults hold and nothing is asked. The opens run in a
# subshell, so a missing terminal fails silently instead of the shell
# reporting the failed redirect.
tty=no
if ( : </dev/tty ) 2>/dev/null && ( : >/dev/tty ) 2>/dev/null; then
  tty=yes
fi

# Asks $1 on /dev/tty with the prompt on stderr, and prints the answer:
# empty when the terminal is gone.
ask() {
  printf 'fiber: %s ' "$1" >&2
  answer=
  if ( : </dev/tty ) 2>/dev/null; then
    IFS= read -r answer </dev/tty || true
  fi
  printf '%s' "$answer"
}

# The install directory: a flag answers it, a terminal is asked with the
# default offered, and anything else keeps the default.
if [ -z "$dir" ] && [ "$tty" = yes ]; then
  answer=$(ask "install to [$dest]:")
  if [ -n "$answer" ]; then
    dest=$answer
  fi
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
  # API writes it. Commas split fields only outside strings, so a comma
  # in a body never splits its string before the closing quote. Depth is
  # counted in braces outside strings, so nested
  # author and asset objects never reset the candidate: fields count only
  # at the release entry's own depth, and only its closing brace decides.
  # The strip is escape-aware, so an escaped quote in a body never ends a
  # string early and its braces never count.
  list=$(curl -fsSL --proto-redir =https "$list_url") ||
    fail "the nightly release list could not be downloaded from $list_url."
  tag=$(printf '%s' "$list" | awk '
    {
      out = ""
      in_str = 0
      esc = 0
      for (i = 1; i <= length($0); i++) {
        c = substr($0, i, 1)
        if (esc) { out = out c; esc = 0; continue }
        if (in_str && c == "\\") { out = out c; esc = 1; continue }
        if (c == "\"") { in_str = !in_str; out = out c; continue }
        if (c == "," && !in_str) { print out; out = ""; continue }
        out = out c
      }
      print out
    }
  ' | awk '
    {
      line = $0
      stripped = line
      gsub(/"([^"\\]|\\.)*"/, "\"\"", stripped)
      opens = gsub(/\{/, "{", stripped)
      closes = gsub(/\}/, "}", stripped)
      if (depth == 0 && opens > 0) { tag = ""; pre = ""; draft = "" }
      if (depth == 1 || (depth == 0 && opens > 0)) {
        if (line ~ /"tag_name"/) {
          tag = line
          sub(/.*"tag_name"[[:space:]]*:[[:space:]]*"/, "", tag)
          sub(/".*/, "", tag)
        }
        if (line ~ /"prerelease"[[:space:]]*:/) { pre = (line ~ /:[[:space:]]*true/) ? "true" : "false" }
        if (line ~ /"draft"[[:space:]]*:/) { draft = (line ~ /:[[:space:]]*true/) ? "true" : "false" }
      }
      depth += opens - closes
      if (depth == 0 && closes > 0) {
        if (tag ~ /^v[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]$/ && pre == "true" && draft == "false") {
          ver = tag
          sub(/^v/, "", ver)
          split(ver, parts, /[.\-]/)
          key = sprintf("%020d.%020d.%020d.%s", parts[1], parts[2], parts[3], parts[5])
          if (best == "" || key > best) { best = key; best_tag = tag }
        }
        tag = ""; pre = ""; draft = ""
      }
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

# The login shell's startup file and the line that puts the install
# directory on PATH, by the shell $SHELL names. Anything else has no
# file to offer: PATH is left alone with the warning.
shell=
if [ -n "${HOME:-}" ]; then
  name=${SHELL:-}
  name=${name##*/}
  name=${name#-}
  case $name in
    zsh | bash | fish) shell=$name ;;
  esac
fi
startup=
line=
# Single-quotes $1 for a startup-file line: POSIX shells and fish both
# read 'a'\''b' back as a'b. A newline cannot sit on one line, so it
# is refused with a non-zero exit.
nl='
'
quote_path() {
  case $1 in
    *"$nl"*) return 1 ;;
  esac
  escaped=$(printf '%s' "$1" | sed "s/'/'\\\\''/g")
  printf "'%s'" "$escaped"
}
if [ -n "$shell" ]; then
  case $shell in
    zsh) startup=$HOME/.zshrc ;;
    bash) startup=$HOME/.bashrc ;;
    fish) startup=$HOME/.config/fish/config.fish ;;
  esac
  quoted=$(quote_path "$dest") ||
    fail "the install directory holds a newline, so it cannot go on PATH."
  if [ "$shell" = fish ]; then
    line="fish_add_path $quoted"
  else
    line="export PATH=$quoted:\$PATH"
  fi
fi

warn_path() {
  printf 'fiber: %s is not on PATH. Add it to use fiber.\n' "$dest" >&2
}

# Appends $2 to $1 unless that exact line is already there, keeping the
# file's last line whole: a rerun adds nothing.
append_once() {
  parent=${1%/*}
  if [ ! -d "$parent" ]; then
    mkdir -p "$parent" || fail "could not create $parent."
  fi
  if [ -f "$1" ] && grep -Fxq -- "$2" "$1"; then
    return 0
  fi
  if [ -s "$1" ] && [ -n "$(tail -c 1 "$1")" ]; then
    printf '\n' >>"$1"
  fi
  printf '%s\n' "$2" >>"$1"
}

case :${PATH:-}: in
  *:"$dest":*) ;;
  *)
    if [ "$want_path" = yes ] && [ -n "$line" ]; then
      append_once "$startup" "$line"
    elif [ "$want_path" = no ]; then
      warn_path
    elif [ -z "$want_path" ] && [ "$tty" = yes ] && [ -n "$line" ]; then
      answer=$(ask "add $dest to PATH in $startup? [y/N]:")
      if [ "$answer" = y ] || [ "$answer" = Y ] || [ "$answer" = yes ]; then
        append_once "$startup" "$line"
      else
        printf 'fiber: %s is not on PATH. To add it, put this line in %s:\n%s\n' "$dest" "$startup" "$line" >&2
      fi
    else
      warn_path
    fi
    ;;
esac

again="installed $dest/fiber, but its docs and extensions were not installed. Run install.sh again."
if [ -z "$version" ]; then
  # `fiber 0.3.0 (4f2a9c1)` gives 0.3.0.
  line=$("$dest/fiber" --version) || fail "$again"
  version=${line#fiber }
  version=${version%% *}
fi
if [ -n "$base_arg" ]; then
  "$dest/fiber" release-install "$version" --base-url "$base_arg" || fail "$again"
else
  "$dest/fiber" release-install "$version" || fail "$again"
fi

# Step 7 runs last, after the binary is in place: everything past the
# second `--` reaches `fiber setup` unchanged. `--no-setup` skips it.
# On a terminal setup reads its questions from /dev/tty, as the script
# does; without one it does only what its flags say. A failed setup
# keeps the installed binary and propagates its exit.
if [ -z "$no_setup" ]; then
  setup_code=0
  if [ "$tty" = yes ]; then
    "$dest/fiber" setup "$@" </dev/tty || setup_code=$?
  else
    "$dest/fiber" setup "$@" || setup_code=$?
  fi
  [ "$setup_code" -eq 0 ] || fail "fiber setup exited $setup_code." "$setup_code"
fi
printf 'fiber: installed %s/fiber %s.\n' "$dest" "$version" >&2
