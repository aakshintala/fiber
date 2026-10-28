use std::path::Path;

use contract::shapes::Effect;
use contract::tool::Effects;

use super::super::classify::classify;

fn classified(command: &str) -> Effects {
    classify(command, Path::new("/work"), Path::new("/work"), &[])
}

fn assert_reads(command: &str) {
    let effects = classified(command);
    assert_eq!(effects.declared.effects, vec![Effect::Reads], "{command}");
    assert!(effects.declared.reversible, "{command}");
}

fn assert_reads_paths(command: &str, paths: &[&str]) {
    assert_reads(command);
    let paths = paths.iter().map(|path| (*path).to_owned()).collect();
    assert_eq!(classified(command).declared.paths, Some(paths), "{command}");
}

fn assert_closed(command: &str) {
    let effects = classified(command);
    assert_eq!(
        effects.declared.effects,
        vec![Effect::Executes],
        "{command}"
    );
    assert!(!effects.declared.reversible, "{command}");
    assert!(effects.declared.paths.is_none(), "{command}");
}

#[test]
fn allowed_assignment_names_read_as_a_prefix() {
    // Every name on the fixed list (`docs/tools.md`, "Shell", "Effects")
    // keeps a read-only inner command read-only.
    for name in [
        "RUST_BACKTRACE",
        "LANG",
        "LC_ALL",
        "TZ",
        "TERM",
        "CI",
        "GIT_TERMINAL_PROMPT",
    ] {
        assert_reads(&format!("{name}=1 ls"));
    }
    // A quoted value cooks before the name check, so it reads too.
    assert_reads("LC_ALL=\"C\" ls");
    assert_reads_paths("LC_ALL=C ls f", &["/work/f"]);
}

#[test]
fn unlisted_assignment_names_execute_as_a_prefix_and_under_env() {
    // Any other name makes the call `executes`, whether it leads the call
    // or follows `env`.
    for name in [
        "RUST_BACKTRACEX",
        "rust_backtrace",
        "PATH",
        "LD_PRELOAD",
        "PAGER",
        "GIT_CONFIG_COUNT",
        "GIT_EDITOR",
    ] {
        assert_closed(&format!("{name}=1 ls x"));
        assert_closed(&format!("env {name}=1 ls x"));
    }
    assert_closed("GIT_CONFIG_COUNT=1 git log");
    assert_closed("env GIT_CONFIG_GLOBAL=x git log");
}

#[test]
fn timeout_durations_read_and_anything_else_executes() {
    for duration in ["5", "5s", "1.5m", "2h", "1d"] {
        assert_reads(&format!("timeout {duration} ls"));
    }
    // A flag, a missing integer, a missing fraction, an exponent, a hex
    // prefix and an empty word are all refused as durations.
    for duration in ["5x", "-5", ".5", "5.", "1e3", "0x5", "\"\""] {
        assert_closed(&format!("timeout {duration} ls"));
    }
    assert_reads_paths("timeout 5 ls f", &["/work/f"]);
}

#[test]
fn timeout_needs_a_bare_duration_and_a_command() {
    for command in [
        "timeout",
        "timeout 5",
        "timeout -k 1 5 ls",
        "timeout -i ls",
        "timeout 5 -k ls",
    ] {
        assert_closed(command);
    }
}

#[test]
fn env_needs_allowed_assignments_and_a_command() {
    // Bare `env`, a flag of its own, and assignments with no command all
    // execute, even when every assignment names an allowed variable.
    for command in [
        "env",
        "env -i ls",
        "env -u X ls",
        "env -S ls",
        "env -- ls",
        "env FOO=1",
        "env LC_ALL=C",
    ] {
        assert_closed(command);
    }
    assert_reads("env LC_ALL=C ls");
    assert_reads("env LC_ALL=\"C\" ls");
    assert_reads("env LC_ALL=C git status");
    assert_reads_paths("env LC_ALL=C ls f", &["/work/f"]);
}

