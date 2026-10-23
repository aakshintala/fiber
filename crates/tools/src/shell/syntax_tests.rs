#![allow(clippy::unwrap_used, reason = "tests unwrap")]

use super::{MAX_COMMAND_BYTES, Redirect, Script, Simple, Stage, cook_quoted, parse};

fn parsed(command: &str) -> Script {
    parse(command).unwrap_or_else(|| panic!("{command:?} parsed to nothing"))
}

fn assert_none(command: &str) {
    assert!(parse(command).is_none(), "{command:?} parsed");
}

/// The script holds one element with one simple stage; anything else fails
/// the test with the command that produced it.
fn only_simple(command: &str) -> Simple {
    let script = parsed(command);
    assert_eq!(script.elements.len(), 1, "{command:?}");
    let mut elements = script.elements;
    let element = elements.pop().unwrap();
    assert_eq!(element.stages.len(), 1, "{command:?}");
    let mut stages = element.stages;
    match stages.pop().unwrap() {
        Stage::Simple(simple) => simple,
        Stage::Opaque => panic!("{command:?} parsed opaque"),
    }
}

fn assert_opaque(command: &str) {
    let script = parsed(command);
    assert_eq!(script.elements.len(), 1, "{command:?}");
    let mut elements = script.elements;
    let element = elements.pop().unwrap();
    assert_eq!(element.stages.len(), 1, "{command:?}");
    let mut stages = element.stages;
    match stages.pop().unwrap() {
        Stage::Simple(_) => panic!("{command:?} parsed simple"),
        Stage::Opaque => {}
    }
}

fn first_simple(script: &Script) -> &Simple {
    let element = script.elements.first().unwrap();
    assert_eq!(element.stages.len(), 1);
    match element.stages.first().unwrap() {
        Stage::Simple(simple) => simple,
        Stage::Opaque => panic!("stage opaque"),
    }
}

fn cooked(command: &str) -> Vec<String> {
    only_simple(command)
        .words
        .iter()
        .map(|word| word.cooked.clone())
        .collect()
}

fn raws(command: &str) -> Vec<String> {
    only_simple(command)
        .words
        .iter()
        .map(|word| word.raw.clone())
        .collect()
}

fn tokens(command: &str) -> Vec<String> {
    only_simple(command).tokens.clone()
}

fn assignments(command: &str) -> Vec<(String, String)> {
    only_simple(command).assignments.clone()
}

fn redirect_kinds(simple: &Simple) -> Vec<&str> {
    simple
        .redirects
        .iter()
        .map(|redirect| match redirect {
            Redirect::Discard => "discard",
            Redirect::Merge => "merge",
            Redirect::Input(_) => "input",
        })
        .collect()
}

fn input_cooked(simple: &Simple) -> Vec<String> {
    simple
        .redirects
        .iter()
        .filter_map(|redirect| match redirect {
            Redirect::Input(word) => Some(word.cooked.clone()),
            Redirect::Discard => None,
            Redirect::Merge => None,
        })
        .collect()
}

fn word_cooked(simple: &Simple) -> Vec<&str> {
    simple
        .words
        .iter()
        .map(|word| word.cooked.as_str())
        .collect()
}

#[test]
fn a_simple_command_builds_one_element_with_one_stage() {
    let simple = only_simple("ls -la");
    assert!(simple.assignments.is_empty());
    assert!(simple.redirects.is_empty());
    assert_eq!(
        simple
            .words
            .iter()
            .map(|word| (word.raw.as_str(), word.cooked.as_str()))
            .collect::<Vec<_>>(),
        [("ls", "ls"), ("-la", "-la")]
    );
    assert_eq!(simple.tokens, ["ls", "-la"]);
}

