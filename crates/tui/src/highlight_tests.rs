//! Tests for the code block lexer.

use super::spans;
use crate::markdown::Role;

/// The runs of `code` in `lang`, as `(role, text)` pairs per line.
fn runs(lang: &str, code: &str) -> Vec<Vec<(Role, String)>> {
    spans(lang, code).expect("a known language")
}

/// The role of the first run whose text is `text`, on any line.
fn role_of(lang: &str, code: &str, text: &str) -> Option<Role> {
    runs(lang, code)
        .into_iter()
        .flatten()
        .find(|(_, run)| run == text)
        .map(|(role, _)| role)
}

fn joined(lines: &[Vec<(Role, String)>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|(_, text)| text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn rust_keywords_numbers_and_comments_take_their_roles() {
    let code = "fn main() { let x = 1; // hi }";
    assert_eq!(
        runs("rust", code),
        vec![vec![
            (Role::Keyword, "fn".to_owned()),
            (Role::CodeText, " ".to_owned()),
            (Role::Function, "main".to_owned()),
            (Role::CodeText, "() { ".to_owned()),
            (Role::Keyword, "let".to_owned()),
            (Role::CodeText, " x ".to_owned()),
            (Role::Operator, "=".to_owned()),
            (Role::CodeText, " ".to_owned()),
            (Role::Number, "1".to_owned()),
            (Role::CodeText, "; ".to_owned()),
            (Role::Comment, "// hi }".to_owned()),
        ]]
    );
}

#[test]
fn an_unknown_or_empty_language_is_none() {
    assert_eq!(spans("brainfuck", "+++"), None);
    assert_eq!(spans("", "x"), None);
}

#[test]
fn aliases_and_case_name_the_same_language() {
    for tag in ["rs", "RUST", "Rust", "rust,ignore", "rust no_run"] {
        assert_eq!(
            spans(tag, "fn"),
            Some(vec![vec![(Role::Keyword, "fn".to_owned())]]),
            "{tag}"
        );
    }
    for (tag, code, keyword) in [
        ("py", "def f", "def"),
        ("js", "const x", "const"),
        ("ts", "interface X", "interface"),
        ("tsx", "interface X", "interface"),
        ("yml", "a: true", "true"),
        ("c++", "template<T>", "template"),
        ("sh", "if x", "if"),
        ("shell", "if x", "if"),
        ("zsh", "if x", "if"),
        ("golang", "func f", "func"),
    ] {
        let role = role_of(tag, code, keyword);
        assert!(
            matches!(role, Some(Role::Keyword | Role::Constant)),
            "{tag}: {role:?}"
        );
    }
}

#[test]
fn every_ruled_language_is_read() {
    for tag in [
        "rust",
        "python",
        "javascript",
        "typescript",
        "tsx",
        "go",
        "c",
        "cpp",
        "java",
        "bash",
        "json",
        "toml",
        "yaml",
        "html",
        "css",
        "sql",
        "diff",
        "markdown",
    ] {
        assert!(spans(tag, "x").is_some(), "{tag}");
    }
}

#[test]
fn lines_join_back_to_the_code_exactly() {
    let samples = [
        (
            "rust",
            "fn f<'a>(x: &'a str) -> char {\n\t'x' // c\n}\n/* a\nb */ \"s\ntwo\"",
        ),
        (
            "python",
            "def f():\n    \"\"\"doc\n    more\"\"\"\n    return 'é' # ✓\n",
        ),
        ("javascript", "const s = `a\n${b}`;\n"),
        ("html", "<a href=\"x\">t &amp; u</a>\n<!-- c\n-->"),
        ("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n same"),
        ("markdown", "# T\n- `x` y\n1. z\n```\n"),
        ("sql", "SELECT 1 -- c\nFROM t"),
        ("bash", "echo \"$HOME\" # c\n"),
        ("go", "x := `raw\nstring`"),
        ("yaml", "key: 'v' # c"),
    ];
    for (lang, code) in samples {
        let lines = runs(lang, code);
        assert_eq!(lines.len(), code.split('\n').count(), "{lang}");
        assert_eq!(joined(&lines), code, "{lang}");
        for line in &lines {
            for pair in line.windows(2) {
                assert_ne!(pair[0].0, pair[1].0, "{lang}: neighbours merge");
            }
        }
    }
}

#[test]
fn strings_types_constants_and_macros() {
    let code = "let s: String = format!(\"a\\\"b\"); MAX_LEN; true; x != y";
    assert_eq!(role_of("rust", code, "\"a\\\"b\""), Some(Role::String));
    assert_eq!(role_of("rust", code, "String"), Some(Role::Type));
    assert_eq!(role_of("rust", code, "format"), Some(Role::Function));
    assert_eq!(role_of("rust", code, "MAX_LEN"), Some(Role::Constant));
    assert_eq!(role_of("rust", code, "true"), Some(Role::Constant));
    assert_eq!(
        runs("rust", "x != y")[0],
        vec![
            (Role::CodeText, "x ".to_owned()),
            (Role::Operator, "!=".to_owned()),
            (Role::CodeText, " y".to_owned()),
        ]
    );
    assert_eq!(role_of("rust", "u32", "u32"), Some(Role::Type));
}

#[test]
fn rust_lifetimes_are_plain_and_char_literals_are_strings() {
    let code = "fn f<'a>(x: &'a str) { 'x'; '\\n'; '\\''; 'é' }";
    assert_eq!(role_of("rust", code, "'x'"), Some(Role::String));
    assert_eq!(role_of("rust", code, "'\\n'"), Some(Role::String));
    assert_eq!(role_of("rust", code, "'\\''"), Some(Role::String));
    assert_eq!(role_of("rust", code, "'é'"), Some(Role::String));
    let first = &runs("rust", code)[0];
    assert!(
        first
            .iter()
            .all(|(role, text)| *role != Role::String || !text.contains("a>"))
    );
}

#[test]
fn a_string_that_may_not_span_lines_stops_at_the_newline() {
    let lines = runs("python", "x = 'open\ny = 2");
    assert_eq!(lines[0].last(), Some(&(Role::String, "'open".to_owned())));
    assert_eq!(lines[1][0], (Role::CodeText, "y ".to_owned()));
}

#[test]
fn a_multiline_string_and_block_comment_run_across_lines() {
    let lines = runs("rust", "/* a\nb */ x");
    assert_eq!(lines[0], vec![(Role::Comment, "/* a".to_owned())]);
    assert_eq!(lines[1][0], (Role::Comment, "b */".to_owned()));
    let lines = runs("python", "s = \"\"\"a\nb\"\"\" + 1");
    assert_eq!(lines[1][0], (Role::String, "b\"\"\"".to_owned()));
    assert_eq!(lines[1].last(), Some(&(Role::Number, "1".to_owned())));
}

#[test]
fn an_unclosed_string_or_comment_runs_to_the_end() {
    assert_eq!(
        runs("rust", "\"abc"),
        vec![vec![(Role::String, "\"abc".to_owned())]]
    );
    assert_eq!(
        runs("c", "/* abc"),
        vec![vec![(Role::Comment, "/* abc".to_owned())]]
    );
}

#[test]
fn numbers_take_radix_exponent_and_suffix_but_not_a_range() {
    assert_eq!(role_of("rust", "0xFF_u8", "0xFF_u8"), Some(Role::Number));
    assert_eq!(role_of("rust", "1.5e3", "1.5e3"), Some(Role::Number));
    let lines = runs("rust", "0..10");
    assert_eq!(
        lines[0],
        vec![
            (Role::Number, "0".to_owned()),
            (Role::CodeText, "..".to_owned()),
            (Role::Number, "10".to_owned()),
        ]
    );
}

#[test]
fn sql_keywords_match_in_any_case() {
    assert_eq!(
        role_of("sql", "SELECT x FROM t", "SELECT"),
        Some(Role::Keyword)
    );
    assert_eq!(
        role_of("sql", "select x from t", "from"),
        Some(Role::Keyword)
    );
    assert_eq!(role_of("sql", "x -- note", "-- note"), Some(Role::Comment));
}

#[test]
fn keys_preprocessor_and_variables() {
    assert_eq!(role_of("yaml", "name: fiber", "name"), Some(Role::Type));
    assert_eq!(role_of("toml", "edition = 1", "edition"), Some(Role::Type));
    assert_eq!(
        role_of("c", "#include <x.h>", "#include"),
        Some(Role::Keyword)
    );
    assert_eq!(
        runs("c", "a # b")[0],
        vec![(Role::CodeText, "a # b".to_owned())]
    );
    assert_eq!(role_of("bash", "echo $HOME", "$HOME"), Some(Role::Constant));
    assert_eq!(role_of("python", "class Foo", "Foo"), Some(Role::Type));
    assert_eq!(role_of("go", "var Foo", " Foo"), Some(Role::CodeText));
}

#[test]
fn diff_lines_take_roles_by_their_first_characters() {
    let lines = runs("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n same");
    let roles: Vec<Role> = lines.iter().map(|line| line[0].0).collect();
    assert_eq!(
        roles,
        vec![
            Role::Keyword,
            Role::Keyword,
            Role::Type,
            Role::Constant,
            Role::String,
            Role::CodeText
        ]
    );
}

#[test]
fn markdown_headings_fences_markers_and_inline_code() {
    let lines = runs("md", "# Title\n  - `x` y\n12. z\n```rust");
    assert_eq!(lines[0], vec![(Role::Keyword, "# Title".to_owned())]);
    assert_eq!(
        lines[1],
        vec![
            (Role::CodeText, "  ".to_owned()),
            (Role::Operator, "-".to_owned()),
            (Role::CodeText, " ".to_owned()),
            (Role::String, "`x`".to_owned()),
            (Role::CodeText, " y".to_owned()),
        ]
    );
    assert_eq!(lines[2][0], (Role::Operator, "12.".to_owned()));
    assert_eq!(lines[3], vec![(Role::Comment, "```rust".to_owned())]);
}

#[test]
fn html_tags_attributes_strings_entities_and_comments() {
    let lines = runs("html", "<a href=\"x\">t &amp; u</a><!-- c -->");
    assert_eq!(
        lines[0],
        vec![
            (Role::Operator, "<".to_owned()),
            (Role::Keyword, "a".to_owned()),
            (Role::CodeText, " ".to_owned()),
            (Role::Type, "href".to_owned()),
            (Role::Operator, "=".to_owned()),
            (Role::String, "\"x\"".to_owned()),
            (Role::Operator, ">".to_owned()),
            (Role::CodeText, "t ".to_owned()),
            (Role::Constant, "&amp;".to_owned()),
            (Role::CodeText, " u".to_owned()),
            (Role::Operator, "</".to_owned()),
            (Role::Keyword, "a".to_owned()),
            (Role::Operator, ">".to_owned()),
            (Role::Comment, "<!-- c -->".to_owned()),
        ]
    );
}

#[test]
fn more_heads_markers_and_edges() {
    let lines = runs("diff", "diff --git a b\nindex 1..2\n");
    assert_eq!(lines[0][0].0, Role::Keyword);
    assert_eq!(lines[1][0].0, Role::Keyword);
    let lines = runs("markdown", "* a\n+ b\n2) c\n~~~\n`open");
    assert_eq!(lines[0][0], (Role::Operator, "*".to_owned()));
    assert_eq!(lines[1][0], (Role::Operator, "+".to_owned()));
    assert_eq!(lines[2][0], (Role::Operator, "2)".to_owned()));
    assert_eq!(lines[3], vec![(Role::Comment, "~~~".to_owned())]);
    assert_eq!(lines[4], vec![(Role::String, "`open".to_owned())]);
    assert_eq!(
        runs("md", "2x y")[0],
        vec![(Role::CodeText, "2x y".to_owned())]
    );
}

#[test]
fn preprocessor_lines_start_after_a_newline_or_indent() {
    assert_eq!(
        role_of("c", "x;\n#define Y 1", "#define"),
        Some(Role::Keyword)
    );
    assert_eq!(role_of("c", "  #if X", "#if"), Some(Role::Keyword));
}

#[test]
fn a_sigil_variable_ends_at_its_word() {
    assert_eq!(role_of("bash", "$A b", "$A"), Some(Role::Constant));
}

#[test]
fn one_capital_is_a_type_and_underscores_with_digits_are_plain() {
    assert_eq!(role_of("rust", "X", "X"), Some(Role::Type));
    assert_eq!(
        runs("rust", "_1 x")[0],
        vec![(Role::CodeText, "_1 x".to_owned())]
    );
}

#[test]
fn a_rust_string_runs_across_lines() {
    let lines = runs("rust", "\"a\nb\" x");
    assert_eq!(lines[1][0], (Role::String, "b\"".to_owned()));
}

#[test]
fn html_self_closing_tags_bare_ampersands_and_unclosed_comments() {
    assert_eq!(role_of("html", "<br/>", "/>"), Some(Role::Operator));
    assert_eq!(role_of("html", "a & b", "&"), Some(Role::Constant));
    assert_eq!(
        runs("html", "<!-- open"),
        vec![vec![(Role::Comment, "<!-- open".to_owned())]]
    );
    assert_eq!(role_of("html", "<a b='c'>", "'c'"), Some(Role::String));
}
