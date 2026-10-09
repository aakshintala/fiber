use std::path::Path;

use contract::shapes::Effect;
use contract::tool::Effects;

use super::super::read_only::COMMANDS;
use super::{Lexer, Part, Word, classify};

fn plain(text: &str) -> Word {
    Word {
        raw: text.to_owned(),
        cooked: text.to_owned(),
    }
}

fn quoted(raw: &str, cooked: &str) -> Word {
    Word {
        raw: raw.to_owned(),
        cooked: cooked.to_owned(),
    }
}

fn lexed(command: &str) -> Vec<Part> {
    Lexer::new(command)
        .run()
        .unwrap_or_else(|| panic!("unreadable: {command:?}"))
}

fn assert_unreadable(command: &str) {
    assert!(Lexer::new(command).run().is_none(), "readable: {command:?}");
}

#[test]
fn operators_outside_quotes_split_and_inside_quotes_do_not() {
    assert_eq!(
        lexed("ls && git status || cat; head | tail"),
        vec![
            Part {
                words: vec![plain("ls")]
            },
            Part {
                words: vec![plain("git"), plain("status")]
            },
            Part {
                words: vec![plain("cat")]
            },
            Part {
                words: vec![plain("head")]
            },
            Part {
                words: vec![plain("tail")]
            },
        ]
    );
    assert_eq!(
        lexed("ls&&git||cat;head|tail"),
        vec![
            Part {
                words: vec![plain("ls")]
            },
            Part {
                words: vec![plain("git")]
            },
            Part {
                words: vec![plain("cat")]
            },
            Part {
                words: vec![plain("head")]
            },
            Part {
                words: vec![plain("tail")]
            },
        ]
    );
    assert_eq!(
        lexed("echo 'ls && git || cat; head | tail'"),
        vec![Part {
            words: vec![
                plain("echo"),
                quoted(
                    "'ls && git || cat; head | tail'",
                    "ls && git || cat; head | tail"
                ),
            ]
        }]
    );
    assert_eq!(
        lexed("echo \"ls && git || cat; head | tail\""),
        vec![Part {
            words: vec![
                plain("echo"),
                quoted(
                    "\"ls && git || cat; head | tail\"",
                    "ls && git || cat; head | tail",
                ),
            ]
        }]
    );
    assert_eq!(
        lexed("echo 'a&&b'&&ls"),
        vec![
            Part {
                words: vec![plain("echo"), quoted("'a&&b'", "a&&b")]
            },
            Part {
                words: vec![plain("ls")]
            },
        ]
    );
}

#[test]
fn characters_outside_the_plain_set_are_unreadable() {
    for command in [
        "echo $(x)",
        "echo `x`",
        "echo <(x)",
        "echo >(x)",
        "echo <f",
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
        "echo café",
        "echo λ",
        "echo a\u{00a0}b",
        "echo 'unterminated",
        "echo \"unterminated",
        "echo \"a$b\"",
        "echo \"a`b\"",
        "echo \"\\$\"",
        "echo \"\\`\"",
        "echo \"a\\\"",
        "echo \"a\\\\\"",
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
        assert_unreadable(command);
    }
}

#[test]
fn a_backslash_before_a_plain_character_inside_double_quotes_is_literal() {
    assert_eq!(
        lexed("echo \"a\\b\""),
        vec![Part {
            words: vec![plain("echo"), quoted("\"a\\b\"", "a\\b"),]
        }]
    );
    assert_eq!(
        lexed("echo \"a\\|b\""),
        vec![Part {
            words: vec![plain("echo"), quoted("\"a\\|b\"", "a\\|b"),]
        }]
    );
}

#[test]
fn a_stderr_redirect_lexes_as_one_word() {
    assert_eq!(
        lexed("ls 2>/dev/null"),
        vec![Part {
            words: vec![plain("ls"), plain("2>/dev/null")]
        }]
    );
    assert_eq!(
        lexed("ls 2>&1 | head"),
        vec![
            Part {
                words: vec![plain("ls"), plain("2>&1")]
            },
            Part {
                words: vec![plain("head")]
            },
        ]
    );
    assert_eq!(
        lexed("cat '2>&1'"),
        vec![Part {
            words: vec![plain("cat"), quoted("'2>&1'", "2>&1")]
        }]
    );
    assert_reads("git 2>/dev/null status");
    assert_eq!(
        classified("cat '2>&1'").declared.paths,
        Some(vec!["/work/2>&1".to_owned()])
    );
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
}