#[test]
fn quoted_words_cook_and_keep_their_raw_spelling() {
    assert_eq!(cooked("echo \"hello\" a'b'c"), ["echo", "hello", "abc"]);
    assert_eq!(raws("echo \"hello\" a'b'c"), ["echo", "\"hello\"", "a'b'c"]);
    assert_eq!(
        cooked("git diff '--output=f' --out'put'=f"),
        ["git", "diff", "--output=f", "--output=f"]
    );
    assert_eq!(cooked("''"), [""]);
    assert_eq!(raws("''"), ["''"]);
    assert_eq!(cooked("''''"), [""]);
    assert_eq!(cooked("echo a=b"), ["echo", "a=b"]);
    assert_eq!(cooked("ls =foo"), ["ls", "=foo"]);
    assert_eq!(cooked("head -5"), ["head", "-5"]);
    assert_eq!(cooked("echo 0x1"), ["echo", "0x1"]);
    assert_eq!(cooked("echo 1.5"), ["echo", "1.5"]);
    assert_eq!(cooked("--help"), ["--help"]);
    assert_eq!(cooked("echo -"), ["echo", "-"]);
    assert_eq!(cooked("555 x"), ["555", "x"]);
    assert_eq!(cooked("7 ls"), ["7", "ls"]);
}

#[test]
fn double_quotes_keep_everything_but_dollar_backtick_and_quote() {
    assert_eq!(cooked("echo \"a\\b\""), ["echo", "a\\b"]);
    assert_eq!(cooked("echo 'a\nb'"), ["echo", "a\nb"]);
    assert_eq!(cooked("echo \"c\nd\""), ["echo", "c\nd"]);
}

#[test]
fn quoted_parts_cook_inside_a_concatenation() {
    let simple = only_simple("echo a\"b\"c");
    assert_eq!(word_cooked(&simple), ["echo", "abc"]);
    assert_eq!(simple.words.get(1).unwrap().raw, "a\"b\"c");
}

#[test]
fn cook_quoted_rejects_a_lopsided_spelling() {
    assert_eq!(cook_quoted("\"ab\"", '"'), Some("ab".to_string()));
    assert_eq!(cook_quoted("ab\"", '"'), None);
    assert_eq!(cook_quoted("\"ab", '"'), None);
}

#[test]
fn a_pipeline_builds_one_element_with_one_stage_per_command() {
    let script = parsed("cat README.md | head -50");
    assert_eq!(script.elements.len(), 1);
    let element = script.elements.first().unwrap();
    assert_eq!(element.stages.len(), 2);
    let mut stages = element.stages.iter();
    match stages.next().unwrap() {
        Stage::Simple(first) => assert_eq!(word_cooked(first), ["cat", "README.md"]),
        Stage::Opaque => panic!("first stage opaque"),
    }
    match stages.next().unwrap() {
        Stage::Simple(second) => assert_eq!(word_cooked(second), ["head", "-50"]),
        Stage::Opaque => panic!("second stage opaque"),
    }
    let script = parsed("ls | head | tail");
    let element = script.elements.first().unwrap();
    assert_eq!(element.stages.len(), 3);
}

#[test]
fn a_redirect_around_a_compound_body_opaques_the_pipeline() {
    // The redirect's body holds two elements (a list) or one element with
    // two stages (a pipeline), so the stage never stands alone.
    assert_opaque("a && b >f | cat");
    assert_opaque("a | b >f | cat");
}

#[test]
fn lists_build_one_element_per_command_in_source_order() {
    for (command, want) in [
        ("ls && pwd", vec!["ls", "pwd"]),
        ("ls || pwd", vec!["ls", "pwd"]),
        ("ls; pwd", vec!["ls", "pwd"]),
        ("a && b || c", vec!["a", "b", "c"]),
        ("echo 'a&&b'&&ls", vec!["echo", "ls"]),
    ] {
        let script = parsed(command);
        assert_eq!(script.elements.len(), want.len(), "{command:?}");
        let heads: Vec<String> = script
            .elements
            .iter()
            .map(|element| {
                assert_eq!(element.stages.len(), 1, "{command:?}");
                match element.stages.first().unwrap() {
                    Stage::Simple(simple) => {
                        assert!(!simple.words.is_empty(), "{command:?}");
                        simple.words.first().unwrap().cooked.clone()
                    }
                    Stage::Opaque => panic!("{command:?} has an opaque stage"),
                }
            })
            .collect();
        assert_eq!(heads, want, "{command:?}");
    }
    let quoted = only_simple("echo 'a&&b'");
    assert_eq!(quoted.words.len(), 2);
    assert_eq!(quoted.words.get(1).unwrap().cooked, "a&&b");
    assert_eq!(quoted.words.get(1).unwrap().raw, "'a&&b'");
}

