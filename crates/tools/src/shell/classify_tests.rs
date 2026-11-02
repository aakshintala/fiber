use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use contract::shapes::Effect;
use contract::tool::Effects;

use super::super::read_only::{COMMANDS, Flag};
use super::classify;

#[test]
fn push_stays_reviewed_with_wrappers_options_and_quoted_text() {
    for command in [
        "git push",
        "git push origin main",
        "sudo /usr/bin/git push",
        "echo git push",
        "echo ok; git push",
        "git push | cat",
        "'git' 'push'",
        "echo \"git push\"",
        "git -C /repo push origin",
        "git -c push.default=current push",
        "git --no-pager push",
        "git --git-dir=/repo/.git push",
        "git --git-dir /repo/.git push origin",
        "git --work-tree /repo push",
        "git -q push origin",
        "git -C /repo -c a=b --no-pager push",
        "git push 'unterminated",
        "git -- push",
    ] {
        assert!(super::review_when_confined(command), "{command}");
    }
    for command in [
        "git fetch",
        "git status",
        "git pull",
        "git pushing",
        "git pus",
        "git -C push status",
        "git -c push status",
        "git --git-dir=push status",
        "git --git-dir /repo/.git status",
        "git -C",
        "git -c",
        "git --no-pager",
        "git -C /repo status",
        "push",
        "git",
    ] {
        assert!(!super::review_when_confined(command), "{command}");
    }
}

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
    classify(command, Path::new("/work"), Path::new("/work"), &[])
}

fn configured(name: &str, flags: &[&str]) -> (String, Vec<String>) {
    (
        name.to_owned(),
        flags.iter().map(|flag| (*flag).to_owned()).collect(),
    )
}

fn classified_with(command: &str, entries: &[(String, Vec<String>)]) -> Effects {
    classify(command, Path::new("/work"), Path::new("/work"), entries)
}

