# Shell parser cost: tree-sitter-bash vs brush-parser (issue #1497 Q1)

Which bash parser Fiber's shell recogniser could use (option C: parse, then
accept only an allow-listed set of node kinds), and what each costs. Only
measured facts; anything not measured is marked "not measured".

## Figures

Probe figures are macOS arm64 peak memory footprint over the empty program
(median of 5, `/usr/bin/time -l` under `script(1)`), the harness's own
method (`research/dependency-rss/run.sh`). The workload is identical for
both: parse the ten commands below and walk every node of each tree,
counting nodes, after one warm-up. Differences under 200 KiB are run-to-run
noise (docs/dependencies.md).

| Candidate (locked version) | Peak footprint (KiB) | Over baseline (KiB) | Stripped probe binary (KiB) | Crates in tree | cargo deny | Parse+walk 10 cmds, median of 5 |
|---|---|---:|---:|---:|---:|---|
| (none) | 1,008 | 0 | 315 (322,576 B) | 1 | n/a | n/a |
| tree-sitter-bash 0.25.1 + tree-sitter 0.27.1 (`shell-ts/`) | 1,072 | +64 (~0, under the noise floor) | 1,786 (1,829,088 B) | 10 | see below | 0.061 ms, 136 nodes |
| brush-parser 0.4.0 (feature `brush-parser`) | 1,232 | +224 | 758 (776,400 B) | 68 | see below | 0.059 ms, 12,378 Debug bytes |

- Nodes: tree-sitter counts real tree nodes. brush-parser's AST has no
  generic walker, so its workload walks every node through the derived
  `Debug` formatter and counts its bytes (12,378); the two node figures are
  not comparable with each other.
- Crates: unique package names from `cargo tree -e normal --prefix none`
  (probe root included, `( *)` duplicates collapsed). shell-ts: shell-ts,
  tree-sitter, tree-sitter-bash, tree-sitter-language, streaming-iterator,
  regex, memchr, aho-corasick, regex-automata, regex-syntax.
  brush-parser's 68 include bon, cached, insta, peg, pest, darling x2,
  syn x2 (v2 and v3), serde (derive, rc), stacker, psm and the rest.
- cargo deny (0.20.2, installed): advisories, bans and sources pass for
  both crates. `licenses` fails only on the unlicensed probe roots
  (`dependency-rss`, `shell-ts`); that failure pre-exists these candidates
  (checked with the change stashed). Neither candidate adds a deny failure.
- Misparses: none in either candidate. All ten commands parse with no
  error (tree-sitter `error=false` on every root; brush-parser `Ok` on all
  ten). Summaries per command are below.

## Growth of Fiber's own binary

Native macOS arm64 release (`cargo build --release --bin fiber`),
stripped. Baseline (branch head, Fiber code untouched): 13,165,112 bytes.
Each candidate was then called from the shell tool's recogniser
(`crates/tools/src/shell/classify.rs`, a live `black_box` call on the
command being classified, so the parser is really linked) and the binary
rebuilt and stripped. Linkage confirmed by grammar strings present only in
the probe binaries (`command_substitution` for tree-sitter, `SimpleCommand`
for brush). All Fiber changes reverted after measuring; none committed.

| Build | Stripped bytes | Growth |
|---|---|---:|
| baseline | 13,165,112 | — |
| + tree-sitter-bash 0.25.1 / tree-sitter 0.27.1 probe call | 14,619,464 | +1,454,352 B (+1,420 KiB) |
| + brush-parser 0.4.0 probe call | 13,347,208 | +182,096 B (+178 KiB) |

The probes link slightly more than a real recogniser would (e.g. the brush
probe formats the AST with `Debug`); treat growth as an upper bound. Linux
figures were not measured: that needs the CI runners
(`gh workflow run dependency-probe.yml`); the 24 MiB cap gates the Linux
musl binary, not this macOS one.

## Per-candidate notes