#[test]
fn a_comment_inside_a_list_opaques_it() {
    // The comment is a fourth child of the `list` node, which only the
    // three-child shape stands for.
    assert_opaque("a && # c\nb");
}

#[test]
fn a_list_operand_may_be_a_pipeline() {
    let script = parsed("a && b | c");
    assert_eq!(script.elements.len(), 2);
    assert_eq!(script.elements.get(1).unwrap().stages.len(), 2);
    let script = parsed("a | b && c");
    assert_eq!(script.elements.len(), 2);
    assert_eq!(script.elements.first().unwrap().stages.len(), 2);
}

#[test]
fn assignments_come_first_and_never_count_as_operands() {
    assert_eq!(assignments("FOO=1"), [("FOO".to_string(), "1".to_string())]);
    assert!(only_simple("FOO=1").words.is_empty());
    assert_eq!(
        assignments("FOO=bar ls"),
        [("FOO".to_string(), "bar".to_string())]
    );
    assert_eq!(cooked("FOO=bar ls"), ["ls"]);
    assert_eq!(
        assignments("FOO='a b' ls"),
        [("FOO".to_string(), "a b".to_string())]
    );
    assert_eq!(assignments("FOO="), [("FOO".to_string(), String::new())]);
    assert_eq!(
        assignments("FOO=1 FOO=2 ls"),
        [
            ("FOO".to_string(), "1".to_string()),
            ("FOO".to_string(), "2".to_string())
        ]
    );
    assert_eq!(cooked("a=b"), Vec::<String>::new());
    assert_eq!(cooked("ls FOO=1"), ["ls", "FOO=1"]);
}

#[test]
fn assignments_alone_in_a_pipeline_or_list_stay_visible() {
    let script = parsed("FOO=1 | cat");
    assert_eq!(script.elements.len(), 1);
    match script.elements.first().unwrap().stages.first().unwrap() {
        Stage::Simple(first) => {
            assert_eq!(first.assignments, [("FOO".to_string(), "1".to_string())])
        }
        Stage::Opaque => panic!("assignment stage opaque"),
    }
    let script = parsed("FOO=1 && ls");
    assert_eq!(script.elements.len(), 2);
    match script.elements.first().unwrap().stages.first().unwrap() {
        Stage::Simple(first) => assert!(!first.assignments.is_empty()),
        Stage::Opaque => panic!("assignment stage opaque"),
    }
}

#[test]
fn discard_redirects_cover_stdout_and_descriptors_one_and_two() {
    for command in [
        "ls >/dev/null",
        "ls 1>/dev/null",
        "ls 2>/dev/null",
        "ls 2> /dev/null",
        "2>/dev/null ls",
        "ls >'/dev/null'",
        "ls 2>'/dev/null'",
    ] {
        let simple = only_simple(command);
        assert_eq!(redirect_kinds(&simple), ["discard"], "{command:?}");
    }
    assert_eq!(tokens("ls 2>/dev/null"), ["ls", "2>/dev/null"]);
    assert_eq!(tokens("2>/dev/null ls"), ["2>/dev/null", "ls"]);
    assert_eq!(cooked("ls 2>/dev/null"), ["ls"]);
}

