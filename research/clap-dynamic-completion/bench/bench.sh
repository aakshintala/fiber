#!/bin/sh
# Times what the shell runs on each Tab, with 20 entries in BENCH_HOME.
# Needs hyperfine. Run after `cargo build --release`.
set -eu
bin="$(pwd)/target/release/bin"
home="$(mktemp -d)"
for i in $(seq -w 1 20); do : > "$home/ext-$i"; done
export BENCH_HOME="$home"

# Check the protocol first: bash passes the words after "--" and the index
# of the word being completed. The output is one candidate per line, joined
# with the IFS the shell sends.
out="$(COMPLETE=bash _CLAP_IFS="
" _CLAP_COMPLETE_INDEX=2 "$bin" -- bin remove ext-1)"
echo "sample output:"; echo "$out" | head -3
[ "$(echo "$out" | wc -l | tr -d ' ')" -ge 10 ] || { echo "completer returned too few candidates"; exit 1; }

hyperfine --warmup 50 --runs 500 -N --export-json results.json \
  --command-name "startup floor: bin --version" "$bin --version" \
  --command-name "startup floor via env (same wrapper as the rows below)" "env BENCH_X=1 $bin --version" \
  --command-name "tab: bin remove <TAB> (20 values)" \
  "env COMPLETE=bash _CLAP_COMPLETE_INDEX=2 $bin -- bin remove ''" \
  --command-name "tab: bin <TAB> (subcommands only)" \
  "env COMPLETE=bash _CLAP_COMPLETE_INDEX=1 $bin -- bin ''" \
  --command-name "shell start: COMPLETE=bash bin (registration script)" \
  "env COMPLETE=bash $bin"

python3 - <<'P'
import json, statistics
for r in json.load(open("results.json"))["results"]:
    t = sorted(r["times"])
    p95 = t[int(len(t) * 0.95) - 1]
    print(f'{r["command"][:60]:60} median {statistics.median(t)*1000:6.2f} ms  p95 {p95*1000:6.2f} ms')
P
rm -rf "$home"