- tree-sitter-bash 0.25.1: last release 2025-12-02. Licence MIT. Its
  `bindings/rust/build.rs` compiles the grammar's C through `cc`: needs a C
  compiler. Runtime `tree-sitter` 0.27.1 (last release 2026-10-08, MIT)
  also builds C (`binding_rust/build.rs` + `cc`; its `bindgen`
  build-dependency is optional and off). Its only Rust runtime dep is
  `tree-sitter-language` 0.1.9.
- brush-parser 0.4.0: last release 2026-05-03. Licence MIT. `build = false`
  itself, but its tree is not pure Rust: it depends unconditionally on
  `insta` (a snapshot-testing crate), which pulls `pest`, which pulls
  `stacker` + `psm` (build.rs + `cc`, arch-specific assembly).
- Structural blocker: tree-sitter-bash cannot be a feature of
  `research/dependency-rss`. `tree-sitter` 0.27 and the probe's
  `arborium-tree-sitter` both declare `links = "tree-sitter"`, which cargo
  refuses to resolve in one manifest, so the candidate lives in the
  standalone `research/dependency-rss/shell-ts/` crate (same workload, same
  method, wired into `run.sh` as its own row). Fiber itself has no
  `links = "tree-sitter"` crate today, so this would not block admitting
  it; it only shapes the probe.

## Method

- Machine: Mac15,6, Apple M3 Pro, Darwin arm64 (macOS).
- Toolchain: rustc 1.99.0 (b940084d7 2026-09-28). Date: 2026-10-13.
- Memory: `research/dependency-rss/run.sh` method (median of 5 runs over
  the featureless program); tree-sitter-bash via `shell-ts/` with the same
  functions. Timing: each probe binary run 5 times; median of the printed
  `parse_walk_ms` (one warm-up pass inside the process, timed pass over all
  ten commands). Per-command summaries: `SHELL_DUMP=1` on the same
  binaries, one run.
- The ten commands: `ls -la`, `git status --short`,
  `grep -rn "foo bar" src/`, `cd crates/tui && git log --oneline -20`,
  `cat README.md | head -50`, `FOO=1 cargo test -p tui 2>&1 | tail -20`,
  `find . -name '*.rs' -exec wc -l {} +`, `echo "$(git rev-parse HEAD)"`,
  `(cd docs; rg -n 'cap' *.md) > /dev/null`,
  `for f in *.md; do wc -l "$f"; done`.

## Raw per-command node summaries

tree-sitter-bash (`cmd`, root `error=`, node-kind histogram):

```text
cmd="ls -la" error=false commandx1 command_namex1 programx1 wordx2
cmd="git status --short" error=false commandx1 command_namex1 programx1 wordx3
cmd="grep -rn \"foo bar\" src/" error=false "x2 commandx1 command_namex1 programx1 stringx1 string_contentx1 wordx3
cmd="cd crates/tui && git log --oneline -20" error=false &&x1 commandx2 command_namex2 listx1 numberx1 programx1 wordx5
cmd="cat README.md | head -50" error=false commandx2 command_namex2 numberx1 pipelinex1 programx1 wordx3 |x1
cmd="FOO=1 cargo test -p tui 2>&1 | tail -20" error=false =x1 >&x1 commandx2 command_namex2 file_descriptorx1 file_redirectx1 numberx3 pipelinex1 programx1 redirected_statementx1 variable_assignmentx1 variable_namex1 wordx5 |x1
cmd="find . -name '*.rs' -exec wc -l {} +" error=false commandx1 command_namex1 concatenationx1 programx1 raw_stringx1 wordx9
cmd="echo \"$(git rev-parse HEAD)\"" error=false "x2 $(x1 )x1 commandx2 command_namex2 command_substitutionx1 programx1 stringx1 wordx4
cmd="(cd docs; rg -n 'cap' *.md) > /dev/null" error=false (x1 )x1 ;x1 >x1 commandx2 command_namex2 file_redirectx1 programx1 raw_stringx1 redirected_statementx1 subshellx1 wordx6
cmd="for f in *.md; do wc -l \"$f\"; done" error=false "x2 $x1 ;x2 commandx1 command_namex1 dox1 do_groupx1 donex1 forx1 for_statementx1 inx1 programx1 simple_expansionx1 stringx1 variable_namex2 wordx3
```

