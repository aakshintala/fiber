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
        "echo \"a\\b\"",
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
fn only_echo_and_pwd_declare_no_paths() {
    for command in COMMANDS {
        let declares = command.name != "echo" && command.name != "pwd";
        assert_eq!(command.paths, declares, "{}", command.name);
    }
}

#[test]
fn denied_flags_are_absent_from_every_allowed_list() {
    const DENIED: &[&str] = &[
        "--output",
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
        if command.paths {
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
        Some(vec!["/work/pat".to_owned()])
    );
    assert_eq!(
        classified("rg --glob=pat file").declared.paths,
        Some(vec!["/work/file".to_owned()])
    );
    assert_eq!(
        classified("grep --include='*.rs' file").declared.paths,
        Some(vec!["/work/file".to_owned()])
    );
    assert_eq!(
        classified("grep --include=x y").declared.paths,
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
        Some(vec!["/work/pattern".to_owned(), "/work/file".to_owned()])
    );
}