fn classified_in(command: &str, workdir: &Path, entries: &[(String, Vec<String>)]) -> Effects {
    classify(command, Path::new("/work"), workdir, entries)
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
fn git_merge_base_reads_without_paths() {
    // `docs/tools.md`, "Shell", "Effects": `git merge-base` is read-only
    // with no flags, and its operands are revisions declaring no paths.
    for command in ["git merge-base a HEAD", "git merge-base abc123 HEAD"] {
        assert_reads(command);
        assert!(classified(command).declared.paths.is_none(), "{command}");
    }
    assert_closed("git merge-base a HEAD --check");
}

#[test]
fn git_diff_read_only_flags_read() {
    for command in [
        "git diff --check",
        "git diff --merge-base",
        "git diff -w",
        "git diff --ignore-all-space",
        "git diff --diff-filter=ACM",
        "git diff --diff-filter=",
    ] {
        assert_reads(command);
    }
}

#[test]
fn git_unified_and_diff_filter_forms() {
    // `-U<n>` takes attached digits only, and `--unified=<n>` its inline
    // all-digit value, for `git diff`, `git log` and `git show`;
    // `--diff-filter=<value>` takes its value inline only. A flag after
    // the first operand still reads.
    for command in [
        "git diff -U80 a..HEAD --check",
        "git log -U3",
        "git show --unified=5",
        "git diff --unified=007",
        "git log -U0",
    ] {
        assert_reads(command);
    }
    for command in [
        "git diff -U",
        "git diff -U8x",
        "git diff -U 80",
        "git diff --unified",
        "git diff --unified=",
        "git diff --unified=x",
        "git diff --unified 5",
        "git diff --diff-filter",
        "git diff --diff-filter ACM",
        "git rev-parse -U5",
    ] {
        assert_closed(command);
    }
}

#[test]
fn git_merge_base_substitution_reads() {
    // `docs/tools.md`, "Shell", "Effects": a `$(git merge-base <rev>
    // <rev>)` substitution reads as an operand of a listed git read, bare
    // or composed with one `..`/`...` range against an outer plain ref.
    // A validated word declares no path, so plain operands declare as
    // before.
    assert_declares(
        "git diff $(git merge-base abc123 HEAD)..HEAD -- src",
        &["/work/src"],
    );
    for command in [
        "git diff $(git merge-base a b)",
        "git diff $(git merge-base a b)...HEAD",
        "git diff HEAD..$(git merge-base a b)",
        "git diff HEAD...$(git merge-base a b)",
        "git diff $(git merge-base HEAD~1 origin/main)..HEAD",
        "git diff $(git merge-base a b) $(git merge-base c d)",
        "git diff $(git merge-base HEAD~0 a)..HEAD",
        "git log -p $(git merge-base a b)",
        // A leading discard redirect stays read-only: the redirect drops
        // out before the flag walk, and the substitution still lines up
        // with its own token.
        ">/dev/null git diff $(git merge-base a b)..HEAD",
    ] {
        assert_reads(command);
    }
    // Two validated substitutions declare no paths of their own: the
    // verdict matches a bare `git diff`, with only the workdir default.
    assert_declares(
        "git diff $(git merge-base a b) $(git merge-base c d)",
        &["/work"],
    );
}

#[test]
fn git_merge_base_substitution_refusals_execute() {
    for command in [
        "cat $(git merge-base a b)",
        "git diff $(git merge-base --fork-point a b)",
        "git diff $(git rev-parse $(echo x))",
        "git diff $(git log -1)",
        "git log -p $(git rev-parse .env)",
        "git diff $(git merge-base a b)--output=/tmp/f",
        "git log -p -- $(git merge-base a b).env",
        "git diff `git merge-base a b`..HEAD",
        "git diff $(/tmp/x/git merge-base a b)..HEAD",
        "git diff $(./git merge-base a b)..HEAD",
        "git diff $(git merge-base a b;id)..HEAD",
        "git diff $(git merge-base a b>f)..HEAD",
        "git diff $(git merge-base\nsh x.sh)",
        "git diff HEAD$(git merge-base a b)..HEAD",
        "git diff .$(git merge-base a b).HEAD",
        "git log -n $(git merge-base a b) .env",
        "git blame -L $(git merge-base a b) .env",
        "git diff $(git merge-base -- a b)",
        "git diff -- $(git merge-base a b)",
        "git diff $(git merge-base a b c)",
        "git diff $(git merge-base HEAD~ a)..HEAD",
        "git diff $(git merge-base HEAD~x a)..HEAD",
        "git diff $(git merge-base a:b c)..HEAD",
        "git diff \"x\"$(git merge-base a b)..HEAD",
        "git diff 'x'$(git merge-base a b)..HEAD",
        "git diff $(git merge-base a b)..HEAD~1",
        "git diff $(git merge-base a b)..HEAD:foo",
        "git diff $(git merge-base a b)....HEAD",
        "git diff $(git merge-base a b)..",
        "git diff ..$(git merge-base a b)",
        "git diff $(git merge-base HEAD^^ a)..HEAD",
        "git diff $(git merge-base a b)..HEAD --output=f",
        "$(git merge-base a b)",
        "git diff --diff-filter=$(git merge-base a b) -- src",
        "git show a < $(git merge-base a b)",
        "git show a > /dev/null $(git merge-base a b)",
        "TZ=$(git merge-base a b) git diff x",
        "env git diff $(git merge-base a b)..HEAD",
        "timeout 5 git diff $(git merge-base a b)..HEAD",
    ] {
        assert_closed(command);
    }
    let entries = [configured("git stash show", &[])];
    assert_configured_reads("git stash show x", &entries, &["/work/x"]);
    assert_configured_closed("git stash show $(git merge-base a b)", &entries);
}

#[test]
fn a_substituted_executes_names_no_subject() {
    // A substituted call that executes names no subject: no allow rule can
    // match a reviewed call whose words arrive only when it runs.
    assert_closed("cat $(echo x)");
    assert!(classified("cat $(echo x)").subject.is_none());
}

#[test]
fn plain_concatenated_operands_stay_unmarked() {
    // `syntax.rs` cooks a concatenation with no substitution unmarked, so
    // an ordinary concatenated operand still reads on a non-git command.
    assert_reads("echo a\"b\"c");
}

#[test]
fn refused_substitution_concatenations_stay_opaque() {
    for command in [
        "git diff $(git merge-base a b)\"..HEAD\"",
        "git diff $(git merge-base a b)'..HEAD'",
        "git diff HEAD$(git merge-base a b)..HEAD",
    ] {
        let script = super::super::syntax::parse(command).expect("valid shell syntax");
        assert!(
            matches!(
                script.elements[0].stages.as_slice(),
                [super::super::syntax::Stage::Opaque(_)]
            ),
            "{command}"
        );
    }
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
        "git diff --fork-point",
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
fn gh_jq_programs_read_unless_they_name_the_environment_or_input() {
    // `docs/tools.md`, "Shell", "Effects": `--jq` joins each listed `gh`
    // read, and a program holding `env`, `ENV` or `input` as a substring,
    // matched case-sensitively, makes the call `executes`.
    for (command, paths) in [
        ("gh pr view 3 --json body --jq .body", vec!["/work/3"]),
        ("gh pr view 3 --json body --jq=.body", vec!["/work/3"]),
        ("gh pr diff --patch --jq .", vec!["/work"]),
        ("gh pr view 3 --json body --jq Env", vec!["/work/3"]),
        ("gh pr view 3 --json body --jq INPUT", vec!["/work/3"]),
        // A repo-like program is `--jq`'s value, never a named owner.
        ("gh pr view --jq owner/repo", vec!["/work"]),
    ] {
        assert_network(command, &[], &paths);
    }
    for command in [
        "gh pr view 3 --json body --jq 'env.GH_TOKEN'",
        "gh pr view 3 --json body --jq '$ENV'",
        "gh pr view 3 --json body --jq '\"env\"'",
        "gh pr view 3 --json body --jq inputs",
        "gh pr view 3 --json body --jq=env.x",
        "gh pr view 3 --json body --jq $(git merge-base a b)",
    ] {
        assert_closed(command);
    }
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
    // A configured `gh pr view` merges its flags into the built-in entry:
    // `--web` reads as a network call. A configured `gh pr status` is
    // honoured: no built-in owns it.
    let view = [configured("gh pr view", &["--web"])];
    assert_network("gh pr view 123 --web", &view, &["/work/123"]);
    assert_configured_closed("gh pr view 123 --bogus", &view);
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
            && command.name != "git merge-base"
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

/// One read-only spelling of a flag: an inline-only value rides `=`, an
/// all-digit value rides `=5`, any other value rides the next word, and a
/// plain flag stands alone.
fn inline_value_line(name: &str, flag: &Flag) -> String {
    if flag.digits_only() {
        format!("{name} {}=5", flag.spelling)
    } else if flag.inline_only() {
        format!("{name} {}=v", flag.spelling)
    } else if flag.takes_value() {
        format!("{name} {} v", flag.spelling)
    } else {
        format!("{name} {}", flag.spelling)
    }
}

#[test]
fn each_allowed_flag_keeps_the_command_read_only() {
    for command in COMMANDS {
        // A `gh` command hits the network, so it declares `Network`
        // with the default host instead of `Reads`.
        if command.name.starts_with("gh ") {
            for flag in command.flags {
                let line = inline_value_line(command.name, flag);
                assert_network(&line, &[], &["/work"]);
            }
            continue;
        }
        for flag in command.flags {
            let line = inline_value_line(command.name, flag);
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
        ("git merge-base", true),
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
    for command in COMMANDS {
        let (_, ends_flags) = stated
            .iter()
            .find(|(name, _)| *name == command.name)
            .unwrap_or_else(|| panic!("missing -- policy for {}", command.name));
        assert_eq!(command.ends_flags, *ends_flags, "{}", command.name);
    }
    for (name, _) in stated {
        assert!(
            COMMANDS.iter().any(|command| command.name == name),
            "stale -- policy for {name}"
        );
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
fn a_configured_flag_on_a_built_in_command_reads() {
    // The merge: the built-in's flags are kept and the configured ones are
    // added. A flag on neither list still executes.
    let entries = [configured("cat", &["--foo"])];
    assert_configured_reads("cat --foo f", &entries, &["/work/f"]);
    assert_configured_reads("cat -n f", &entries, &["/work/f"]);
    assert_configured_closed("cat --bar f", &entries);
    // Without the entry the configured-only flag executes.
    assert_closed("cat --foo f");
    // An empty configured flag list adds nothing.
    assert_configured_closed("cat --foo f", &[configured("cat", &[])]);
}

#[test]
fn a_configured_subcommand_flag_on_git_status_reads() {
    // A configured subcommand path under a built-in with subcommand entries
    // merges too: `git status` has a flag list.
    let entries = [configured("git status", &["--long"])];
    for command in ["git status --long", "git status -s"] {
        let effects = classified_with(command, &entries);
        assert_eq!(effects.declared.effects, vec![Effect::Reads], "{command}");
        assert!(effects.declared.reversible, "{command}");
        assert!(effects.declared.paths.is_none(), "{command}");
    }
    // The bare exact form keeps precedence with no paths.
    let bare = classified_with("git status", &entries);
    assert_eq!(bare.declared.effects, vec![Effect::Reads]);
    assert!(bare.declared.paths.is_none());
    // A flag on neither list still executes.
    assert_configured_closed("git status --bogus", &entries);
    // Without the entry the configured-only flag executes.
    assert_closed("git status --long");
}

#[test]
fn a_configured_value_flag_on_a_built_in_never_takes_a_substitution() {
    let entries = [configured("cat", &["--foo="])];
    assert_configured_reads("cat --foo=bar f", &entries, &["/work/f"]);
    assert_configured_reads("cat --foo bar f", &entries, &["/work/f"]);
    assert_configured_closed("cat --foo", &entries);
    assert_configured_closed("cat --foo $(echo bar) f", &entries);
}

#[test]
fn a_path_qualified_grep_entry_adds_nothing_to_grep() {
    // Exact key match only: `/usr/bin/grep` never merges into `grep`, so
    // neither spelling reads with the configured-only flag.
    let entries = [configured("/usr/bin/grep", &["-P"])];
    assert_configured_closed("/usr/bin/grep -P x", &entries);
    assert_configured_closed("grep -P x", &entries);
    // A built-in flag still reads.
    assert_configured_reads("grep -n pat file", &entries, &["/work/file"]);
}

#[test]
fn a_flagless_built_in_takes_no_configured_flags() {
    // Table entries with no flag list take nothing: the exact-form
    // `git branch --show-current` and `pwd` merge no configured flags.
    let branch = [configured("git branch --show-current", &["--foo"])];
    assert_configured_closed("git branch --show-current --foo", &branch);
    assert_reads("git branch --show-current x");
    let pwd = [configured("pwd", &["--foo"])];
    assert_configured_closed("pwd --foo", &pwd);
    assert_reads("pwd");
}

#[test]
fn a_configured_link_following_flag_stays_reviewed() {
    // A recursive read that follows links is never read-only, even when the
    // person lists the flag: the credential deny sees only the paths a call
    // names, never a link below them.
    for (command, name, flags) in [
        ("grep -R x ws", "grep", &["-R"] as &[&str]),
        ("grep -R", "grep", &["-R"]),
        (
            "grep --dereference-recursive x ws",
            "grep",
            &["--dereference-recursive"],
        ),
        (
            "grep --dereference-recursive=x ws",
            "grep",
            &["--dereference-recursive="],
        ),
        ("grep -Rn x ws", "grep", &["-Rn"]),
        ("grep -nR x ws", "grep", &["-nR"]),
        ("grep -Rr x ws", "grep", &["-Rr"]),
        ("rg -L x", "rg", &["-L"]),
        ("rg --follow x", "rg", &["--follow"]),
        ("rg -Ln x ws", "rg", &["-Ln"]),
        ("find -L x", "find", &["-L"]),
        ("find -H x", "find", &["-H"]),
        ("find -follow x", "find", &["-follow"]),
        ("find -LH x", "find", &["-LH"]),
        ("find -Lx x", "find", &["-Lx"]),
        ("find -Hx x", "find", &["-Hx"]),
        ("ls -L x", "ls", &["-L"]),
        ("ls -lL x", "ls", &["-lL"]),
    ] {
        let entries = [configured(name, flags)];
        assert_configured_closed(command, &entries);
    }
    // A bundled cluster without the link-following letter still reads under
    // its listed spelling.
    assert_configured_reads(
        "grep -Pn x ws",
        &[configured("grep", &["-Pn"])],
        &["/work/ws"],
    );
    // A long option is never letter-matched: only the exact spellings above
    // follow links, so a configured long spelling holding `R` still reads.
    // The `||` guard pins this: `&&` would letter-match `-fooR` and execute.
    assert_configured_reads(
        "grep --fooR x ws",
        &[configured("grep", &["--fooR"])],
        &["/work/ws"],
    );
    // Near misses still read: the guard fires on the exact spelling or a
    // cluster holding its letter, in flag position only.
    for command in [
        "grep -r x ws",
        "grep -L x ws",
        "rg -l x",
        "find -name x",
        "ls -l x",
        "grep -- -R ws",
        "ls -- -L x",
    ] {
        assert_reads(command);
    }
}

#[test]
fn a_path_qualified_or_owned_configured_name_is_never_consulted() {
    // A plain built-in basename merges instead (the test above); what stays
    // unconsulted is a path-qualified name, `sed` with its own grammar, a
    // wrapper step, or a name the built-ins own some other way.
    for (command, name, flags) in [
        ("/usr/bin/grep -R x ws", "/usr/bin/grep", &["-R"] as &[&str]),
        ("/usr/bin/grep -r x ws", "/usr/bin/grep", &["-r"]),
        ("./rg -L x", "./rg", &["-L"]),
        ("/usr/bin/find -L x", "/usr/bin/find", &["-L"]),
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
    super::not_shown_read_only(command, Path::new("/work"), Path::new("/work"), &[])
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

// The `gh` own-owner fast path (`docs/tools.md`, "Shell", "Effects"): a
// `gh` read whose every named repository belongs to an owner of the
// session workspace's `github.com` remotes declares `reads` with the paths
// it declares today, instead of the reviewed `network` declaration.

/// A session workspace whose `.git/config` holds `config`: the trust
/// anchor the own-owner check reads, never the call's `workdir`.
fn owner_workspace(name: &str, config: &str) -> (fakes::TempDir, PathBuf) {
    let dir = fakes::TempDir::new(name);
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    std::fs::write(workspace.join(".git").join("config"), config).unwrap();
    (dir, workspace)
}

/// A workspace config with one `github.com` remote.
fn remote_config(url: &str) -> String {
    format!("[remote \"origin\"]\n\turl = {url}\n")
}

/// A workspace whose `origin` is `git@github.com:acme/app.git`.
fn acme_workspace(name: &str) -> (fakes::TempDir, PathBuf) {
    owner_workspace(name, &remote_config("git@github.com:acme/app.git"))
}

/// The `name` operand resolved against `workspace`, as the classifier
/// declares operand paths today.
fn workspace_path(workspace: &Path, name: &str) -> String {
    workspace.join(name).to_str().unwrap().to_owned()
}

fn assert_own(command: &str, workspace: &Path, workdir: &Path, paths: Vec<String>) {
    let effects = classify(command, workspace, workdir, &[]);
    assert_eq!(effects.declared.effects, vec![Effect::Reads], "{command}");
    assert!(effects.declared.reversible, "{command}");
    assert!(effects.hosts.is_none(), "{command}");
    assert_eq!(effects.declared.paths, Some(paths), "{command}");
}

fn assert_foreign(
    command: &str,
    workspace: &Path,
    workdir: &Path,
    entries: &[(String, Vec<String>)],
    paths: Vec<String>,
) {
    let effects = classify(command, workspace, workdir, entries);
    assert_eq!(effects.declared.effects, vec![Effect::Network], "{command}");
    assert_eq!(
        effects.hosts,
        Some(vec!["https://api.github.com/".to_owned()]),
        "{command}"
    );
    assert_eq!(effects.declared.paths, Some(paths), "{command}");
}

fn assert_foreign_effect(command: &str, workspace: &Path, workdir: &Path) {
    let effects = classify(command, workspace, workdir, &[]);
    assert_eq!(effects.declared.effects, vec![Effect::Network], "{command}");
    assert_eq!(
        effects.hosts,
        Some(vec!["https://api.github.com/".to_owned()]),
        "{command}"
    );
}

#[test]
fn own_reads_take_the_fast_path() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-own");
    // No repository named: `gh` resolves it from the workspace's remotes.
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
    // A named repository owned by a workspace remote owner.
    assert_own(
        "gh pr view 3 --repo acme/other",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "3")],
    );
    // A GitHub URL operand names its repository by owner alone.
    assert_own(
        "gh pr view https://github.com/acme/app/pull/3",
        &workspace,
        &workspace,
        vec![workspace_path(
            &workspace,
            "https://github.com/acme/app/pull/3",
        )],
    );
}

#[test]
fn foreign_repos_keep_the_reviewed_network_declaration() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-foreign");
    assert_foreign(
        "gh issue view 12 --repo evil/app",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
    assert_foreign(
        "gh pr view https://github.com/evil/app",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "https://github.com/evil/app")],
    );
}

#[test]
fn every_supported_remote_form_yields_its_owner() {
    let mut configs: Vec<String> = [
        "git@github.com:acme/app.git",
        "git@github.com:acme/app",
        "https://github.com/acme/app",
        "https://github.com/acme/app.git",
        "https://github.com/acme/app/pull/3",
        "ssh://git@github.com/acme/app",
        "ssh://git@github.com/acme/app.git",
        "HTTPS://GITHUB.COM/ACME/APP",
        "git@github.com:acme/app # the team's repo",
    ]
    .iter()
    .map(|url| remote_config(url))
    .collect();
    // A second remote under another name counts the same.
    configs.push("[remote \"origin\"]\n\turl = https://example.com/acme/app.git\n[remote \"upstream\"]\n\turl = https://github.com/acme/other.git\n".to_owned());
    for (index, config) in configs.iter().enumerate() {
        let (_dir, workspace) = owner_workspace(&format!("fiber-shell-gh-form-{index}"), config);
        assert_own(
            "gh issue view 12",
            &workspace,
            &workspace,
            vec![workspace_path(&workspace, "12")],
        );
    }
}

#[test]
fn a_hash_inside_a_quoted_remote_subsection_is_not_a_comment() {
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-quoted-hash-subsection",
        "[remote \"origin # upstream\"]\n\turl = https://github.com/acme/app.git\n",
    );
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn other_hosts_and_spellings_yield_no_owners() {
    for (index, url) in [
        "https://example.com/acme/app",
        "http://github.com/acme/app",
        "https://evilgithub.com/acme/app",
        "https://github.com.evil.com/acme/app",
        "https://github.com:443/acme/app",
        "https://user@github.com/acme/app",
        "https://github.com/acme",
        "https://github.com/",
        "https://github.com",
        "git@github.com:acme",
        "git@other.com:acme/app",
        "GIT@github.com:acme/app",
        "ssh://github.com/acme/app",
        "ssh://git@github.com:22/acme/app",
        "git://github.com/acme/app",
    ]
    .iter()
    .enumerate()
    {
        let (_dir, workspace) = owner_workspace(
            &format!("fiber-shell-gh-noform-{index}"),
            &remote_config(url),
        );
        assert_foreign(
            "gh issue view 12",
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, "12")],
        );
    }
}

#[test]
fn a_workspace_without_a_github_remote_has_no_owners() {
    // No repository at all.
    let bare_dir = fakes::TempDir::new("fiber-shell-gh-norepo");
    let bare = bare_dir.path().join("ws");
    std::fs::create_dir_all(&bare).unwrap();
    assert_foreign(
        "gh issue view 12",
        &bare,
        &bare,
        &[],
        vec![workspace_path(&bare, "12")],
    );
    // A config with no remotes.
    let (_dir, core) = owner_workspace(
        "fiber-shell-gh-core",
        "[core]\n\trepositoryformatversion = 0\n",
    );
    assert_foreign(
        "gh issue view 12",
        &core,
        &core,
        &[],
        vec![workspace_path(&core, "12")],
    );
    // A non-GitHub remote.
    let (_dir, other) = owner_workspace(
        "fiber-shell-gh-other",
        &remote_config("https://example.com/acme/app.git"),
    );
    assert_foreign(
        "gh issue view 12",
        &other,
        &other,
        &[],
        vec![workspace_path(&other, "12")],
    );
    // A `pushurl` without a `url` contributes nothing.
    let (_dir, push) = owner_workspace(
        "fiber-shell-gh-pushurl",
        "[remote \"origin\"]\n\tpushurl = git@github.com:acme/app.git\n",
    );
    assert_foreign(
        "gh issue view 12",
        &push,
        &push,
        &[],
        vec![workspace_path(&push, "12")],
    );
    // An `include.path` target is never followed, even when it names an owner.
    let include_dir = fakes::TempDir::new("fiber-shell-gh-include");
    let included = include_dir.path().join("extra.config");
    std::fs::write(&included, remote_config("git@github.com:acme/app.git")).unwrap();
    let workspace = include_dir.path().join("ws");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    std::fs::write(
        workspace.join(".git").join("config"),
        format!("[include]\n\tpath = {}\n", included.to_str().unwrap()),
    )
    .unwrap();
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn a_malformed_config_has_no_owners() {
    for (index, config) in [
        "not a config {{{",
        "[unclosed",
        "url = git@github.com:acme/app.git",
        "[remote \"origin\"]\nurl = \"unterminated",
        "[remote \"origin\"]\nurl = \"git@github.com:acme/app\" junk",
        "[remote \"origin\"]\nurl = git@github.com:acme/app.git\0",
        "[remote \"origin\"]\nurl = git@github.com:acme/app.git\n[include",
    ]
    .iter()
    .enumerate()
    {
        let (_dir, workspace) =
            owner_workspace(&format!("fiber-shell-gh-malformed-{index}"), config);
        assert_foreign(
            "gh issue view 12",
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, "12")],
        );
    }
    // A non-UTF8 config cannot be parsed.
    let (_dir, utf8) = owner_workspace("fiber-shell-gh-utf8", "[core]\n");
    std::fs::write(
        utf8.join(".git").join("config"),
        b"[remote \"origin\"]\n\turl = \xff\n",
    )
    .unwrap();
    assert_foreign(
        "gh issue view 12",
        &utf8,
        &utf8,
        &[],
        vec![workspace_path(&utf8, "12")],
    );
    // A directory where the config belongs cannot be read.
    let (_dir, is_dir) = owner_workspace("fiber-shell-gh-configdir", "[core]\n");
    std::fs::remove_file(is_dir.join(".git").join("config")).unwrap();
    std::fs::create_dir_all(is_dir.join(".git").join("config")).unwrap();
    assert_foreign(
        "gh issue view 12",
        &is_dir,
        &is_dir,
        &[],
        vec![workspace_path(&is_dir, "12")],
    );
}

#[test]
fn a_malformed_value_outside_a_remote_url_has_no_owners() {
    // Every config value is validated, not just `remote.*.url`: a valid
    // remote beside an unterminated quoted value elsewhere still fails
    // closed, per `docs/tools.md` ("unparsable config").
    for (index, config) in [
        "[core]\n\tfoo = \"unterminated\n[remote \"origin\"]\n\turl = git@github.com:acme/app.git\n",
        "[remote \"origin\"]\n\turl = git@github.com:acme/app.git\n[core]\n\tbar = \"bad\\q\"\n",
    ]
    .iter()
    .enumerate()
    {
        let (_dir, workspace) = owner_workspace(
            &format!("fiber-shell-gh-outside-value-{index}"),
            config,
        );
        assert_foreign(
            "gh issue view 12",
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, "12")],
        );
    }
}

