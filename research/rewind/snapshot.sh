#!/bin/sh
# Whole-tree git snapshot cost with a persistent per-session index and a separate
# object directory. Run on a disposable copy: `cp -c -R <repo> sp` (APFS clone).
# Measured on macOS arm64. Usage: snapshot.sh <copy-of-repo>
set -e
cd "$1"
mkdir -p nm
python3 -c "
import os
for i in range(20000):
    d=f'nm/p{i//100}'; os.makedirs(d,exist_ok=True); open(f'{d}/f{i}.js','w').write(str(i))
"
mkdir -p ../obj
export GIT_OBJECT_DIRECTORY=$PWD/../obj GIT_ALTERNATE_OBJECT_DIRECTORIES=$PWD/.git/objects GIT_INDEX_FILE=$PWD/../persist.idx
cp .git/index "$GIT_INDEX_FILE"
t() { python3 -c "
import subprocess,time,sys
s=time.perf_counter(); subprocess.run('git add -A && git write-tree',shell=True,capture_output=True)
print(sys.argv[1], round((time.perf_counter()-s)*1000), 'ms')" "$1"; }
t first-snapshot; t second-no-change; t third-no-change
echo z > nm/p5/f500.js; t after-untracked-change
echo q >> GLOSSARY.md; t after-tracked-change