#[test]
fn anything_but_the_two_exact_redirects_is_unreadable() {
    for command in [
        "ls 2> /dev/null",
        "ls 12>/dev/null",
        "ls 1>/dev/null",
        "ls x2>/dev/null",
        "ls \"2\">/dev/null",
        "ls 2>>/dev/null",
        "ls 2>&-",
        "ls 2>&2",
        "ls 2>/dev/nullx",
        "ls 2>&12",
        "2>/dev/null ls",
        "2>&1 ls",
        "echo 2>(x)",
        "ls 2>/dev/null/x",
        "ls 2>/dev/zero",
        "ls >/dev/null",
        "ls 1>/dev/null",
        "ls 2>file",
        "ls >file",
        "ls &>file",
        "ls 2>\tfile",
    ] {
        assert_unreadable(command);
    }
}

#[test]
fn a_newline_or_operator_inside_quotes_stays_one_word() {
    assert_eq!(
        lexed("echo 'a\nb' \"c\nd\""),
        vec![Part {
            words: vec![
                plain("echo"),
                quoted("'a\nb'", "a\nb"),
                quoted("\"c\nd\"", "c\nd"),
            ]
        }]
    );
    assert_eq!(
        lexed("echo '$HOME' '*' \"~\""),
        vec![Part {
            words: vec![
                plain("echo"),
                quoted("'$HOME'", "$HOME"),
                quoted("'*'", "*"),
                quoted("\"~\"", "~"),
            ]
        }]
    );
}

#[test]
fn cooked_words_drop_quotes_and_keep_the_text() {
    assert_eq!(
        lexed("git diff '--output=f' --out'put'=f"),
        vec![Part {
            words: vec![
                plain("git"),
                plain("diff"),
                quoted("'--output=f'", "--output=f"),
                quoted("--out'put'=f", "--output=f"),
            ]
        }]
    );
    assert_eq!(
        lexed("echo \"hello\" a'b'c"),
        vec![Part {
            words: vec![
                plain("echo"),
                quoted("\"hello\"", "hello"),
                quoted("a'b'c", "abc"),
            ]
        }]
    );
    assert_eq!(
        lexed("npm  test\t--"),
        vec![Part {
            words: vec![plain("npm"), plain("test"), plain("--")]
        }]
    );
    assert_eq!(
        lexed("echo a_b-c.d/e:f=g@h%i+j,k"),
        vec![Part {
            words: vec![plain("echo"), plain("a_b-c.d/e:f=g@h%i+j,k")]
        }]
    );
    assert_eq!(
        lexed("''"),
        vec![Part {
            words: vec![quoted("''", "")]
        }]
    );
}

fn classified(command: &str) -> Effects {
    classify(command, Path::new("/work"))
}

fn assert_reads(command: &str) {
    let effects = classified(command);
    assert_eq!(effects.declared.effects, vec![Effect::Reads], "{command}");
    assert!(effects.declared.reversible, "{command}");
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
    assert!(classified("git branch --show-current").declared.paths.is_none());
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
fn hostile_git_forms_execute() {
    for command in [
        "git -c a=b log",
        "git -c diff.external=x diff",
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
        "git branch --show-current x",
        "git branch --show-current --show-current",
        "git checkout x",
        "git stash",
        "git config x",
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
        classified("sed -n 1p /home/me/.fiber/credentials/k").declared.paths,
        Some(vec!["/home/me/.fiber/credentials/k".to_owned()])
    );
    assert_reads("sed -n 1p a b");
    assert_reads("'sed' -n 1p f");
    assert_reads("sed -n 1p f 2>/dev/null");
    for command in [
        "sed -n 1p /proc/self/environ",
        "/usr/bin/sed -n 1p f",
        "sed -n 1p f > out",
        "sed -n 1p f >/dev/null",
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
fn only_echo_and_pwd_declare_no_paths() {
    for command in COMMANDS {
        let declares = command.name != "echo"
            && command.name != "pwd"
            && command.name != "git rev-parse";
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
        ("git status", true),
        ("git diff", true),
        ("git log", true),
        ("git show", true),
        ("git rev-parse", true),
        ("git ls-files", true),
        ("git blame", true),
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
        Some(vec!["/work/src".to_owned(), "/work".to_owned()])
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