#[test]
fn a_symlinked_config_has_no_owners() {
    // `read_bounded` opens without following a trailing symlink, so a
    // config swapped for a link to an owned config still leaves no owners.
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-config-link");
    let config = workspace.join(".git").join("config");
    let real = workspace.join(".git").join("config.real");
    std::fs::rename(&config, &real).unwrap();
    std::os::unix::fs::symlink(&real, &config).unwrap();
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn a_fifo_config_has_no_owners_without_blocking() {
    // A FIFO where the config belongs cannot block classification: the
    // open requires a regular file before reading a byte.
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-config-fifo");
    let config = workspace.join(".git").join("config");
    std::fs::remove_file(&config).unwrap();
    fakes::fifo(&config);
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn the_config_size_cap_holds_at_64_kib() {
    for (size, reads) in [(65_535, true), (65_536, true), (65_537, false)] {
        let (_dir, workspace) = acme_workspace(&format!("fiber-shell-gh-size-{size}"));
        let base = std::fs::read(workspace.join(".git").join("config")).unwrap();
        let pad = size - base.len() - 2;
        let config = format!("{}#{}\n", String::from_utf8(base).unwrap(), "x".repeat(pad));
        assert_eq!(config.len(), size, "{size}");
        std::fs::write(workspace.join(".git").join("config"), config).unwrap();
        if reads {
            assert_own(
                "gh issue view 12",
                &workspace,
                &workspace,
                vec![workspace_path(&workspace, "12")],
            );
        } else {
            assert_foreign(
                "gh issue view 12",
                &workspace,
                &workspace,
                &[],
                vec![workspace_path(&workspace, "12")],
            );
        }
    }
}

/// A worktree checkout: `checkout/.git` names `gitdir: .../wt`, whose
/// `commondir` names the common directory holding `common_config`, while
/// the gitdir's own config holds `gitdir_config`.
fn worktree_workspace(
    name: &str,
    common_config: &str,
    gitdir_config: &str,
) -> (fakes::TempDir, PathBuf) {
    let dir = fakes::TempDir::new(name);
    let root = dir.path().join("ws");
    let common_dir = root.join("repo").join(".git");
    let gitdir = common_dir.join("worktrees").join("wt");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::write(common_dir.join("config"), common_config).unwrap();
    std::fs::write(gitdir.join("config"), gitdir_config).unwrap();
    std::fs::write(
        gitdir.join("commondir"),
        format!("{}\n", common_dir.to_str().unwrap()),
    )
    .unwrap();
    let checkout = root.join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();
    (dir, checkout)
}

#[test]
fn a_worktree_reads_the_common_directory_config() {
    // A named repository tells which config counts: the common one names
    // acme while the gitdir names evil, so only the acme name reads.
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-worktree",
        &remote_config("git@github.com:acme/app.git"),
        &remote_config("git@github.com:evil/app.git"),
    );
    assert_own(
        "gh issue view 12 --repo acme/app",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
    assert_foreign(
        "gh issue view 12 --repo evil/app",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
    // And the reverse: an owned gitdir config never qualifies the common one.
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-worktree-gitdir",
        &remote_config("git@github.com:evil/app.git"),
        &remote_config("git@github.com:acme/app.git"),
    );
    assert_own(
        "gh issue view 12 --repo evil/app",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
    assert_foreign(
        "gh issue view 12 --repo acme/app",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn worktree_pointer_failures_have_no_owners() {
    // A `commondir` file past 512 bytes.
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-commondir-size",
        &remote_config("git@github.com:acme/app.git"),
        &remote_config("git@github.com:acme/app.git"),
    );
    let gitdir = workspace
        .join("..")
        .join("repo")
        .join(".git")
        .join("worktrees")
        .join("wt");
    let target = std::fs::read(gitdir.join("commondir")).unwrap();
    let mut over = String::from_utf8(target).unwrap();
    over.pop();
    while over.len() < 513 {
        over.push('/');
    }
    over.push('\n');
    assert_eq!(over.len(), 514);
    std::fs::write(gitdir.join("commondir"), &over).unwrap();
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
    // A symlinked `commondir` file.
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-commondir-link",
        &remote_config("git@github.com:acme/app.git"),
        &remote_config("git@github.com:acme/app.git"),
    );
    let gitdir = workspace
        .join("..")
        .join("repo")
        .join(".git")
        .join("worktrees")
        .join("wt");
    let real = gitdir.join("commondir.real");
    std::fs::rename(gitdir.join("commondir"), &real).unwrap();
    std::os::unix::fs::symlink(&real, gitdir.join("commondir")).unwrap();
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
    // A `commondir` target outside version control.
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-commondir-outside",
        &remote_config("git@github.com:acme/app.git"),
        &remote_config("git@github.com:acme/app.git"),
    );
    let gitdir = workspace
        .join("..")
        .join("repo")
        .join(".git")
        .join("worktrees")
        .join("wt");
    let elsewhere = workspace.join("..").join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(
        elsewhere.join("config"),
        remote_config("git@github.com:acme/app.git"),
    )
    .unwrap();
    std::fs::write(
        gitdir.join("commondir"),
        format!("{}\n", elsewhere.to_str().unwrap()),
    )
    .unwrap();
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
    // A symlinked `.git` file.
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-gitfile-link",
        &remote_config("git@github.com:acme/app.git"),
        &remote_config("git@github.com:acme/app.git"),
    );
    let real = workspace.join(".git.real");
    std::fs::rename(workspace.join(".git"), &real).unwrap();
    std::os::unix::fs::symlink(&real, workspace.join(".git")).unwrap();
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn a_commondir_directory_has_no_owners() {
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-commondir-directory",
        &remote_config("git@github.com:acme/app.git"),
        &remote_config("git@github.com:acme/app.git"),
    );
    let gitdir = workspace
        .join("..")
        .join("repo")
        .join(".git")
        .join("worktrees")
        .join("wt");
    let pointer = gitdir.join("commondir");
    std::fs::remove_file(&pointer).unwrap();
    std::fs::create_dir(&pointer).unwrap();
    // Only `!meta.is_file()` holds for this ordinary directory.
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn the_gitfile_size_cap_holds_at_512_bytes() {
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-gitfile-size",
        &remote_config("git@github.com:acme/app.git"),
        &remote_config("git@github.com:acme/app.git"),
    );
    let base = "gitdir: ../repo/.git/worktrees/wt";
    for (size, reads) in [(511, true), (512, true), (513, false)] {
        let content = format!("{base}{}\n", "/".repeat(size - 1 - base.len()));
        assert_eq!(content.len(), size, "{size}");
        std::fs::write(workspace.join(".git"), &content).unwrap();
        if reads {
            assert_own(
                "gh issue view 12",
                &workspace,
                &workspace,
                vec![workspace_path(&workspace, "12")],
            );
        } else {
            assert_foreign(
                "gh issue view 12",
                &workspace,
                &workspace,
                &[],
                vec![workspace_path(&workspace, "12")],
            );
        }
    }
}

#[test]
fn an_unnamed_read_needs_the_workspace_root_and_no_cd() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-unnamed");
    let sub = workspace.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    // Another workdir stays reviewed even with no `cd`.
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &sub,
        &[],
        vec![workspace_path(&sub, "12")],
    );
    // Any `cd` keeps an unnamed read reviewed, wherever the `cd` stands.
    assert_foreign_effect("cd sub && gh issue view 12", &workspace, &workspace);
    assert_foreign_effect("gh issue view 12 && cd sub", &workspace, &workspace);
    // A named repository is judged by its name alone: another workdir or a
    // `cd` changes nothing.
    assert_own(
        "gh issue view 12 --repo acme/app",
        &workspace,
        &sub,
        vec![workspace_path(&sub, "12")],
    );
    assert_own(
        "cd sub && gh issue view 12 --repo acme/app",
        &workspace,
        &workspace,
        vec![sub.to_str().unwrap().to_owned(), workspace_path(&sub, "12")],
    );
    // An explicit `.` workdir is the workspace root.
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace.join("."),
        vec![format!("{}/./12", workspace.to_str().unwrap())],
    );
    // The constructed escape: a workdir whose own clone names an attacker
    // owner never widens the workspace's owners.
    let (_evil, evil) = owner_workspace(
        "fiber-shell-gh-escape",
        &remote_config("git@github.com:evil/app.git"),
    );
    assert_foreign(
        "gh issue view 1 --repo evil/app",
        &workspace,
        &evil,
        &[],
        vec![workspace_path(&evil, "1")],
    );
    assert_foreign(
        "gh issue view 1",
        &workspace,
        &evil,
        &[],
        vec![workspace_path(&evil, "1")],
    );
}