#[test]
fn attached_words_are_operands_not_descriptors() {
    let simple = only_simple("ls x2>/dev/null");
    assert_eq!(word_cooked(&simple), ["ls", "x2"]);
    assert_eq!(simple.tokens, ["ls", "x2", ">/dev/null"]);
    assert_eq!(redirect_kinds(&simple), ["discard"]);
    let simple = only_simple("ls \"2\">/dev/null");
    assert_eq!(word_cooked(&simple), ["ls", "2"]);
    assert_eq!(simple.words.get(1).unwrap().raw, "\"2\"");
    assert_eq!(redirect_kinds(&simple), ["discard"]);
}

#[test]
fn a_spaced_digit_before_a_redirect_stays_an_operand() {
    // The digit is spaced from the redirect, so it is not spelled against
    // it and stays an operand: stealing it would bless descriptor `"3"`.
    let simple = only_simple("ls 3 >/dev/null");
    assert_eq!(word_cooked(&simple), ["ls", "3"]);
    assert_eq!(simple.tokens, ["ls", "3", ">/dev/null"]);
    assert_eq!(redirect_kinds(&simple), ["discard"]);
}

#[test]
fn merge_redirects_cover_descriptors_one_and_two() {
    for command in ["npm test 2>&1", "ls >&2", "ls 1>&2", "ls 2>&2", "2>&1 ls"] {
        let simple = only_simple(command);
        assert_eq!(redirect_kinds(&simple), ["merge"], "{command:?}");
    }
    assert_eq!(tokens("npm test 2>&1"), ["npm", "test", "2>&1"]);
}

#[test]
fn input_redirects_cover_bare_and_zero_descriptors() {
    for command in ["cat <f", "cat < f", "cat 0<f", "cat <0", "cat <'/dev/null'"] {
        let simple = only_simple(command);
        assert_eq!(redirect_kinds(&simple), ["input"], "{command:?}");
        assert_eq!(input_cooked(&simple).len(), 1, "{command:?}");
    }
    let simple = only_simple("cat <f");
    assert_eq!(input_cooked(&simple), ["f"]);
    let simple = only_simple("cat 0<f");
    assert_eq!(word_cooked(&simple), ["cat"]);
    assert_eq!(simple.tokens, ["cat", "<f"]);
    assert_eq!(tokens("cat <f1 <f2"), ["cat", "<f1", "<f2"]);
    assert_eq!(
        redirect_kinds(&only_simple("cat <f1 <f2")),
        ["input", "input"]
    );
}

#[test]
fn redirects_wrap_lists_and_pipelines_at_their_own_end() {
    let script = parsed("ls && pwd 2>/dev/null");
    assert_eq!(script.elements.len(), 2);
    match script.elements.get(1).unwrap().stages.first().unwrap() {
        Stage::Simple(last) => {
            assert_eq!(last.tokens, ["pwd", "2>/dev/null"]);
            assert_eq!(redirect_kinds(last), ["discard"]);
        }
        Stage::Opaque => panic!("trailing list redirect opaque"),
    }
    let script = parsed("a | b 2>/dev/null");
    assert_eq!(script.elements.len(), 1);
    assert_eq!(script.elements.first().unwrap().stages.len(), 2);
    match script.elements.first().unwrap().stages.get(1).unwrap() {
        Stage::Simple(last) => assert_eq!(redirect_kinds(last), ["discard"]),
        Stage::Opaque => panic!("trailing pipe redirect opaque"),
    }
    let script = parsed("2>/dev/null ls && pwd");
    assert_eq!(script.elements.len(), 2);
    let first = first_simple(&script);
    assert_eq!(first.tokens, ["2>/dev/null", "ls"]);
    let script = parsed("git status | head -5 2>/dev/null");
    match script.elements.first().unwrap().stages.get(1).unwrap() {
        Stage::Simple(last) => {
            assert_eq!(word_cooked(last), ["head", "-5"]);
            assert_eq!(redirect_kinds(last), ["discard"]);
        }
        Stage::Opaque => panic!("pipeline redirect opaque"),
    }
}

