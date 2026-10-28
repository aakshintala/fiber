use std::collections::BTreeSet;
use std::path::Path;

use contract::shapes::Effect;
use contract::tool::Effects;

use super::super::read_only::COMMANDS;
use super::classify;

#[test]
fn one_grep_accepts_only_one_plain_builtin_command() {
    for command in [
        "grep x a.txt",
        "grep -n x a.txt 2>/dev/null",
        "grep x a.txt 2>&1",
        "'grep' x a.txt",
        "\"grep\" x a.txt",
        "grep",
        // An assignment prefix still runs grep, and an empty operand word
        // does not disqualify the call.
        "FOO=1 grep x a",
        "grep '' x",
    ] {
        assert!(super::one_grep(command), "{command}");
    }
    for command in [
        "",
        // A bare redirect names no command.
        "2>/dev/null",
        "cat a | grep x",
        "grep x a; ls",
        "grep x a && ls",
        "grep x a || ls",
        "command grep x a",
        "/usr/bin/grep x a",
        "rg x",
        "env grep x a",
        "grep x a > out",
        "grep $(echo x) a",
        "grep x *.rs",
        "\\grep x a",
        "echo grep x a",
        "grep x a\ngrep x b",
        "grep x a &",
        "grep x a;",
        "grep x a | grep x a | grep x a",
    ] {
        assert!(!super::one_grep(command), "{command}");
    }
}

#[test]
fn operators_outside_quotes_split_elements() {
    // Every element reads. Each pathless element declares the workdir, so
    // the four-element pipelines declare it four times.
    let four = vec!["/work".to_owned(); 4];
    for command in ["ls && pwd || cat; head | tail", "ls&&pwd||cat;head|tail"] {
        assert_reads(command);
        assert_eq!(
            classified(command).declared.paths,
            Some(four.clone()),
            "{command}"
        );
    }
    assert_reads("echo 'ls && pwd || cat; head | tail'");
    assert_reads("echo \"ls && pwd || cat; head | tail\"");
    assert_reads("echo 'a&&b'&&ls");
}

#[test]
fn characters_outside_the_plain_set_execute() {
    for command in [
        "echo $(x)",
        "echo `x`",
        "echo <(x)",
        "echo >(x)",
        "echo >f",
        "echo (x)",
        "echo x)",
        "echo a & echo b",
        "echo a&echo b",
        "ls&",
        "&",
        "echo !",
        "echo hello!",
        "echo # comment",
        "echo foo #bar",
        "echo ~",
        "echo ~/x",
        "echo a~b",
        "echo a\\b",
        "echo *",
        "echo a*b",
        "echo a?",
        "echo file?",
        "echo [a]",
        "echo {a}",
        "echo {",
        "echo }",
        "echo a\nb",
        "echo a\rb",
        "echo a\u{0001}b",
        "echo a\u{007f}b",
        "echo caf\u{e9}",
        "echo \u{3bb}",
        "echo a\u{00a0}b",
        "echo 'unterminated",
        "echo \"unterminated",
        "echo \"a$b\"",
        "echo \"a`b\"",
        "echo \"\\$\"",
        "echo \"\\`\"",
        "echo \"a\\\"",
        "echo \"a\\\n b\"",
        "ls ;; ls",
        "ls;;ls",
        ";;",
        "ls |& ls",
        "|&",
        "ls&&&ls",
        "&& ls",
        "ls &&",
        "ls ||",
        "ls ;",
        "ls |",
        "| ls",
        "; ls",
        "|| ls",
        "ls && && ls",
        "ls || || ls",
        "ls | | ls",
        "ls ; ; ls",
        "ls&&&&ls",
        "",
        "   ",
        "\t",
    ] {
        assert_closed(command);
    }
}

#[test]
fn an_input_redirect_declares_its_file() {
    // `echo <f` opens `f` for stdin where the bare call declares nothing;
    // a `/proc` file executes.
    assert_reads("echo <f");
    assert_eq!(
        classified("echo <f").declared.paths,
        Some(vec!["/work/f".to_owned()])
    );
    assert_closed("cat </proc/version");
}

#[test]
fn a_stderr_redirect_reads_at_each_boundary() {
    assert_reads("ls 2>/dev/null");
    assert_reads("ls 2>/dev/null | head");
    assert_reads("ls 2>/dev/null\t| head");
    assert_reads("ls 2>/dev/null||head");
    assert_reads("ls 2>/dev/null&&pwd");
    assert_reads("ls 2>/dev/null;pwd");
    assert_reads("ls 2>&1");
    assert_reads("git 2>&1 status");
    // A redirect stands anywhere, and a quoted redirect-like word is an
    // operand naming its own path.
    assert_reads("2>/dev/null ls");
    assert_reads("2>&1 ls");
    assert_reads("git 2>/dev/null status");
    assert_eq!(
        classified("cat '2>&1'").declared.paths,
        Some(vec!["/work/2>&1".to_owned()])
    );
}

#[test]
fn refused_redirects_execute() {
    for command in [
        "ls 12>/dev/null",
        "ls 2>>/dev/null",
        "ls 2>&-",
        "ls 2>/dev/nullx",
        "ls 2>&12",
        "echo 2>(x)",
        "ls 2>/dev/null/x",
        "ls 2>/dev/zero",
        "ls 2>file",
        "ls >file",
        "ls &>file",
        "ls 2>\tfile",
    ] {
        assert_closed(command);
    }
    // A descriptor the allow-list blesses reads, however it is spelled.
    // Merging into a captured stream changes nothing the runner sees.
    for command in [
        "ls 1>/dev/null",
        "ls >/dev/null",
        "ls 2> /dev/null",
        "ls 2>&2",
    ] {
        assert_reads(command);
    }
    // A word before the redirect stays an operand naming its own path.
    assert_eq!(
        classified("ls x2>/dev/null").declared.paths,
        Some(vec!["/work/x2".to_owned()])
    );
    assert_eq!(
        classified("ls \"2\">/dev/null").declared.paths,
        Some(vec!["/work/2".to_owned()])
    );
}

fn classified(command: &str) -> Effects {
    classify(command, Path::new("/work"), &[])
}

fn configured(name: &str, flags: &[&str]) -> (String, Vec<String>) {
    (
        name.to_owned(),
        flags.iter().map(|flag| (*flag).to_owned()).collect(),
    )
}

fn classified_with(command: &str, entries: &[(String, Vec<String>)]) -> Effects {
    classify(command, Path::new("/work"), entries)
}