#[test]
fn search_and_configured_entries_keep_network() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-search");
    // A list query can name another repository through `repo:`, so any
    // `--search` or `-S` keeps the reviewed path even with an own `--repo`.
    for command in [
        "gh issue list --repo acme/app --search x",
        "gh pr list --repo acme/app --search=x",
        "gh pr list --repo acme/app -S x",
        "gh issue list -S x --repo acme/app",
        "gh issue list --repo acme/app --search repo:evil/other",
    ] {
        assert_foreign(
            command,
            &workspace,
            &workspace,
            &[],
            vec![workspace.to_str().unwrap().to_owned()],
        );
    }
    // A `--search` word consumed as another flag's value is no query: the
    // call resolves its repository from the workspace's remotes.
    assert_own(
        "gh pr list --state --search",
        &workspace,
        &workspace,
        vec![workspace.to_str().unwrap().to_owned()],
    );
    // A configured `gh` entry keeps `network`: only the built-in reads
    // qualify, because Fiber knows their flags and operands.
    let status = [configured("gh pr status", &["--json="])];
    assert_foreign(
        "gh pr status 123",
        &workspace,
        &workspace,
        &status,
        vec![workspace_path(&workspace, "123")],
    );
    // `-S` takes its query as its value, so a bare `-S` is not read-only.
    for command in ["gh pr list -S", "gh pr list --search"] {
        let effects = classify(command, &workspace, &workspace, &[]);
        assert_eq!(
            effects.declared.effects,
            vec![Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
}

#[test]
fn a_repo_flag_consumed_as_a_value_stays_reviewed() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-consumed");
    // `--json` consumes `--repo`, so the call names nothing: the `123`
    // operand alone would read as unnamed, but the consumed flag forces
    // the reviewed path.
    assert_foreign(
        "gh pr view --json --repo 123",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "123")],
    );
    assert_foreign(
        "gh pr view --json --repo",
        &workspace,
        &workspace,
        &[],
        vec![workspace.to_str().unwrap().to_owned()],
    );
}