#[test]
fn a_mid_pipeline_redirect_stays_on_its_stage() {
    let script = parsed("FOO=1 cargo test -p tui 2>&1 | tail -20");
    assert_eq!(script.elements.len(), 1);
    assert_eq!(script.elements.first().unwrap().stages.len(), 2);
    match script.elements.first().unwrap().stages.first().unwrap() {
        Stage::Simple(first) => {
            assert_eq!(first.assignments, [("FOO".to_string(), "1".to_string())]);
            assert_eq!(word_cooked(first), ["cargo", "test", "-p", "tui"]);
            assert_eq!(redirect_kinds(first), ["merge"]);
        }
        Stage::Opaque => panic!("mid-pipeline redirect opaque"),
    }
    let script = parsed("a 2>/dev/null | b");
    match script.elements.first().unwrap().stages.first().unwrap() {
        Stage::Simple(first) => assert_eq!(redirect_kinds(first), ["discard"]),
        Stage::Opaque => panic!("mid-pipeline redirect opaque"),
    }
}

#[test]
fn empty_blank_and_nul_commands_parse_to_nothing() {
    for command in ["", "   ", "\t", "\n", "ls \0", "\0ls"] {
        assert_none(command);
    }
}

#[test]
fn broken_trees_and_bad_separators_parse_to_nothing() {
    for command in [
        "ls &&",
        "; ls",
        "ls |",
        "| ls",
        "ls && && ls",
        "!",
        "echo \"unterminated",
        "echo 'a",
        "echo `",
        "ls;;ls",
        "ls & ls",
        "ls&",
        "echo a\nb",
        "ls;",
        ";",
        "# comment",
        "echo foo #bar",
        "ls #c",
        "ls; #c",
        "cat <> f",
        "ls 00>/dev/null",
        "cat <<EOF",
    ] {
        assert_none(command);
    }
}

#[test]
fn unlisted_statements_become_one_opaque_stage() {
    for command in [
        "(cd x)",
        "echo (x)",
        "cat <(x)",
        "cat >(x)",
        "cat <<EOF\nhi\nEOF",
        "cat <<< hi",
        "for f in *.md; do wc -l \"$f\"; done",
        "select x in y; do z; done",
        "while x; do y; done",
        "if a; then b; fi",
        "case x in y) z;; esac",
        "f() { ls; }",
        "function f { ls; }",
        "{ ls; }",
        "[[ -n x ]]",
        "((x++))",
        "! ls",
        "! ! ls",
        "ls |& head",
        "export FOO=1",
        "export FOO=1 ls",
        ">/dev/null",
        "2>&1",
        "555<f",
    ] {
        assert_opaque(command);
    }
}

#[test]
fn an_unlisted_operand_opaques_only_its_own_element() {
    let script = parsed("a && (cd x)");
    assert_eq!(script.elements.len(), 2);
    match script.elements.get(1).unwrap().stages.first().unwrap() {
        Stage::Simple(_) => panic!("(cd x) parsed simple"),
        Stage::Opaque => {}
    }
}

#[test]
fn unlisted_words_and_redirects_opaque_their_stage() {
    for command in [
        "echo $HOME",
        "echo ${x}",
        "echo $(x)",
        "echo `x`",
        "echo $((1))",
        "echo $'a'",
        "echo a$b",
        "echo \"a$\"",
        "echo \"a\\\"b\"",
        "echo \"a\\\\\"",
        "echo \"a\\$b\"",
        "echo \"a\\`b\"",
        "echo *",
        "echo a*b",
        "echo {a,b}",
        "echo ~",
        "echo ~/x",
        "echo foo#bar",
        "echo a\rb",
        "echo a\x0bb",
        "echo a\x0cb",
        "echo a\x7fb",
        "echo caf\u{e9}",
        "echo a\\b",
        "FOO=$(x) ls",
        "FOO=$(x)",
        "ls 12>/dev/null",
        "ls 0>/dev/null",
        "ls 3>/dev/null",
        "ls >> out",
        "ls &> out",
        "ls &>> out",
        "ls >| out",
        "ls <&0",
        "ls >&-",
        "ls 1<f",
        "ls 2<f",
        "cat < /dev/tcp/x/1",
        "ls > /dev/tcp/x/1",
        "ls >&/dev/null",
        "ls 2>/dev/null/x",
        "ls 2>/dev//null",
        "ls 2>/dev/null 2>file",
    ] {
        assert_opaque(command);
    }
}