brush-parser (`cmd`, `ok`, Debug bytes, variant-name histogram extracted
from the `Debug` output, so span/option wrappers and interpolated word text
appear as entries):

```text
cmd="ls -la" ok debug_bytes=572 AndOrListx1 CommandSuffixx1 CompoundListx1 CompoundListItemx1 Nonex2 Pipelinex1 Programx1 Sequencex1 Simplex1 SimpleCommandx1 Somex4 SourcePositionx4 SourceSpanx2 Wordx3
cmd="git status --short" ok debug_bytes=751 AndOrListx1 CommandSuffixx1 CompoundListx1 CompoundListItemx1 Nonex2 Pipelinex1 Programx1 Sequencex1 Simplex1 SimpleCommandx1 Somex5 SourcePositionx6 SourceSpanx3 Wordx5
cmd="grep -rn \"foo bar\" src/" ok debug_bytes=920 AndOrListx1 CommandSuffixx1 CompoundListx1 CompoundListItemx1 Nonex2 Pipelinex1 Programx1 Sequencex1 Simplex1 SimpleCommandx1 Somex6 SourcePositionx8 SourceSpanx4 Wordx7
cmd="cd crates/tui && git log --oneline -20" ok debug_bytes=1397 Andx1 AndOrListx1 CommandSuffixx2 CompoundListx1 CompoundListItemx1 Nonex4 Pipelinex2 Programx1 Sequencex1 Simplex2 SimpleCommandx2 Somex10 SourcePositionx12 SourceSpanx6 Wordx10
cmd="cat README.md | head -50" ok debug_bytes=1005 AndOrListx1 CommandSuffixx2 CompoundListx1 CompoundListItemx1 Nonex3 Pipelinex1 Programx1 READMEx1 Sequencex1 Simplex2 SimpleCommandx2 Somex8 SourcePositionx8 SourceSpanx4 Wordx6
cmd="FOO=1 cargo test -p tui 2>&1 | tail -20" ok debug_bytes=1987 AndOrListx1 Assignmentx1 AssignmentWordx1 CommandPrefixx1 CommandSuffixx2 CompoundListx1 CompoundListItemx1 Duplicatex1 DuplicateOutputx1 FOOx2 Filex1 IoRedirectx1 Nonex3 Pipelinex1 Programx1 Scalarx1 Sequencex1 Simplex2 SimpleCommandx2 Somex14 SourcePositionx18 SourceSpanx9 VariableNamex1 Wordx13
cmd="find . -name '*.rs' -exec wc -l {} +" ok debug_bytes=1755 AndOrListx1 CommandSuffixx1 CompoundListx1 CompoundListItemx1 Nonex2 Pipelinex1 Programx1 Sequencex1 Simplex1 SimpleCommandx1 Somex11 SourcePositionx18 SourceSpanx9 Wordx17
cmd="echo \"$(git rev-parse HEAD)\"" ok debug_bytes=598 AndOrListx1 CommandSuffixx1 CompoundListx1 CompoundListItemx1 HEADx1 Nonex2 Pipelinex1 Programx1 Sequencex1 Simplex1 SimpleCommandx1 Somex4 SourcePositionx4 SourceSpanx2 Wordx3
cmd="(cd docs; rg -n 'cap' *.md) > /dev/null" ok debug_bytes=1971 AndOrListx3 CommandSuffixx2 Compoundx1 CompoundListx2 CompoundListItemx3 Filex1 Filenamex1 Nonex6 Pipelinex3 Programx1 RedirectListx1 Sequencex3 Simplex2 SimpleCommandx2 Somex12 SourcePositionx16 SourceSpanx8 Subshellx1 SubshellCommandx1 Wordx11 Writex1
cmd="for f in *.md; do wc -l \"$f\"; done" ok debug_bytes=1422 AndOrListx2 CommandSuffixx1 Compoundx1 CompoundListx2 CompoundListItemx2 DoGroupCommandx1 ForClausex1 ForClauseCommandx1 Nonex4 Pipelinex2 Programx1 Sequencex2 Simplex1 SimpleCommandx1 Somex7 SourcePositionx12 SourceSpanx6 Wordx6
```