#[test]
fn repo_value_forms_name_their_owner_or_fail_closed() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-repovalue");
    for command in [
        "gh pr view 3 -R acme/other",
        "gh pr view 3 --repo=acme/other",
        "gh pr view 3 --repo ACME/other",
    ] {
        assert_own(
            command,
            &workspace,
            &workspace,
            vec![workspace_path(&workspace, "3")],
        );
    }
    for command in [
        "gh pr view 3 --repo evil/other",
        "gh pr view 3 --repo=evil/other",
        "gh pr view 3 --repo v",
        "gh pr view 3 --repo=",
        "gh pr view 3 --repo acme/",
        "gh pr view 3 --repo /other",
    ] {
        assert_foreign(
            command,
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, "3")],
        );
    }
    // A host-bearing `--repo` value reaches past the default endpoint, so
    // the call stays `executes` as before.
    for command in [
        "gh pr view 3 --repo h/o/r",
        "gh pr view 3 --repo=github.corp/o/r",
    ] {
        let effects = classify(command, &workspace, &workspace, &[]);
        assert_eq!(
            effects.declared.effects,
            vec![Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
}

#[test]
fn a_positional_repo_reference_is_never_an_unnamed_default() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-positional");
    // `owner/repo` names a repository outside the ticket's named forms, so
    // it keeps the reviewed path even for a workspace owner.
    assert_foreign(
        "gh repo view acme/app",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "acme/app")],
    );
    // With no operand the repository resolves from the workspace's remotes.
    assert_own(
        "gh repo view",
        &workspace,
        &workspace,
        vec![workspace.to_str().unwrap().to_owned()],
    );
    // A qualifying GitHub URL operand is judged by its owner.
    assert_own(
        "gh repo view https://github.com/acme/app",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "https://github.com/acme/app")],
    );
    assert_foreign(
        "gh repo view https://github.com/evil/app",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "https://github.com/evil/app")],
    );
}

