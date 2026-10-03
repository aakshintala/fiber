# Is clap's dynamic completion stable enough to depend on?

Research for #451, part of #1. Written 2026-10-03.

## Answer

The feature flag is a long-lived gate on a working feature, not a sign of an unstable one. The maintainer has not declared it stable, though, and the tracking issue still has open items. Recommendation: pin and wrap. Use it for value completion behind one Fiber module, pin the version, and keep static completion as the fallback. Revisit when the flag is removed or the next breaking change lands.

## What dynamic completion means

Static completion is a script generated once. It lists every command and flag, and the shell reads that list when you press Tab. It knows nothing that changes on your machine. Dynamic completion works the other way: the script is small, and each time you press Tab the shell runs the program itself and asks it what could come next. That is how `fiber extension remove <Tab>` could list the extensions you have installed, or `--model <Tab>` could list model ids. The cost is one program start per Tab press. The clap feature that does this is called `unstable-dynamic` and is a switch in the `clap_complete` crate.

## Findings

### The flag is old, and the label says "not declared stable", not "broken"

- The engine was added in clap_complete 3.1.3 (2022-04-30) "behind `unstable-dynamic` feature flag" ([CHANGELOG](https://github.com/clap-rs/clap/blob/master/clap_complete/CHANGELOG.md)). The flag is four years old.
- The tracking issue is [clap#3166, "Stablize Rust-Native Completion Engine Tracking Issue"](https://github.com/clap-rs/clap/issues/3166). It is open and was last updated 2026-06-16. Clap's contributing guide says a feature starts behind an `unstable-<name>` flag with a stabilisation tracking issue ([CONTRIBUTING.md](https://github.com/clap-rs/clap/blob/master/CONTRIBUTING.md), line 62), so the flag is the project's normal pre-stable state.
- The issue body is the maintainer's checklist. Still unchecked: handling of spaces ([#5587](https://github.com/clap-rs/clap/issues/5587)), non-UTF-8 paths, quoting rules per shell, the Elvish registration script ([#5729](https://github.com/clap-rs/clap/issues/5729)), and lazy loading ([#5668](https://github.com/clap-rs/clap/issues/5668)). It also says the feature "shouldn't be behind a feature flag for forever" and that its dependencies differ from the static code, so it must find a home.
- I found no maintainer statement that sets a date or a single blocker for stabilisation. The nearest is the maintainer (epage) on 2026-02-05 pointing users to Cargo's use of it "to get a feel for what this is capable of" ([comment](https://github.com/clap-rs/clap/issues/3166#issuecomment-3855664023)).
- The crate's own docs carry a warning: the interface between the generated shell code and the program is unstable, so re-source completions on upgrade and generate them at shell start rather than saving them to a file ([env module docs](https://github.com/clap-rs/clap/blob/master/clap_complete/src/env/mod.rs)). Fiber's planned `source <(fiber completion zsh)` form already follows that advice.

### API churn: heavy in 2024, additive since

The [CHANGELOG](https://github.com/clap-rs/clap/blob/master/clap_complete/CHANGELOG.md) shows the dynamic API was reshaped between 4.5.13 and 4.5.31 (2024-08-08 to 2024-10-02). Renames and removals in that window:

- `CustomCompleter` became `ValueCandidates`, then `ArgValueCandidates` took the old `ArgValueCompleter` name (4.5.14, 4.5.20).
- The `dynamic` module became `engine`; `command` and `env` moved out of it (4.5.17, 4.5.19).
- `CompleteCommand` was removed, and the binary that gets called became `args_os()[0]` (4.5.25, 4.5.28).
- `CompletionCandidate::get_content` became `get_value` (4.5.23).
- `CompleteEnv::with_factory` now takes a `Fn`, not a `FnOnce` (4.5.31, 2024-10-02).
- Behaviour changes in 4.5.29 and 4.5.30 (ordering, no default path completion).

Since 4.5.32 (2024-10-02) there are 48 releases through 4.6.11 and the changelog shows no removal or rename of a dynamic item. The changes are additions (`ValueCompleter::complete_at`, `PossibleValue` helpers in 4.6.8, `SubcommandCandidates`) and bug fixes. The 4.6.0 release only raised the minimum Rust version to 1.85. I read the changelog headings, not the diffs, so a silent behaviour change could still hide in a fix entry.

### Open bugs

Of the 67 open issues labelled `A-completion` on 2026-10-03, most concern static completion. I read the bodies of the ones below and they concern the dynamic engine. I did not read every issue, so this list may be incomplete.

- zsh: [#5856](https://github.com/clap-rs/clap/issues/5856) reports "not enough arguments" (open since 2024-12). [#6365](https://github.com/clap-rs/clap/issues/6365) says `~/<Tab>` is quoted to `\~/` and breaks home-directory expansion.
- bash: [#6280](https://github.com/clap-rs/clap/issues/6280) asks for `COMP_WORDBREAKS` handling. [#6107](https://github.com/clap-rs/clap/issues/6107) reports wrong behaviour when completing mid-line after `sudo`. The bash and zsh sides share the space and `=` problems in [#5587](https://github.com/clap-rs/clap/issues/5587).
- fish: no open dynamic-only bug found. [#6196](https://github.com/clap-rs/clap/issues/6196) (binary path with a space) is closed and fixed in 4.5.62. Open fish issues I found ([#6295](https://github.com/clap-rs/clap/issues/6295), positionals missing) use the static generator.
- Elvish: [#5729](https://github.com/clap-rs/clap/issues/5729) says the registration script uses syntax removed in Elvish 0.21. It was reported by Cargo's tests. Elvish is not one of Fiber's three shells.
- PowerShell: the earlier bugs ([#5847](https://github.com/clap-rs/clap/issues/5847), [#6010](https://github.com/clap-rs/clap/issues/6010)) are closed. Not one of Fiber's three shells.
- Cross-shell: sort order ([#6371](https://github.com/clap-rs/clap/issues/6371)), a candidate function cannot see earlier arguments ([#5784](https://github.com/clap-rs/clap/issues/5784), waiting on design), and registering as an external subcommand ([#6173](https://github.com/clap-rs/clap/issues/6173)).

For Fiber's use (a few flat values on bash, zsh and fish), the cross-shell gap that matters is #5784: `--model <Tab>` cannot depend on an earlier `--provider` flag today.

### Who depends on it

All of these set `features = ["unstable-dynamic"]` on clap_complete in their `Cargo.toml` (found with `gh search code 'unstable-dynamic' --filename Cargo.toml`; stars read 2026-10-03):

- [rust-lang/cargo](https://github.com/rust-lang/cargo/blob/master/Cargo.toml) (15.5k stars), pinned to `4.6.0` or newer. It calls `CompleteEnv::with_factory(...).var("CARGO_COMPLETE")` in [src/bin/cargo/main.rs](https://github.com/rust-lang/cargo/blob/master/src/bin/cargo/main.rs). Cargo itself still labels this unstable: it runs only on the nightly channel, and the book lists it as `native-completions` ([tracking issue cargo#14520](https://github.com/rust-lang/cargo/issues/14520)). So cargo's caution is partly cargo's own, but it also shows the maintainers of clap use cargo as their main test.
- [jj-vcs/jj](https://github.com/jj-vcs/jj) (31.9k), uses `CompleteEnv` in `cli/src/cli_util.rs`.
- [casey/just](https://github.com/casey/just) (36.1k), uses `CompleteEnv` with `JUST_COMPLETE`, and pins the exact version `=4.6.8`. The maintainer of just asked about value completion hooks on the tracking issue ([comment](https://github.com/clap-rs/clap/issues/3166#issuecomment-3855445187)).
- [rust-lang/rustup](https://github.com/rust-lang/rustup) (7.1k), uses `CompleteEnv` in `src/cli/rustup_mode.rs`.
- [facebook/buck2](https://github.com/facebook/buck2) (4.5k), [j178/prek](https://github.com/j178/prek), [cachix/devenv](https://github.com/cachix/devenv), [spinframework/spin](https://github.com/spinframework/spin), [Canop/bacon](https://github.com/Canop/bacon), and [fish-shell/fish-shell](https://github.com/fish-shell/fish-shell) (34k stars) list the flag. I did not check how each one calls it.

The first page of results held 100 repositories; I did not count the rest.

## Cost per Tab

Method: a release build of the bench crate in `bench/` (clap 4.6.7, clap_complete 4.6.11, link-time optimisation, stripped). The `remove` subcommand takes a value that completes from the 20 files in a directory, standing in for Fiber home. `bench/bench.sh` runs the same command the generated bash script runs, `COMPLETE=bash _CLAP_COMPLETE_INDEX=2 bin -- bin remove ''`, 500 runs after 50 warm-up runs with hyperfine in no-shell mode (`-N`), so the figures contain no shell fork. The `env` wrapper that sets the variables costs an extra program start, so I measured the startup floor both ways. The "via env" row is the fair comparison.

| Platform | Row | Median | p95 |
| --- | --- | --- | --- |
| macOS 26.6 arm64 (Apple M3 Pro), rustc 1.98.1, hyperfine | startup floor: `bin --version` | 1.6 ms | 1.7 ms |
| same | startup floor through `env` | 3.0 ms | 3.5 ms |
| same | Tab on `remove` (20 values read from disk) | 3.1 ms | 3.7 ms |
| same | Tab on the command name (no values read) | 3.0 ms | 3.5 ms |
| same | shell start: `COMPLETE=bash bin` (prints the script) | 3.1 ms | 3.8 ms |
| Linux x86_64 (GitHub ubuntu-latest) | all rows | pending | pending |

Medians and p95 are from the first of three runs; the other two runs differed by up to 0.4 ms in the median (2.7 to 3.1 ms), so read these as "about 3 ms". Reading twenty directory entries adds nothing measurable above the program start. A real Tab also pays for the shell's own work, which these figures leave out. Fiber's real startup will be larger than this toy, because it reads configuration and links more code, and that cost lands on every Tab. Fiber's own `--version` floor is the number to check before shipping.

To fill the Linux row: copy `bench/` to `research/clap-dynamic-completion/bench/` on a throwaway branch named `completion-bench`, put `bench/bench.yml` at `.github/workflows/bench.yml`, push, and read the `bench.txt` artefact. I wrote the workflow without running it.

## Recommendation: pin and wrap

Why not depend now with no guard: the maintainer has not called it stable, the crate's docs say the script-to-program interface may change, and the 2024 renames show the maintainers will break the API when the design needs it.

Why not wait: the API has been additive for two years, the biggest Rust CLIs use it, and the cost is about the startup floor of the program. Waiting would leave `extension remove <Tab>` and `--model <Tab>` undone, with no sign of a stabilisation date.

What pin and wrap means here:

- Pin `clap_complete` to an exact version, as just does (`=4.6.8`), and move it deliberately.
- Keep every use of the dynamic API (`CompleteEnv`, `ArgValueCandidates`) in one Fiber module. The candidate functions are plain Rust functions that read Fiber home and know nothing about clap.
- Keep `fiber completion <shell>`. The registration script it prints can come from the same crate, because `env::Shells` and `EnvCompleter::write_registration` are public. I did not build this. The static generator stays as the fallback if the dynamic path breaks.
- Test it by running the real shells, as the crate's own `unstable-shell-tests` feature does, not by checking that a file exists.

Revisit trigger: any one of these.

- clap_complete removes the `unstable-dynamic` flag, or the tracking issue #3166 closes. Then drop the pin and the wrapper.
- A release removes or renames a dynamic item, or the CHANGELOG records a breaking dynamic change. Then re-read before bumping.
- The Linux row, or Fiber's real startup floor, shows a Tab costing more than about 50 ms. That figure is picked, not sourced: Fiber has no completion budget in `docs/performance.md`. Then add the lazy approach in [#5668](https://github.com/clap-rs/clap/issues/5668) or cache the value list.

## Claims I am less sure of

- That the 67 open `A-completion` issues are mostly static: I classified only the issues I opened.
- That no dynamic-only fish bug is open: I read a sample, not all.
- That nothing broke silently after 4.5.32: I read changelog headings, not code diffs.
- That adopters other than cargo, jj, just and rustup call the dynamic API: I only saw the Cargo.toml feature line.