#[test]
fn xargs_reads_only_into_six_commands() {
    // A target that never opens files reads on its own. A path-reading
    // target reads only on proven static stdin: each word must resolve
    // inside a canonical running directory. Over the fake workdir every
    // relative word resolves lexically, so `echo f` proves (declaring
    // the file); the symlink escape needs a real directory and lives in
    // `xargs_with_static_stdin_words_declares_canonical_paths`.
    for command in [
        "xargs echo a",
        "xargs printf %s a",
        "echo /path/to/credentials | xargs echo",
        "echo f | xargs head",
        "echo /work/f | xargs head",
    ] {
        assert_reads(command);
    }
    assert_reads_paths("echo f | xargs head", &["/work", "/work/f"]);
    // Any other target, a path-qualified target, a flag of `xargs`'s own
    // and a bare `xargs` all execute, as does the target's unlisted flag.
    // So does a path-reading target on unproven stdin: terminal stdin, a
    // redirect file of its own, an absolute word outside the running
    // directories, a `printf` producer (direct `printf` still executes),
    // or a producer that is not plain `echo` or `printf` (`ls`, `find`,
    // `git ls-files`, `cat`).
    for command in [
        "xargs",
        "xargs git status",
        "xargs rm f",
        "xargs sh -c x",
        "xargs find .",
        "xargs /usr/bin/wc -l f",
        "xargs -0 wc -l f",
        "xargs -n1 wc f",
        "xargs -I{} wc f",
        "xargs grep -R x f",
        "xargs wc -l f",
        "xargs grep x f",
        "xargs head -5 f",
        "xargs tail -3 f",
        "xargs head < f",
        "git ls-files | xargs wc -l",
        "ls | xargs head",
        "ls | xargs wc -l",
        "ls sub | xargs head",
        "ls sub | xargs head < f",
        "find . -name x | xargs grep y f",
        "cat f | xargs head",
        "printf %s f | xargs head",
        "printf '%s%s' cred ential | xargs head",
        "echo /path/to/credentials | xargs head",
        // A redirect file of its own replaces the pipeline: the proven
        // words never reach the target, so the call is reviewed.
        "echo f | xargs head < g",
        // A word the pipeline would split is never proven: `xargs` feeds
        // `a` and `b`, neither of which the call declares. A leading `-`
        // reads as a flag at runtime, so an option-like word fails the
        // whole call even beside plain words.
        "echo \"a b\" | xargs head",
        "echo -R needle dir | xargs grep",
    ] {
        assert_closed(command);
    }
    // Plain static words still read with every word declared.
    assert_reads_paths(
        "echo needle dir | xargs grep",
        &["/work", "/work/needle", "/work/dir"],
    );
}

#[test]
fn xargs_with_static_stdin_words_declares_canonical_paths() {
    // Only statically-known stdin words count as proven: each is resolved
    // exactly as the credential deny holds it, and each must land inside
    // a canonical running directory. A symlink escaping the workspace
    // fails the whole call, while a static in-workspace file reads with
    // the file declared.
    use std::os::unix::fs::symlink;

    let dir = fakes::TempDir::new("fiber-shell-xargs-stdin");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("f"), "words\n").unwrap();
    std::fs::write(dir.path().join("outside"), "elsewhere\n").unwrap();
    symlink(dir.path().join("outside"), workspace.join("link")).unwrap();
    let workdir = workspace.to_str().unwrap().to_owned();
    let root = dir.path().to_path_buf();
    let classified = |command: &str| classify(command, &root, Path::new(&workdir), &[]);
    let canonical = workspace.canonicalize().unwrap();
    let file = canonical.join("f").to_str().unwrap().to_owned();

    let effects = classified("echo f | xargs head");
    assert_eq!(effects.declared.effects, vec![Effect::Reads], "echo f");
    assert_eq!(
        effects.declared.paths,
        Some(vec![workdir.clone(), file]),
        "echo f"
    );
    for command in ["echo link | xargs head", "ls | xargs head"] {
        let effects = classified(command);
        assert_eq!(
            effects.declared.effects,
            vec![Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
}

#[test]
fn an_xargs_stage_counts_toward_the_wrapper_depth_cap() {
    // The unwrap loop caps `timeout` wrappers and the `xargs` step
    // together: three wrappers read with the proven word declared, the
    // fourth wrapper is the fifth step and the call is reviewed.
    assert_reads_paths(
        "echo f | timeout 1 timeout 1 timeout 1 xargs head",
        &["/work", "/work/f"],
    );
    assert_closed("echo f | timeout 1 timeout 1 timeout 1 timeout 1 xargs head");
}

#[test]
fn nested_wrappers_read_to_four_then_execute() {
    assert_reads("timeout 1 ls");
    assert_reads("timeout 1 timeout 1 ls");
    assert_reads("timeout 1 timeout 1 timeout 1 ls");
    assert_reads("timeout 1 timeout 1 timeout 1 timeout 1 ls");
    assert_closed("timeout 1 timeout 1 timeout 1 timeout 1 timeout 1 ls");
    assert_reads("env LC_ALL=C timeout 5 ls");
    assert_closed("timeout 1 env FOO=1 ls");
}

#[test]
fn find_exec_forms_still_execute() {
    for command in [
        "find . -exec grep x {} \\;",
        "find . -exec echo {} \\;",
        "find . -delete",
    ] {
        assert_closed(command);
    }
}

#[test]
fn a_wrapped_git_after_a_cd_executes() {
    for command in [
        "cd x && LC_ALL=C git status",
        "cd x && timeout 5 git log",
        "cd x && env LC_ALL=C git status",
        "cd x && xargs git status",
    ] {
        assert_closed(command);
    }
}

#[test]
fn an_assignment_prefix_before_cd_reads_through_the_cd_path() {
    // An allow-listed assignment prefix is not a subprocess: `LC_ALL=C cd`
    // uses the `cd` verdict and preserves directory tracking for later
    // commands. An `env` or `timeout` wrapper around `cd` moves nothing
    // the directory states track, so the call is reviewed.
    assert_reads_paths("LC_ALL=C cd x", &["/work/x"]);
    assert_reads("LC_ALL=C cd sub && ls");
    assert_reads_paths("LC_ALL=C cd sub && ls f", &["/work/sub", "/work/sub/f"]);
    for command in ["timeout 5 cd x", "env LC_ALL=C cd x"] {
        assert_closed(command);
    }
}