#[test]
fn url_operand_failures_keep_the_reviewed_path() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-urloperand");
    // Only an `https://github.com/<owner>/<repo>/...` operand names its
    // repository: every other operand with a slash keeps the reviewed
    // path instead of passing as an unnamed default.
    for operand in [
        "https://evil.com/acme/app",
        "https://github.com:443/acme/app",
        "https://user@github.com/acme/app",
        "http://github.com/acme/app",
        "https://github.com/acme",
        "https://github.com/",
        "https://github.com",
        "ssh://git@github.com/acme/app",
        "git@github.com:acme/app",
    ] {
        let command = format!("gh pr view {operand}");
        assert_foreign(
            &command,
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, operand)],
        );
    }
}

#[test]
fn mixed_stages_fold_toward_review() {
    // `docs/tools.md`, "Shell", "Effects" promises the paths each stage
    // names, not their order, so mixed stages assert membership.
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-mixed");
    let effects = classify(
        "gh issue view 1 --repo acme/app && ls",
        &workspace,
        &workspace,
        &[],
    );
    assert_eq!(effects.declared.effects, vec![Effect::Reads]);
    assert_eq!(
        BTreeSet::from_iter(effects.declared.paths.unwrap_or_default()),
        BTreeSet::from([
            workspace_path(&workspace, "1"),
            workspace.to_str().unwrap().to_owned(),
        ]),
    );
    let effects = classify(
        "gh issue view 1 --repo acme/app && gh pr view 2 --repo evil/other",
        &workspace,
        &workspace,
        &[],
    );
    assert_eq!(effects.declared.effects, vec![Effect::Network]);
    assert_eq!(
        effects.hosts,
        Some(vec!["https://api.github.com/".to_owned()]),
    );
    assert_eq!(
        BTreeSet::from_iter(effects.declared.paths.unwrap_or_default()),
        BTreeSet::from([
            workspace_path(&workspace, "1"),
            workspace_path(&workspace, "2"),
        ]),
    );
}

#[test]
fn a_repo_word_consumed_as_a_value_does_not_select_a_host() {
    // Only a `--repo`/`-R` in flag position trips the host check: `--json`
    // consumes the following `--repo`, so `h/o/r` is an operand and the
    // call stays `network` instead of becoming `executes`. A consumed
    // `--repo=` likewise names no host: with no operand left, the read
    // resolves from the workspace and stays `reads`.
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-consumed-repo");
    assert_foreign(
        "gh pr view --json --repo h/o/r",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "h/o/r")],
    );
    assert_own(
        "gh pr view --json --repo=h/o/r",
        &workspace,
        &workspace,
        vec![workspace.to_str().unwrap().to_owned()],
    );
}

#[test]
fn a_workspace_inside_a_repository_uses_that_repository() {
    let dir = fakes::TempDir::new("fiber-shell-gh-nested");
    let root = dir.path().join("ws");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(
        root.join(".git").join("config"),
        remote_config("git@github.com:acme/app.git"),
    )
    .unwrap();
    let workspace = root.join("sub");
    std::fs::create_dir_all(&workspace).unwrap();
    assert_own(
        "gh issue view 7",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "7")],
    );
}

