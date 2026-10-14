# Shared release-build mechanics for scripts/release-size and
# scripts/paging-jig, sourced sibling to the script
# (`$(dirname "${BASH_SOURCE[0]}")/lib/release.sh`, before any `cd`): the
# checkout the script operates on holds no scripts/ of its own in tests,
# and the base worktree never provides them. Each script keeps its
# differences: what it selects to build, strip versus copy, the fallback
# when the base does not build, and its cleanup and messages.
#
# Requires $top, the head checkout; sets $target, the target that ships.
# shellcheck shell=bash
# shellcheck disable=SC2154 # $top comes from the sourcing script

readonly target=x86_64-unknown-linux-musl

# Builds the checkout in the current directory into the shared target
# directory ($top/target), selecting the binary with "$@" (`--bin fiber`
# or `-p tui --example paging`). The caller chains the copy out of
# target/ onto this call, so a failed build fails even where errexit does
# not apply, and never copies a binary an earlier build left in target/.
build() {
  CARGO_TARGET_DIR="$top/target" cargo build --release --locked --target "$target" "$@"
}

# Checks out a detached worktree of $1 at $2, reusing this checkout's
# target directory. The caller removes it on exit and decides what a
# failed checkout means: the release comparison falls back, the jig skips
# its base.
base_worktree() {
  git worktree add --detach "$2" "$1"
}