fn classified_in(command: &str, workdir: &Path, entries: &[(String, Vec<String>)]) -> Effects {
    classify(command, workdir, entries)
}

fn assert_configured_reads(command: &str, entries: &[(String, Vec<String>)], paths: &[&str]) {
    let effects = classified_with(command, entries);
    assert_eq!(effects.declared.effects, vec![Effect::Reads], "{command}");
    assert!(effects.declared.reversible, "{command}");
    let paths = paths.iter().map(|path| (*path).to_owned()).collect();
    assert_eq!(effects.declared.paths, Some(paths), "{command}");
}

fn assert_reads(command: &str) {
    let effects = classified(command);
    assert_eq!(effects.declared.effects, vec![Effect::Reads], "{command}");
    assert!(effects.declared.reversible, "{command}");
}

/// The paths a `cd` call declares, as a membership: the doc promises
/// which directories each element runs in, not their order.
fn assert_declares(command: &str, paths: &[&str]) {
    assert_reads(command);
    let declared = classified(command).declared.paths.unwrap_or_default();
    let actual: BTreeSet<&str> = declared.iter().map(String::as_str).collect();
    assert_eq!(
        actual,
        BTreeSet::from_iter(paths.iter().copied()),
        "{command}"
    );
}

/// The network a `gh` read declares: the default GitHub API endpoint,
/// in the shape `web_fetch` declares it.
fn assert_network(command: &str, entries: &[(String, Vec<String>)], paths: &[&str]) {
    let effects = classified_with(command, entries);
    assert_eq!(effects.declared.effects, vec![Effect::Network], "{command}");
    assert_eq!(
        effects.hosts,
        Some(vec!["https://api.github.com/".to_owned()]),
        "{command}"
    );
    let paths = paths.iter().map(|path| (*path).to_owned()).collect();
    assert_eq!(effects.declared.paths, Some(paths), "{command}");
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
fn new_git_forms_read() {
    for command in [
        "git log --oneline -8",
        "git log -1",
        "git log -10",
        "git log -p",
        "git show --stat HEAD",
        "git status",
        "git diff",
        "git rev-parse HEAD",
        "git rev-parse --show-toplevel",
        "git rev-parse --abbrev-ref HEAD",
        "git ls-files src",
        "git blame -L 10,20 f",
        "git branch --show-current",
        "git branch '--show-current'",
    ] {
        assert_reads(command);
    }
    assert!(classified("git rev-parse HEAD").declared.paths.is_none());
    assert!(
        classified("git branch --show-current")
            .declared
            .paths
            .is_none()
    );
    assert_eq!(
        classified("git ls-files src").declared.paths,
        Some(vec!["/work/src".to_owned()])
    );
    assert_eq!(
        classified("git blame -L 10,20 f").declared.paths,
        Some(vec!["/work/f".to_owned()])
    );
}

#[test]
fn exact_forms_read_without_paths() {
    // `docs/tools.md`, "Shell", "Effects": if every command is on the
    // read-only list, the call declares `reads` with no paths here.
    for command in ["git branch --show-current", "git status", "git stash list"] {
        assert_reads(command);
        assert!(classified(command).declared.paths.is_none(), "{command}");
    }
    // A writing form executes even with a discard redirect: `git init`
    // creates or updates `.git` state, and `git push --dry-run` reaches
    // the remote.
    for command in ["git init", "git push --dry-run"] {
        assert_closed(command);
        assert_closed(&format!("{command} 2>/dev/null"));
    }
    // A discard redirect drops out before the exact match.
    for command in [
        "git branch --show-current 2>/dev/null",
        "git status 2>/dev/null",
    ] {
        assert_reads(command);
        assert!(classified(command).declared.paths.is_none(), "{command}");
    }
}

#[test]
fn nl_numbering_and_layout_flags_read() {
    // The ticket's acceptance command: `nl -ba` over a file, piped
    // through a print-only `sed`. The `sed` with no file declares the
    // workdir beside the numbered file. `docs/tools.md`, "Shell",
    // "Effects" promises the paths each stage names, not their order,
    // so the two paths are asserted as a set (`docs/testing.md`,
    // "Event streams").
    assert_declares(
        "nl -ba crates/main/src/switch.rs | sed -n '1,5p'",
        &["/work/crates/main/src/switch.rs", "/work"],
    );
    for command in [
        "nl f",
        "nl -ba f",
        "nl -b a -n ln -w 6 -s ' ' -v 10 -i 1 f",
        "nl --body-numbering a --number-format ln --number-width 6 f",
        "nl --body-numbering=a --number-format=ln --number-width=6 --number-separator=' ' --starting-line-number=1 --line-increment=1 f",
        "nl -- -b",
    ] {
        assert_reads(command);
    }
    assert_eq!(
        classified("nl -b a -n ln -w 6 -s ' ' -v 10 -i 1 f")
            .declared
            .paths,
        Some(vec!["/work/f".to_owned()])
    );
    assert_eq!(
        classified("nl --body-numbering=a f").declared.paths,
        Some(vec!["/work/f".to_owned()])
    );
    assert_eq!(
        classified("nl -- -b").declared.paths,
        Some(vec!["/work/-b".to_owned()])
    );
    // The credential deny judges declared paths: `-ba` takes no value,
    // so the operand survives as a path.
    assert_eq!(
        classified("nl -ba ../credentials/openrouter")
            .declared
            .paths,
        Some(vec!["/work/../credentials/openrouter".to_owned()])
    );
    // With no operand `nl` reads its standard input, so the call
    // declares the workdir, as `cat` with no operand does.
    assert_eq!(
        classified("nl").declared.paths,
        Some(vec!["/work".to_owned()])
    );
}

#[test]
fn nl_other_forms_are_not_read_only() {
    // An attached short value is not on the list, as with `head -n10`.
    // A cluster mixing a value-taking letter is not one either.
    // A value flag with no value, a long flag with no value, and any
    // unlisted flag make the call `executes`.
    for command in [
        "nl -w3 f",
        "nl -b=t f",
        "nl -bax f",
        "nl -bn f",
        "nl -b",
        "nl --number-width",
        "nl -v",
        "nl -p f",
        "nl -d '\t' f",
        "nl --help",
        "nl --number-width=6 --bogus f",
        "nl /proc/self/environ",
        "nl notes.txt /proc/self/environ",
    ] {
        assert_closed(command);
    }
}

#[test]
fn bare_true_and_colon_read_without_paths() {
    // The ticket's acceptance command: `true` after a read-only list
    // still declares `reads`, with only the earlier part's path.
    assert_reads("cat a 2>/dev/null || true");
    assert_eq!(
        classified("cat a 2>/dev/null || true").declared.paths,
        Some(vec!["/work/a".to_owned()])
    );
    for command in ["true", ":"] {
        assert_reads(command);
        assert!(classified(command).declared.paths.is_none(), "{command}");
    }
    // A discard redirect drops out before the exact match, as with
    // `git status 2>/dev/null`.
    for command in ["true 2>/dev/null", ": 2>/dev/null"] {
        assert_reads(command);
        assert!(classified(command).declared.paths.is_none(), "{command}");
    }
    // Any operand, flag, or writing redirect leaves the bare-only form.
    for command in [
        "true x",
        "true -n",
        "true --not-listed",
        ": x",
        ": -n",
        ": --not-listed",
        ": >f",
        "true f > out",
    ] {
        assert_closed(command);
    }
}

#[test]
fn the_longest_built_in_path_wins() {
    // `git branch --show-current` reads by its own entry: an extra operand
    // still reads with no paths.
    assert_reads("git branch --show-current x");
    assert!(
        classified("git branch --show-current x")
            .declared
            .paths
            .is_none()
    );
    // A configured entry at the same path is never consulted: the exact
    // form reads, and the longer call executes as an owned path.
    let entries = [configured("git stash list", &[])];
    assert_reads("git stash list");
    assert!(
        classified_with("git stash list", &entries)
            .declared
            .paths
            .is_none()
    );
    assert_configured_closed("git stash list extra", &entries);
}

#[test]
fn an_assignment_after_the_name_is_an_operand() {
    // `ls FOO=1` reads the `FOO=1` operand; a leading assignment reads
    // only for a name on the fixed list (#1497).
    assert_eq!(
        classified("ls FOO=1").declared.paths,
        Some(vec!["/work/FOO=1".to_owned()])
    );
    assert_closed("FOO=1 ls");
    let status = classified("LC_ALL=C git status");
    assert_eq!(status.declared.effects, vec![Effect::Reads]);
    assert!(status.declared.reversible);
    assert!(status.declared.paths.is_none());
}

#[test]
fn only_listed_assignment_names_read() {
    // A leading assignment reads only for a name on the fixed list
    // (#1497): `LC_ALL` reads, while `FOO`, a bare letter matching a
    // takes-value flag such as `head -n`, and any other name execute.
    // `docs/tools.md`, "Shell", "Effects" promises the refused call
    // declares `executes` with no paths.
    assert_reads("LC_ALL=C git status");
    assert_closed("FOO=1 git status");
    assert_closed("n=1 head -5");
}

#[test]
fn hostile_git_forms_execute() {
    for command in [
        "git -c a=b log",
        "git -c diff.external=x diff",
        "git --config-env=a=b log",
        "git --exec-path=/x log",
        "git --exec-path log",
        "git -p log",
        "git --paginate log",
        "git --git-dir=/x log",
        "git --git-dir /x status",
        "git --work-tree=/x status",
        "git log --ext-diff",
        "git show --ext-diff",
        "git diff --ext-diff",
        "git blame --ext-diff",
        "git log --textconv",
        "git show --textconv",
        "git log --output=f",
        "git log --output f",
        "git show --output=f",
        "git log -",
        "git log -8x",
        "git log -x8",
        "git show -8",
        "git diff -8",
        "git rev-parse --short=7",
        "git rev-parse --git-dir",
        "git ls-files --exclude-from=f",
        "git blame --contents f x",
        "git branch",
        "git branch foo",
        "git branch -D foo",
        "git branch -a",
        "git branch --list",
        "git branch --show-current --show-current",
        "git checkout x",
        "git stash",
        "git config x",
        // Writing forms, not read-only ones.
        "git init",
        "git push --dry-run",
    ] {
        assert_closed(command);
    }
}

#[test]
fn gh_read_only_forms_read() {
    // The table tests below prove every flag alone and every row reads;
    // one combined row pins the declared network, host and paths.
    assert_network(
        "gh pr view 123 --json number,title --comments --repo owner/name -R owner/name",
        &[],
        &["/work/123"],
    );
    assert_network("LC_ALL=C gh pr view 123", &[], &["/work/123"]);
}

#[test]
fn gh_host_overrides_execute() {
    // Any host override leaves the default endpoint, so the call is
    // reviewed: an assignment prefix naming a host variable, and the
    // `--hostname` flag, whose value the classifier never sees.
    for command in [
        "GH_HOST=example.com gh pr view 123",
        "GH_ENTERPRISE_TOKEN=token gh pr view 123",
        "env GH_HOST=example.com gh pr view 123",
        "gh pr view 123 --hostname example.com",
    ] {
        assert_closed(command);
    }
}

#[test]
fn other_gh_forms_execute() {
    for command in [
        "gh",
        "gh api repos/owner/name",
        "gh pr merge 123",
        "gh pr create --title x",
        "gh pr view 123 --web",
        "gh pr view -- x",
        "gh -R owner/name pr view 123",
        "gh pr view 123 --repo github.corp/owner/repo",
        "gh pr view 123 --repo=github.corp/owner/repo",
        "gh pr view 123 -R github.corp/owner/repo",
        "gh issue close 123",
        "gh repo clone owner/name",
        "gh auth status",
        "gh run watch 123",
        "curl https://example.com",
    ] {
        assert_closed(command);
    }
    // A configured `gh pr view` adds nothing: the built-in path owns the
    // name. A configured `gh pr status` is honoured: no built-in owns it.
    let view = [configured("gh pr view", &["--web"])];
    assert_configured_closed("gh pr view 123 --web", &view);
    // Only echo stages print statically-known words: a gh stage's output
    // leaves a later `xargs` reader unproven, so the call is reviewed.
    assert_closed("gh pr view 123 | xargs wc -l");
    let status = [configured("gh pr status", &["--json="])];
    // A configured `gh` entry hits the network too, so it declares
    // `Network` with the default host instead of `Reads`.
    assert_network("gh pr status 123", &status, &["/work/123"]);
    assert_network("gh pr status 123 --json number", &status, &["/work/123"]);
    assert_eq!(
        classified_with("gh pr status 123 --web", &status)
            .declared
            .effects,
        vec![Effect::Executes]
    );
}

#[test]
fn find_selection_operators_read() {
    for command in [
        "find . -name \"*.py\" -not -path \"./.git/*\"",
        "find . -path ./.git -prune -o -name \"*.py\" -print",
    ] {
        assert_reads(command);
    }
    for command in [
        "find . -name x -delete",
        "find . \\( -name a -o -name b \\) -exec rm {} \\;",
    ] {
        assert_closed(command);
    }
}

#[test]
fn echo_reads_text_and_only_n_counts_as_a_flag() {
    for command in [
        "echo ---",
        "echo -",
        "echo -x",
        "echo -nx",
        "echo -n",
        "echo -nn",
        "echo --not-listed",
    ] {
        assert_reads(command);
    }
    for command in ["echo -e", "echo -E", "echo -ne"] {
        assert_closed(command);
    }
    assert_closed("pwd ---");
}

#[test]
fn sed_through_the_classifier_declares_its_files() {
    assert_eq!(
        classified("sed -n 1p a b").declared.paths,
        Some(vec!["/work/a".to_owned(), "/work/b".to_owned()])
    );
    assert_eq!(
        classified("sed -n 1p").declared.paths,
        Some(vec!["/work".to_owned()])
    );
    assert_eq!(
        classified("sed -e 1p f").declared.paths,
        Some(vec!["/work/f".to_owned()])
    );
    assert_eq!(
        classified("sed -n 1p /home/me/.fiber/credentials/k")
            .declared
            .paths,
        Some(vec!["/home/me/.fiber/credentials/k".to_owned()])
    );
    assert_reads("sed -n 1p a b");
    assert_reads("'sed' -n 1p f");
    assert_reads("sed -n 1p f 2>/dev/null");
    assert_reads("sed -n 1p f >/dev/null");
    for command in [
        "sed -n 1p /proc/self/environ",
        "/usr/bin/sed -n 1p f",
        "sed -n 1p f > out",
        "sed -n 1p f 2>file",
        "sed -n '1w out' f",
        "sed -i 1p f",
        "sed --in-place 1p f",
    ] {
        assert_closed(command);
    }
}

#[test]
fn the_four_quoted_commands_read() {
    assert_reads("sed -n '320,520p' crates/main/src/switch.rs");
    assert_reads("sed -n 1,120p docs/testing.md 2>/dev/null || ls docs/");
    assert_reads("git log --oneline -8; echo ---; git show --stat HEAD");
    assert_reads(
        "grep -rn \"command_id\\|command_accepted\\|command_rejected\" crates/ --include=\"*.rs\" | head -n 80",
    );
    assert_closed("git log --oneline -8; echo ---; git show --stat HEAD; ...");
    assert_eq!(
        classified(
            "grep -rn \"command_id\\|command_accepted\\|command_rejected\" crates/ --include=\"*.rs\" | head -n 80",
        )
        .declared
        .paths,
        Some(vec!["/work/crates/".to_owned(), "/work".to_owned()])
    );
}

#[test]
fn head_and_tail_count_forms_read() {
    for command in [
        "ls src | head -n 5",
        "ls src | head -5",
        "grep -rn \"def \" src | head -20",
        "tail -n +3 f",
    ] {
        assert_reads(command);
    }
}

#[test]
fn only_pathless_commands_declare_no_paths() {
    for command in COMMANDS {
        let declares = command.name != "echo"
            && command.name != "pwd"
            && command.name != "git rev-parse"
            && command.name != "git status"
            && command.name != "git branch --show-current";
        assert_eq!(command.paths, declares, "{}", command.name);
    }
}

#[test]
fn denied_flags_are_absent_from_every_allowed_list() {
    const DENIED: &[&str] = &[
        "--output",
        "--ext-diff",
        "--textconv",
        "--pre",
        "--compress-program",
        "-exec",
        "-execdir",
        "-ok",
        "-okdir",
        "-delete",
        "-fprint",
        "-z",
    ];
    for command in COMMANDS {
        for flag in command.flags {
            assert!(
                !DENIED.contains(&flag.spelling),
                "{} allows {}",
                command.name,
                flag.spelling
            );
        }
    }
    for (name, spelling) in [
        ("sort", "-o"),
        ("sort", "-T"),
        ("tail", "-f"),
        ("grep", "-f"),
        ("grep", "-R"),
        ("grep", "--dereference-recursive"),
        ("rg", "-L"),
        ("rg", "--follow"),
        ("find", "-L"),
        ("find", "-H"),
        ("find", "-follow"),
        ("ls", "-L"),
    ] {
        let command = COMMANDS
            .iter()
            .find(|command| command.name == name)
            .unwrap();
        assert!(
            command.flags.iter().all(|flag| flag.spelling != spelling),
            "{name} allows {spelling}"
        );
    }
}

#[test]
fn every_listed_command_with_a_plain_operand_reads() {
    for command in COMMANDS {
        // A `gh` command hits the network, so it declares `Network`
        // with the default host instead of `Reads`.
        if command.name.starts_with("gh ") {
            let line = format!("{} x", command.name);
            assert_network(&line, &[], &["/work/x"]);
            continue;
        }
        let line = format!("{} x", command.name);
        assert_reads(&line);
        let paths = classified(&line).declared.paths;
        if command.pattern {
            assert_eq!(paths, Some(vec!["/work".to_owned()]), "{line}");
        } else if command.paths {
            assert_eq!(paths, Some(vec!["/work/x".to_owned()]), "{line}");
        } else {
            assert!(paths.is_none(), "{line}");
        }
    }
}

#[test]
fn each_allowed_flag_keeps_the_command_read_only() {
    for command in COMMANDS {
        // A `gh` command hits the network, so it declares `Network`
        // with the default host instead of `Reads`.
        if command.name.starts_with("gh ") {
            for flag in command.flags {
                let line = if flag.takes_value {
                    format!("{} {} v", command.name, flag.spelling)
                } else {
                    format!("{} {}", command.name, flag.spelling)
                };
                assert_network(&line, &[], &["/work"]);
            }
            continue;
        }
        for flag in command.flags {
            let line = if flag.takes_value {
                format!("{} {} v", command.name, flag.spelling)
            } else {
                format!("{} {}", command.name, flag.spelling)
            };
            assert_reads(&line);
            let paths = classified(&line).declared.paths;
            if command.paths {
                assert_eq!(paths, Some(vec!["/work".to_owned()]), "{line}");
            } else {
                assert!(paths.is_none(), "{line}");
            }
        }
    }
}

#[test]
fn a_flag_value_is_not_a_declared_path() {
    assert_eq!(
        classified("grep -e secret file").declared.paths,
        Some(vec!["/work/file".to_owned()])
    );
    assert_eq!(
        classified("head -n 10 file").declared.paths,
        Some(vec!["/work/file".to_owned()])
    );
    assert_eq!(
        classified("rg --glob '*.rs' pat").declared.paths,
        Some(vec!["/work".to_owned()])
    );
    assert_eq!(
        classified("rg --glob=pat foo file").declared.paths,
        Some(vec!["/work/file".to_owned()])
    );
    assert_eq!(
        classified("grep --include='*.rs' foo file").declared.paths,
        Some(vec!["/work/file".to_owned()])
    );
    assert_eq!(
        classified("grep --include=x foo y").declared.paths,
        Some(vec!["/work/y".to_owned()])
    );
    assert_eq!(
        classified("find src -name foo").declared.paths,
        Some(vec!["/work/src".to_owned()])
    );
    assert_eq!(
        classified("find -newer secret").declared.paths,
        Some(vec!["/work".to_owned()])
    );
    assert_eq!(
        classified("git diff --cached --stat -- src").declared.paths,
        Some(vec!["/work/src".to_owned()])
    );
}

#[test]
fn a_writing_or_executing_flag_is_not_read_only() {
    for command in [
        "git diff --output=f",
        "git diff --output f",
        "git diff --output",
        "git diff '--output=f'",
        "git diff --out'put'=f",
        "git diff \"--output\"=f",
        "git diff --out=f",
        "git diff --outp=f",
        "git diff --out f",
        "git diff --stat=1",
        "sort -o out",
        "sort -o x y",
        "sort -ofile",
        "sort -fo",
        "sort '-o' out",
        "sort --compress-program gzip",
        "sort -T /tmp",
        "rg --pre cmd",
        "rg --pre=cmd",
        "rg --pr cmd",
        "rg '--pre' cmd",
        "rg --'pre'=cmd",
        "rg -z",
        "find -exec rm",
        "find -execdir rm",
        "find -ok rm",
        "find -okdir rm",
        "find -delete",
        "find -fprint out",
        "find '-exec' rm",
        "find '-delete'",
        "tail -f",
        "grep -f names",
        "head -n",
        "head -n10 file",
        "head -qn",
        "grep -A",
        "grep -A3 file",
        "grep -A=3 x",
        "grep --=x y",
        "grep -nA 3 file",
        "cat -",
        "sort -",
        "sort -k",
        "sort -k2 file",
        "sort -t, file",
        "ls --help",
        "ls --color",
        "ls --color=auto",
        "ls -lao",
        "pwd -L",
        "echo -e",
        "echo -ne",
        "cat --number",
        "git status --untracked-files=all",
        "git -C /tmp status",
        "git -c a=b status",
        "/usr/bin/git status",
        "/usr/bin/ls x",
        "FOO=bar ls",
        "ls src && git diff --output=f",
        "ls /etc && sort -o out",
        "echo hi && find -delete",
    ] {
        assert_closed(command);
    }
}

#[test]
fn an_unlisted_flag_is_not_read_only() {
    for command in COMMANDS {
        // bash's `echo` reads any other word as text, so it stays read-only.
        if command.name == "echo" {
            assert_reads("echo --not-listed");
            continue;
        }
        assert_closed(&format!("{} --not-listed", command.name));
        assert_closed(&format!("{} --not-listed=1", command.name));
    }
}

#[test]
fn a_read_only_part_beside_a_closed_part_is_not_read_only() {
    assert_closed("git status && sort -o out");
    assert_closed("pwd && ls --help");
}

#[test]
fn double_dash_ends_flags_only_where_the_command_says_so() {
    for command in [
        "find -- x -delete",
        "find -- x -exec /bin/echo x ';'",
        "find x -- -delete",
    ] {
        assert_closed(command);
        assert!(classified(command).subject.is_some(), "{command}");
    }
    assert_reads("grep -- -x file");
    assert_reads("git diff -- path");
}

#[test]
fn every_command_states_whether_double_dash_ends_flags() {
    let stated = [
        ("pwd", true),
        ("echo", true),
        ("ls", true),
        ("cat", true),
        ("head", true),
        ("tail", true),
        ("wc", true),
        ("grep", true),
        ("rg", true),
        ("find", false),
        ("sort", true),
        ("nl", true),
        ("git status", true),
        ("git branch --show-current", false),
        ("git diff", true),
        ("git log", true),
        ("git show", true),
        ("git rev-parse", true),
        ("git ls-files", true),
        ("git blame", true),
        ("gh pr view", false),
        ("gh pr list", false),
        ("gh pr diff", false),
        ("gh pr checks", false),
        ("gh issue view", false),
        ("gh issue list", false),
        ("gh repo view", false),
        ("gh run list", false),
        ("gh run view", false),
    ];
    assert_eq!(COMMANDS.len(), stated.len());
    for (command, (name, ends_flags)) in COMMANDS.iter().zip(stated) {
        assert_eq!(command.name, name);
        assert_eq!(command.ends_flags, ends_flags, "{name}");
    }
}

#[test]
fn every_part_on_the_list_reads() {
    assert_reads("ls src && git status");
    assert_eq!(
        classified("ls src && git status").declared.paths,
        Some(vec!["/work/src".to_owned()])
    );
    assert_reads("echo hi && pwd");
    assert!(classified("echo hi && pwd").declared.paths.is_none());
    assert_reads("echo -n hi && ls -lah src && git diff --stat");
    assert_eq!(
        classified("echo -n hi && ls -lah src && git diff --stat")
            .declared
            .paths,
        Some(vec!["/work/src".to_owned(), "/work".to_owned()])
    );
    assert_reads("'ls' src");
    assert_reads("git 'status'");
    assert_reads("ls -la");
    assert_eq!(
        classified("ls -la").declared.paths,
        Some(vec!["/work".to_owned()])
    );
    assert_reads("ls -- -l");
    assert_eq!(
        classified("ls -- -l").declared.paths,
        Some(vec!["/work/-l".to_owned()])
    );
    assert_reads("ls");
    assert_eq!(
        classified("ls").declared.paths,
        Some(vec!["/work".to_owned()])
    );
    assert_reads("echo");
    assert!(classified("echo").declared.paths.is_none());
    assert_reads("grep -n -A 3 pattern file");
    assert_eq!(
        classified("grep -n -A 3 pattern file").declared.paths,
        Some(vec!["/work/file".to_owned()])
    );
}

#[test]
fn a_search_pattern_is_not_a_declared_path() {
    for (command, paths) in [
        ("grep -r foo", vec!["/work"]),
        ("grep foo src lib", vec!["/work/src", "/work/lib"]),
        ("grep -- -x file", vec!["/work/file"]),
        ("grep -e foo src", vec!["/work/src"]),
        ("grep src -e foo", vec!["/work/src"]),
        ("rg foo", vec!["/work"]),
        ("rg -n foo src", vec!["/work/src"]),
        ("rg -e foo src", vec!["/work/src"]),
        ("rg --files src", vec!["/work/src"]),
    ] {
        assert_reads(command);
        let paths = paths.into_iter().map(str::to_owned).collect();
        assert_eq!(classified(command).declared.paths, Some(paths), "{command}");
    }
}

#[test]
fn a_search_or_diff_of_fiber_home_declares_fiber_home_or_the_workdir() {
    // The paths the credential deny judges: each contains Fiber home's
    // `credentials/` (`docs/permissions.md`, "Credentials").
    for (command, paths) in [
        ("grep -r '' /home/me/.fiber", vec!["/home/me/.fiber"]),
        ("rg sk-", vec!["/work"]),
        (
            "git diff /home/me/.fiber /tmp",
            vec!["/home/me/.fiber", "/tmp"],
        ),
    ] {
        assert_reads(command);
        let paths = paths.into_iter().map(str::to_owned).collect();
        assert_eq!(classified(command).declared.paths, Some(paths), "{command}");
    }
}

#[test]
fn an_operand_under_proc_is_not_read_only() {
    // `/proc/<pid>/environ` holds every variable the process was given,
    // an `env` credential source's key among them.
    for command in [
        "cat /proc/self/environ",
        "head -c 100000 /proc/1/environ",
        "grep -r KEY /proc",
        "ls /proc",
        "cat ../../proc/self/environ",
        "cat /work/../proc/self/environ",
        "find /proc -name environ",
        "cat notes.txt /proc/self/environ",
        "ls && cat /proc/self/environ",
    ] {
        assert_closed(command);
    }
    for command in ["cat proc/self/environ", "cat /procfs/x", "ls /work/proc"] {
        assert_reads(command);
    }
}

fn assert_configured_closed(command: &str, entries: &[(String, Vec<String>)]) {
    let effects = classified_with(command, entries);
    assert_eq!(
        effects.declared.effects,
        vec![Effect::Executes],
        "{command}"
    );
    assert!(!effects.declared.reversible, "{command}");
    assert!(effects.declared.paths.is_none(), "{command}");
}

#[test]
fn a_configured_command_reads_with_only_its_flags() {
    let entries = [configured("jq", &[])];
    assert_configured_reads("jq . data.json", &entries, &["/work/.", "/work/data.json"]);
    assert_configured_reads("jq", &entries, &["/work"]);
    assert_configured_closed("jq -r . f", &entries);
    // Without the entry the same call is not read-only.
    assert_closed("jq . data.json");
    assert_closed("jq");
}

#[test]
fn a_configured_flag_list_admits_only_what_it_names() {
    let entries = [configured("jq", &["-r"])];
    assert_configured_reads("jq -r . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_closed("jq -c . f", &entries);
    assert_configured_closed("jq -r -c . f", &entries);
}

#[test]
fn a_configured_value_flag_takes_its_value_inline_or_next() {
    let entries = [configured("jq", &["--indent="])];
    // The value is never a declared path: only the operands are.
    assert_configured_reads("jq --indent=2 . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_reads("jq --indent 2 . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_reads("jq --indent= . f", &entries, &["/work/.", "/work/f"]);
    // A missing separate value is not read-only.
    assert_configured_closed("jq . f --indent", &entries);
    // A longer or shorter spelling is a different flag.
    assert_configured_closed("jq --indentx=2 . f", &entries);
    assert_configured_closed("jq --indent2 . f", &entries);
    assert_configured_closed("jq --indentx 2 . f", &entries);
    // A separate value may start with `-`, and may be the final word.
    assert_configured_reads("jq --indent -x . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_reads("jq . f --indent 2", &entries, &["/work/.", "/work/f"]);
}

#[test]
fn a_configured_short_value_flag_needs_its_equals_marker() {
    let entries = [configured("jq", &["-p="])];
    assert_configured_reads("jq -p=x . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_reads("jq -p x . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_reads("jq -p= . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_closed("jq -p", &entries);
    assert_configured_closed("jq -p . f", &[configured("jq", &[])]);
    // A long-only parser would read `-p` as a plain flag and keep the
    // value as an operand; the configured grammar consumes it instead.
    assert_configured_reads("jq -p 2", &entries, &["/work"]);
}

#[test]
fn a_configured_plain_flag_takes_no_value() {
    let entries = [configured("jq", &["--color"])];
    assert_configured_reads("jq --color . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_closed("jq --color=always . f", &entries);
    // The next word stays an operand: its path is declared.
    assert_configured_reads(
        "jq --color always . f",
        &entries,
        &["/work/always", "/work/.", "/work/f"],
    );
    // With the value-taking spelling the same word is its value instead.
    assert_configured_reads(
        "jq --color always . f",
        &[configured("jq", &["--color="])],
        &["/work/.", "/work/f"],
    );
}

#[test]
fn a_configured_short_cluster_is_literal() {
    assert_configured_closed("jq -rc . f", &[configured("jq", &["-r", "-c"])]);
    assert_configured_closed("jq -rc . f", &[configured("jq", &["-r"])]);
    assert_configured_reads(
        "jq -rc . f",
        &[configured("jq", &["-rc"])],
        &["/work/.", "/work/f"],
    );
}

#[test]
fn a_configured_double_dash_and_lone_dash_must_be_listed() {
    // Absent, both execute.
    assert_configured_closed("jq -- . f", &[configured("jq", &["-r"])]);
    assert_configured_closed("jq - . f", &[configured("jq", &["-r"])]);
    // Listed, both are flags that take no value.
    assert_configured_reads(
        "jq -- . f",
        &[configured("jq", &["--"])],
        &["/work/.", "/work/f"],
    );
    assert_configured_reads(
        "jq - . f",
        &[configured("jq", &["-"])],
        &["/work/.", "/work/f"],
    );
    // `--` never ends flags for a configured command: the word after it
    // is still a flag, and an unlisted one executes.
    assert_configured_closed("jq -- -x", &[configured("jq", &["--"])]);
    assert_configured_closed("jq -- -x . f", &[configured("jq", &["--"])]);
    assert_configured_reads(
        "jq -- -x . f",
        &[configured("jq", &["--", "-x"])],
        &["/work/.", "/work/f"],
    );
}

#[test]
fn an_empty_or_non_flag_spelling_admits_no_flag() {
    for entries in [
        vec![configured("jq", &[])],
        vec![configured("jq", &[""])],
        vec![configured("jq", &["json"])],
    ] {
        assert_configured_closed("jq --json . f", &entries);
        assert_configured_closed("jq -j . f", &entries);
    }
}

#[test]
fn a_configured_plain_spelling_wins_over_its_equals_twin() {
    // In either list order the exact word takes no value, so the next
    // word stays an operand and its path is declared.
    for entries in [
        vec![configured("jq", &["--opt", "--opt="])],
        vec![configured("jq", &["--opt=", "--opt"])],
    ] {
        assert_configured_reads("jq --opt /h/credentials/k", &entries, &["/h/credentials/k"]);
    }
    // With only the value-taking spelling the trailing word is its value,
    // and a missing value executes.
    assert_configured_closed("jq . f --opt", &[configured("jq", &["--opt="])]);
    assert_configured_reads(
        "jq . f --opt v",
        &[configured("jq", &["--opt="])],
        &["/work/.", "/work/f"],
    );
}

#[test]
fn a_configured_name_matches_exactly() {
    let entries = [configured("jq", &[])];
    assert_configured_closed("jqx . f", &entries);
    assert_configured_closed("/usr/bin/jq . f", &entries);
    assert_configured_closed("jq . f", &[]);
    // A multiword name matches one quoted word equal to the whole name,
    // or the word path: `my tool a b` reads as the `my tool` command.
    let spaced = [configured("my tool", &[])];
    assert_configured_reads("'my tool' a b", &spaced, &["/work/a", "/work/b"]);
    assert_configured_reads("my tool a b", &spaced, &["/work/a", "/work/b"]);
    assert_configured_closed("'my tool x' a", &spaced);
    // A first word holding `=` is never a command name.
    assert_configured_closed("jq=. f", &[configured("jq=", &[])]);
    assert_configured_closed("FOO=bar jq . f", &entries);
}

#[test]
fn a_configured_builtin_basename_is_never_consulted() {
    for (command, name, flags) in [
        ("grep -R x ws", "grep", &["-R"] as &[&str]),
        ("/usr/bin/grep -R x ws", "/usr/bin/grep", &["-R"]),
        ("/usr/bin/grep -r x ws", "/usr/bin/grep", &["-r"]),
        ("grep -R", "grep", &["-R"]),
        ("rg -L x", "rg", &["-L"]),
        ("./rg -L x", "./rg", &["-L"]),
        ("find -L x", "find", &["-L"]),
        ("/usr/bin/find -L x", "/usr/bin/find", &["-L"]),
        ("cat --foo", "cat", &["--foo"]),
        ("/bin/cat --foo", "/bin/cat", &["--foo"]),
        ("git push", "/usr/bin/git push", &[]),
        ("sed -i 1p f", "sed", &["-i"]),
        ("/usr/bin/sed -n 1p f", "/usr/bin/sed", &["-n"]),
    ] {
        let entries = [configured(name, flags)];
        assert_configured_closed(command, &entries);
    }
    // The built-in rule still answers those commands as it did.
    assert_closed("/usr/bin/grep -r x ws");
    assert_reads("grep -r x ws");
    assert_reads("git branch --show-current");
    assert_closed("git push");
    // `git` is consulted with the person's flags: `push` reads as an
    // operand where no built-in path owns the call.
    let push = [configured("git", &["push"])];
    assert_configured_reads("git push", &push, &["/work/push"]);
    // A path whose basename is not built in is consulted, for that exact
    // word only.
    let entries = [configured("/opt/tools/jq", &[])];
    assert_configured_reads("/opt/tools/jq . f", &entries, &["/work/.", "/work/f"]);
    assert_configured_closed("jq . f", &entries);
    assert_configured_closed("/opt/tools/jqx . f", &entries);
    // A name ending in `/` is not built in, and matches only itself.
    let trailing = [configured("tools/", &[])];
    assert_configured_reads("tools/ a", &trailing, &["/work/a"]);
    assert_configured_closed("tools a", &trailing);
}

#[test]
fn a_configured_empty_name_matches_only_an_empty_first_word() {
    let entries = [configured("", &[])];
    // A quoted empty first word takes the configured path.
    assert_configured_reads("'' a b", &entries, &["/work/a", "/work/b"]);
    // The same call with no empty entry executes.
    assert_closed("'' a b");
    // No non-empty word matches the empty entry.
    assert_configured_closed("jq a b", &entries);
    assert_configured_closed("x a", &entries);
    // A part with no word never matches.
    assert_configured_closed("ls && ''", &[]);
}

#[test]
fn a_configured_operand_under_proc_is_not_read_only() {
    let entries = [configured("jq", &[])];
    for command in [
        "jq . /proc/self/environ",
        "jq . /proc/../proc/self/environ",
        "jq . notes.txt /proc/self/environ",
    ] {
        assert_configured_closed(command, &entries);
    }
    // A workdir under `/proc` is not read-only either.
    assert_eq!(
        classified_in("jq . f", Path::new("/proc"), &entries)
            .declared
            .effects,
        vec![Effect::Executes]
    );
    assert_eq!(
        classified_in("jq", Path::new("/proc"), &entries)
            .declared
            .effects,
        vec![Effect::Executes]
    );
    // A workdir that fails to resolve executes.
    use std::os::unix::ffi::OsStrExt;
    let bad = Path::new(std::ffi::OsStr::from_bytes(b"/work/\xff"));
    assert_eq!(
        classified_in("jq . f", bad, &entries).declared.effects,
        vec![Effect::Executes]
    );
    assert_eq!(
        classified_in("jq", bad, &entries).declared.effects,
        vec![Effect::Executes]
    );
}

#[test]
fn a_configured_part_beside_a_closed_part_is_not_read_only() {
    let entries = [configured("jq", &["-r"])];
    assert_configured_reads(
        "jq -r . f && ls src",
        &entries,
        &["/work/.", "/work/f", "/work/src"],
    );
    for command in [
        "jq -r . f && jq -c . g",
        "jq -r . f || unknowns . g",
        "jq -r . f; unknowns . g",
        "jq -r . f | unknowns",
        "ls src && jq -c . g",
        "jq -r . f && grep -R x ws",
    ] {
        assert_configured_closed(command, &entries);
    }
}

#[test]
fn a_configured_call_keeps_redirects_and_refusals() {
    let entries = [configured("jq", &[])];
    // A stderr redirect is dropped before the check and kept in the subject.
    assert_configured_reads("jq . f 2>/dev/null", &entries, &["/work/.", "/work/f"]);
    // A write redirect, a substitution and an expansion never read.
    for command in [
        "jq . f > out",
        "jq $(echo .) f",
        "jq `echo .` f",
        "jq $HOME f",
        "jq . *.json",
    ] {
        assert_configured_closed(command, &entries);
    }
}

#[test]
fn a_quoted_path_with_a_space_still_ends_at_its_basename() {
    // A built-in basename under a spaced path is never consulted: the
    // link-following flag stays refused.
    let entries = [configured("/opt/my tools/grep", &["-R"])];
    assert_configured_closed("'/opt/my tools/grep' -R pattern ws", &entries);
    assert_configured_closed("'/opt/my tools/grep' -r pattern ws", &entries);
    // A non-built-in basename under a spaced path is consulted, for that
    // exact word only.
    let spaced = [configured("/opt/my tools/jq", &[])];
    assert_configured_reads("'/opt/my tools/jq' . f", &spaced, &["/work/.", "/work/f"]);
    assert_configured_closed("/opt/my tools/jq . f", &spaced);
    assert_configured_closed("'/opt/my tools/jqx' . f", &spaced);
    assert_configured_closed("'/opt/my tools/jq' -x . f", &spaced);
}

#[test]
fn cd_forms_outside_the_allow_list_execute() {
    for command in [
        "cd",
        "cd -",
        "cd \"\"",
        "cd -P",
        "cd -L",
        "cd -x",
        "cd -P x",
        "cd -L x",
        "cd a b",
        "cd ~",
        "cd \"~\"",
        "cd \"~/x\"",
        "cd a*b",
        "cd \"a*b\"",
        "cd \"a?b\"",
        "cd \"a[b]\"",
        "cd \"a{b}\"",
        "cd $(x)",
        "cd `x`",
        "cd $HOME",
        "cd --",
        "cd -- -",
    ] {
        assert_closed(command);
    }
}

#[test]
fn cd_dash_dash_names_one_directory() {
    assert_declares("cd -- x", &["/work/x"]);
    // A flag-like operand reads only after `--`.
    assert_declares("cd -- -P", &["/work/-P"]);
}

#[test]
fn cd_declares_its_directory() {
    assert_declares("cd sub", &["/work/sub"]);
    // A lexical join still resolves against the workdir.
    assert_declares("cd sub/../proc", &["/work/proc"]);
    // Under /proc the call is reviewed.
    for command in ["cd /proc", "cd /proc/self", "cd /proc/1/fd"] {
        assert_closed(command);
    }
}

#[test]
fn operand_less_commands_declare_each_running_directory() {
    // Each element declares the directories it runs in: the `cd` its own
    // join, then the operand-less `ls` each running directory.
    assert_declares("cd sub && ls", &["/work/sub"]);
    for command in ["cd sub ; ls", "cd sub || ls"] {
        assert_declares(command, &["/work", "/work/sub"]);
    }
}

#[test]
fn git_after_any_cd_executes() {
    for command in [
        "cd sub && git status",
        "cd sub ; git status",
        "cd sub | git status",
        "cd sub || git status",
        "cd sub && git stash list",
        "cd sub && git log",
    ] {
        assert_closed(command);
    }
    // Git before the cd still reads, declaring the joined directory.
    assert_declares("git status && cd sub", &["/work/sub"]);
}

#[test]
fn cd_before_an_input_redirect_checks_each_running_directory() {
    assert_declares("cd sub && cat <f", &["/work/sub", "/work/sub/f"]);
    for command in ["cd sub ; cat <f", "cd sub || cat <f"] {
        assert_declares(command, &["/work", "/work/sub", "/work/f", "/work/sub/f"]);
    }
}

#[test]
fn cd_in_a_pipeline_moves_nothing() {
    assert_declares("cd sub | cat f", &["/work/sub", "/work/f"]);
}

#[test]
fn losing_the_path_executes_every_later_element() {
    assert_reads(&cd_chain(30));
    assert_reads(&cd_chain(31));
    assert_closed(&cd_chain(32));
}

fn cd_chain(count: usize) -> String {
    let mut command = String::new();
    for n in 1..=count {
        if n > 1 {
            command.push_str(" ; ");
        }
        command.push_str(&format!("cd /d{n}"));
    }
    command.push_str(" ; ls");
    command
}

fn noted(command: &str) -> Option<String> {
    super::not_shown_read_only(command, Path::new("/work"), &[])
}

#[test]
fn the_note_names_only_the_stages_that_were_not_shown_read_only() {
    assert_eq!(
        noted("ls && npm test"),
        Some("Not shown read-only: `npm test`".to_owned())
    );
    assert_eq!(noted("npm test"), None);
    assert_eq!(noted("ls && pwd"), None);
    assert_eq!(noted("ls &&"), None);
    assert_eq!(
        noted("echo $(x) && ls"),
        Some("Not shown read-only: `echo $(x)`".to_owned())
    );
}

#[test]
fn the_note_joins_several_unshown_parts_with_comma() {
    assert_eq!(
        noted("ls && npm test && npm run"),
        Some("Not shown read-only: `npm test`, `npm run`".to_owned())
    );
}

#[test]
fn the_note_names_a_git_stage_failed_by_an_earlier_cd() {
    assert_eq!(
        noted("cd sub && git status"),
        Some("Not shown read-only: `git status`".to_owned())
    );
}

#[test]
fn the_note_names_an_opaque_subshell_stage() {
    assert_eq!(
        noted("(ls) && pwd"),
        Some("Not shown read-only: `(ls)`".to_owned())
    );
}

#[test]
fn the_note_keeps_verbatim_spacing() {
    assert_eq!(
        noted("ls  &&  npm   test"),
        Some("Not shown read-only: `npm   test`".to_owned())
    );
}

#[test]
fn the_note_names_a_pipeline_stage() {
    assert_eq!(
        noted("ls | npm test"),
        Some("Not shown read-only: `npm test`".to_owned())
    );
}

#[test]
fn the_note_names_a_stage_reading_under_proc() {
    assert_eq!(
        noted("cat /proc/version && ls"),
        Some("Not shown read-only: `cat /proc/version`".to_owned())
    );
}

#[test]
fn the_note_names_the_command_after_a_lost_path() {
    assert_eq!(
        noted(&cd_chain(32)),
        Some("Not shown read-only: `ls`".to_owned())
    );
}

#[test]
fn an_oversize_call_has_no_note() {
    let over = format!("echo {}", "a".repeat(9_996));
    assert!(over.len() > super::super::syntax::MAX_COMMAND_BYTES);
    assert_eq!(noted(&over), None);
}