#[test]
fn an_own_read_keeps_the_declared_paths_without_a_host() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-paths");
    assert_own(
        "gh pr view 123 --json number,title --comments --repo acme/other",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "123")],
    );
}

#[test]
fn rejected_gh_forms_still_execute_with_owners_present() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-rejected");
    for command in [
        "gh",
        "gh api repos/acme/app",
        "gh pr merge 123",
        "gh pr view 123 --web",
        "gh pr view -- x",
        "gh -R acme/app pr view 123",
        "gh issue close 123",
        "gh repo clone acme/app",
        "gh auth status",
        "gh run watch 123",
        "GH_REPO=acme/app gh issue view 12",
        "GH_HOST=example.com gh pr view 123",
    ] {
        let effects = classify(command, &workspace, &workspace, &[]);
        assert_eq!(
            effects.declared.effects,
            vec![Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
}

fn assert_executes_with(
    command: &str,
    workspace: &Path,
    workdir: &Path,
    entries: &[(String, Vec<String>)],
) {
    let effects = classify(command, workspace, workdir, entries);
    assert_eq!(
        effects.declared.effects,
        vec![Effect::Executes],
        "{command}"
    );
    assert!(!effects.declared.reversible, "{command}");
    assert!(effects.declared.paths.is_none(), "{command}");
}

// Fallback host disjunction (`classify.rs`, `repo_selects_host` with no
// built-in entry): a configured `gh` read reaches `executes` when either
// the attached `--repo=` form or the separate `--repo`/`-R` pair names a
// host, and stays `network` otherwise.
#[test]
fn fallback_repo_host_forms_execute() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-fallback-host");
    let entries = [
        configured("gh", &["--repo=", "-R=", "--json="]),
        configured("gh custom", &["--repo=", "-R="]),
    ];
    // Attached host alone trips the first disjunct.
    assert_executes_with("gh custom --repo=h/o/r", &workspace, &workspace, &entries);
    // Separate long and short pairs trip the second disjunct.
    assert_executes_with("gh custom --repo h/o/r", &workspace, &workspace, &entries);
    assert_executes_with("gh custom -R h/o/r", &workspace, &workspace, &entries);
}

#[test]
fn unclean_repo_segments_stay_foreign() {
    // `clean_segment` (`gh_owner.rs`): a space pins the first
    // `whitespace || control` disjunction, while `?` pins the second
    // `control || marks` one. Both keep the call reviewed.
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-clean-seg");
    for command in [
        "gh pr view 3 --repo \"acme/a b\"",
        "gh pr view 3 --repo \"acme/app?\"",
        "gh pr view 3 --repo \"acme/app#frag\"",
        "gh pr view 3 --repo \"acme/app@frag\"",
        "gh pr view 3 --repo \"acme/app:1\"",
        "gh pr view 3 --repo \"acme/\u{1}\"",
    ] {
        assert_foreign(
            command,
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, "3")],
        );
    }
    // A control-only segment pins the first disjunction the same way.
    assert_foreign(
        "gh pr view 3 --repo \"acme/app\u{1}\"",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "3")],
    );
}

#[test]
fn ssh_remote_user_and_host_mismatches_have_no_owners() {
    // `remote_owner` (`gh_owner.rs`): a non-`git` user pins the first
    // `||`, while a non-GitHub host pins the second.
    for (index, url) in [
        "ssh://evil@github.com/acme/app",
        "ssh://git@evil.com/acme/app",
    ]
    .iter()
    .enumerate()
    {
        let (_dir, workspace) = owner_workspace(
            &format!("fiber-shell-gh-ssh-user-{index}"),
            &remote_config(url),
        );
        assert_foreign(
            "gh issue view 12",
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, "12")],
        );
    }
}

#[test]
fn scp_remote_extra_parts_and_empty_repo_have_no_owners() {
    // `remote_owner` scp form: a third path part pins the first `||`,
    // while an empty repository pins the second.
    for (index, url) in ["git@github.com:acme/app/extra", "git@github.com:acme/"]
        .iter()
        .enumerate()
    {
        let (_dir, workspace) =
            owner_workspace(&format!("fiber-shell-gh-scp-{index}"), &remote_config(url));
        assert_foreign(
            "gh issue view 12",
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, "12")],
        );
    }
}

#[test]
fn url_path_with_empty_repo_stays_foreign() {
    // `url_path_owner`: `https://github.com/acme/` names no owner since
    // the repository segment is empty.
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-url-empty-repo");
    assert_foreign(
        "gh pr view https://github.com/acme/",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "https://github.com/acme/")],
    );
}

#[test]
fn fallback_non_host_forms_stay_network() {
    let (_dir, workspace) = acme_workspace("fiber-shell-gh-fallback-nohost");
    let entries = [configured("gh", &["--repo=", "-R=", "--json="])];
    // A separate `--repo` without a host: the left side holds while the
    // host side fails, so the pair stays `network`.
    let effects = classify(
        "gh custom --repo acme/other",
        &workspace,
        &workspace,
        &entries,
    );
    assert_eq!(
        effects.declared.effects,
        vec![Effect::Network],
        "gh custom --repo acme/other"
    );
    assert_eq!(
        effects.hosts,
        Some(vec!["https://api.github.com/".to_owned()]),
        "gh custom --repo acme/other"
    );
    // An unrelated value flag holding a host: the host holds while the
    // flag side fails, so the call stays `network`.
    let effects = classify("gh custom --json h/o/r", &workspace, &workspace, &entries);
    assert_eq!(
        effects.declared.effects,
        vec![Effect::Network],
        "gh custom --json h/o/r"
    );
    assert_eq!(
        effects.hosts,
        Some(vec!["https://api.github.com/".to_owned()]),
        "gh custom --json h/o/r"
    );
    // A separate `-R` without a host also stays `network`.
    let effects = classify("gh custom -R acme/other", &workspace, &workspace, &entries);
    assert_eq!(
        effects.declared.effects,
        vec![Effect::Network],
        "gh custom -R acme/other"
    );
}