#[test]
fn trailing_redirect_words_stay_visible_in_source_order() {
    assert_eq!(
        tokens("ls 2>/dev/null 2>/dev/null"),
        ["ls", "2>/dev/null", "2>/dev/null"]
    );
    assert_eq!(tokens("ls 2>/dev/null 2>&1"), ["ls", "2>/dev/null", "2>&1"]);
    assert_eq!(tokens("2>/dev/null ls 2>&1"), ["2>/dev/null", "ls", "2>&1"]);
}

#[test]
fn the_ten_thousand_byte_cap_fails_closed() {
    let under = format!("ls {}", "a".repeat(9_996));
    assert_eq!(under.len(), 9_999);
    assert!(parse(&under).is_some());
    let exact = format!("ls {}", "a".repeat(9_997));
    assert_eq!(exact.len(), 10_000);
    assert!(parse(&exact).is_some());
    let over = format!("ls {}", "a".repeat(9_998));
    assert_eq!(over.len(), 10_001);
    assert_none(&over);
}

#[test]
fn a_two_thousand_word_list_parses_without_growing_the_stack() {
    let command = std::iter::repeat_n("ls&&", 2_399).collect::<String>() + "ls";
    assert!(command.len() < MAX_COMMAND_BYTES);
    let script = parsed(&command);
    assert_eq!(script.elements.len(), 2_400);
    for element in &script.elements {
        assert_eq!(element.stages.len(), 1);
        match element.stages.first().unwrap() {
            Stage::Simple(simple) => assert_eq!(word_cooked(simple), ["ls"]),
            Stage::Opaque => panic!("list element opaque"),
        }
    }
}

#[test]
fn a_four_thousand_byte_quoted_word_cooks_whole() {
    let command = format!("echo \"{}\"", "b".repeat(4_000));
    let simple = only_simple(&command);
    assert_eq!(simple.words.len(), 2);
    assert_eq!(simple.words.get(1).unwrap().cooked.len(), 4_000);
    assert!(
        simple
            .words
            .get(1)
            .unwrap()
            .cooked
            .bytes()
            .all(|b| b == b'b')
    );
}

#[test]
fn the_kind_table_lists_every_allowed_named_kind_and_token_once() {
    let battery = [
        "a=b ls",
        "a && b",
        "a || b",
        "a; b",
        "a | b",
        "ls 2>/dev/null",
        "ls 2>&1",
        "cat <f",
        "head -5",
        "echo \"a\"",
        "echo 'a'",
        "echo a'b'",
    ];
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .unwrap();
    let mut named = std::collections::BTreeSet::new();
    let mut tokens = std::collections::BTreeSet::new();
    for command in battery {
        let tree = parser.parse(command, None).unwrap();
        assert!(!tree.root_node().has_error(), "{command:?}");
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.is_named() {
                named.insert(node.kind().to_string());
            } else {
                tokens.insert(node.kind().to_string());
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
    }
    assert_eq!(
        named.into_iter().collect::<Vec<_>>(),
        [
            "command",
            "command_name",
            "concatenation",
            "file_descriptor",
            "file_redirect",
            "list",
            "number",
            "pipeline",
            "program",
            "raw_string",
            "redirected_statement",
            "string",
            "string_content",
            "variable_assignment",
            "variable_name",
            "word",
        ]
    );
    for token in [";", "&&", "||", "|", "=", ">", ">&", "<", "\""] {
        assert!(tokens.contains(token), "token {token} never appears");
    }
    for token in &tokens {
        assert!(
            [";", "&&", "||", "|", "=", ">", ">&", "<", "\""].contains(&token.as_str()),
            "unexpected token {token}"
        );
    }
}