#[test]
fn gitdir_pointer_outside_dot_git_has_no_owners() {
    // `gitdir_common`: a canonical gitdir outside any `.git` component
    // pins the `under_dot_git` disjunct.
    let dir = fakes::TempDir::new("fiber-shell-gh-gitdir-outside");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(
        elsewhere.join("config"),
        remote_config("git@github.com:acme/app.git"),
    )
    .unwrap();
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", elsewhere.to_str().unwrap()),
    )
    .unwrap();
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn commondir_as_directory_has_no_owners() {
    // `commondir_common`: a `commondir` that is a directory (neither a
    // symlink nor a file) pins the `is_symlink || !is_file` disjunction.
    let (_dir, workspace) = worktree_workspace(
        "fiber-shell-gh-commondir-dir",
        &remote_config("git@github.com:acme/app.git"),
        &remote_config("git@github.com:acme/app.git"),
    );
    let gitdir = workspace
        .join("..")
        .join("repo")
        .join(".git")
        .join("worktrees")
        .join("wt");
    std::fs::remove_file(gitdir.join("commondir")).unwrap();
    std::fs::create_dir_all(gitdir.join("commondir")).unwrap();
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn missing_commondir_falls_back_to_the_gitdir() {
    // `commondir_common`: a missing `commondir` file resolves to the
    // gitdir itself, so an owned gitdir config reads.
    let dir = fakes::TempDir::new("fiber-shell-gh-commondir-missing");
    let root = dir.path().join("ws");
    let gitdir = root.join("repo").join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::write(
        gitdir.join("config"),
        remote_config("git@github.com:acme/app.git"),
    )
    .unwrap();
    let checkout = root.join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();
    assert_own(
        "gh issue view 12",
        &checkout,
        &checkout,
        vec![workspace_path(&checkout, "12")],
    );
}

#[test]
fn other_io_errors_fail_closed_in_common_dir() {
    // `common_dir`: an I/O error other than `NotFound` (here `ENOTDIR`
    // through a file component) fails closed instead of walking to the
    // parent repository.
    let dir = fakes::TempDir::new("fiber-shell-gh-enotdir");
    let root = dir.path().join("ws");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(
        root.join(".git").join("config"),
        remote_config("git@github.com:acme/app.git"),
    )
    .unwrap();
    let file = root.join("file");
    std::fs::write(&file, "x").unwrap();
    let workspace = file.join("ws");
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn backslash_continuation_joins_config_lines() {
    // `logical_lines` and `trailing_backslashes`: a single trailing
    // backslash joins the next line, so the split section header still
    // reads. Without the join the first line never closes, so the config
    // is malformed.
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-continuation",
        "[remote \"ori\\\ngin\"]\n\turl = git@github.com:acme/app.git\n",
    );
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn trailing_continuation_without_newline_still_reads() {
    // `logical_lines`: a trailing backslash on the final line without a
    // trailing newline still joins, so the remote URL reads.
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-trailing-continuation",
        "[remote \"origin\"]\n\turl = git@github.com:acme/app.git\\",
    );
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn section_header_with_trailing_comment_stays_own() {
    // `strip_comment`: a `#` after the section header is a comment, so
    // the remote still reads. The tab variant pins the tab side of the
    // whitespace disjunction.
    for (index, config) in [
        "[remote \"origin\"] # the team\n\turl = git@github.com:acme/app.git\n",
        "[remote \"origin\"]\t# the team\n\turl = git@github.com:acme/app.git\n",
    ]
    .iter()
    .enumerate()
    {
        let (_dir, workspace) =
            owner_workspace(&format!("fiber-shell-gh-section-comment-{index}"), config);
        assert_own(
            "gh issue view 12",
            &workspace,
            &workspace,
            vec![workspace_path(&workspace, "12")],
        );
    }
}

#[test]
fn empty_section_name_has_no_owners() {
    // `parse_section`: `[]` names nothing, so the whole config is
    // malformed even beside a valid remote.
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-empty-section",
        "[]\n[remote \"origin\"]\n\turl = git@github.com:acme/app.git\n",
    );
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn dotted_and_dashed_section_names_read() {
    // `parse_section`: `.` and `-` are word characters in section names.
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-section-chars",
        "[a.b-c]\n\tfoo = bar\n[remote \"origin\"]\n\turl = git@github.com:acme/app.git\n",
    );
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn bare_subsection_with_whitespace_has_no_owners() {
    // `parse_section`: a bare subsection holding whitespace is malformed.
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-bare-subsection",
        "[remote my sub]\n\turl = git@github.com:acme/app.git\n",
    );
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn bare_boolean_key_beside_a_remote_still_reads() {
    // `split_key`: a bare boolean key validates, so the remote beside it
    // still reads.
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-bare-key",
        "[remote \"origin\"]\n\tfoo\n\turl = git@github.com:acme/app.git\n",
    );
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn invalid_key_lines_have_no_owners() {
    // `split_key` and `valid_key`: an empty key, and keys holding `.`,
    // `-` or `_` where the mutant forbids them, fail closed. Each config
    // holds one invalid line beside a valid remote, so the whole config
    // has no owners.
    for (index, config) in [
        "[remote \"origin\"]\n\t= value\n\turl = git@github.com:acme/app.git\n",
        "[remote \"origin\"]\n\tnot a key!!!\n\turl = git@github.com:acme/app.git\n",
    ]
    .iter()
    .enumerate()
    {
        let (_dir, workspace) =
            owner_workspace(&format!("fiber-shell-gh-invalid-key-{index}"), config);
        assert_foreign(
            "gh issue view 12",
            &workspace,
            &workspace,
            &[],
            vec![workspace_path(&workspace, "12")],
        );
    }
}

#[test]
fn dotted_dashed_and_underscored_keys_read() {
    // `valid_key`: `.`, `-` and `_` are word characters in key names, so
    // extra keys holding them do not disturb the remote.
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-key-chars",
        "[remote \"origin\"]\n\tmy.key = foo\n\tmy-key = foo\n\tmy_key = foo\n\turl = git@github.com:acme/app.git\n",
    );
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn quoted_escapes_in_subsections_read() {
    // `quoted_value`: each escape in a subsection still yields a
    // non-empty name, so the remote still reads.
    for (index, escape) in ["\\n", "\\t", "\\b", "\\\"", "\\\\"].iter().enumerate() {
        let config = format!("[remote \"a{escape}b\"]\n\turl = git@github.com:acme/app.git\n");
        let (_dir, workspace) = owner_workspace(&format!("fiber-shell-gh-escape-{index}"), &config);
        assert_own(
            "gh issue view 12",
            &workspace,
            &workspace,
            vec![workspace_path(&workspace, "12")],
        );
    }
}

#[test]
fn empty_quoted_subsection_has_no_owners() {
    // `unescape`: an empty quoted subsection is empty, so the remote
    // does not count.
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-empty-subsection",
        "[remote \"\"]\n\turl = git@github.com:acme/app.git\n",
    );
    assert_foreign(
        "gh issue view 12",
        &workspace,
        &workspace,
        &[],
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn value_with_equals_inside_quotes_reads() {
    // `split_unquoted`: an `=` inside quotes is not a separator, so the
    // remote still reads (`=` is allowed in a segment).
    let (_dir, workspace) = owner_workspace(
        "fiber-shell-gh-equals-in-quotes",
        "[remote \"origin\"]\n\turl = \"git@github.com:acme/app=bar\"\n",
    );
    assert_own(
        "gh issue view 12",
        &workspace,
        &workspace,
        vec![workspace_path(&workspace, "12")],
    );
}

#[test]
fn rm_starts_suffix_skips_wrapped_rm_word() {
    // `rm_starts`: the suffix starts after the `rm` word, so a wrapper
    // such as `sudo` does not shift it.
    let words = ["sudo", "rm", "-rf", "tmp"];
    let suffixes: Vec<&[&str]> = super::rm_starts(&words).collect();
    assert_eq!(suffixes, [["-rf", "tmp"].as_slice()]);
}

#[test]
fn git_starts_suffix_skips_git_and_subcommand_words() {
    // `git_starts`: the suffix starts after `git` and the subcommand, so
    // a wrapper such as `sudo` does not shift it.
    let words = ["sudo", "git", "reset", "--hard", "HEAD"];
    let suffixes: Vec<&[&str]> = super::git_starts(&words, "reset").collect();
    assert_eq!(suffixes, [["--hard", "HEAD"].as_slice()]);
}
