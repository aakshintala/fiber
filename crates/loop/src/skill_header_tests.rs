use super::{Header, Invalid, parse};

/// A header with `body` between the fences.
fn header(body: &str) -> String {
    format!("---\n{body}\n---\nThe skill's text.\n")
}

fn description(body: &str) -> String {
    let text = header(&format!("name: n\n{body}"));
    parse(&text).unwrap().description
}

fn invalid(text: &str) -> Invalid {
    parse(text).unwrap_err()
}

#[test]
fn a_plain_single_line_value() {
    let parsed = parse(&header("name: review\ndescription: Reviews a diff.")).unwrap();
    assert_eq!(
        parsed,
        Header {
            name: "review".into(),
            description: "Reviews a diff.".into(),
            model_invocable: true,
        }
    );
}

#[test]
fn a_plain_value_keeps_colons_inside() {
    assert_eq!(
        description("description: Use when: the user asks"),
        "Use when: the user asks"
    );
}

#[test]
fn a_plain_value_continues_on_indented_lines() {
    assert_eq!(
        description("description: one\n  two\n   three"),
        "one two three"
    );
}

#[test]
fn a_plain_value_may_start_on_the_next_line() {
    assert_eq!(
        description("description:\n  first\n  second"),
        "first second"
    );
}

#[test]
fn a_blank_continuation_line_is_a_line_break() {
    assert_eq!(description("description: one\n\n  two"), "one\ntwo");
}

#[test]
fn a_plain_value_loses_its_comment() {
    assert_eq!(description("description: Reviews # not this"), "Reviews");
    assert_eq!(description("description: C# code"), "C# code");
    assert_eq!(
        invalid(&header("name: n\ndescription: # only a comment")),
        Invalid::NoDescription
    );
}

#[test]
fn a_comment_line_inside_a_plain_value_is_skipped() {
    assert_eq!(description("description: a\n  # skipped\n  b"), "a b");
}

#[test]
fn a_single_quoted_value() {
    assert_eq!(description("description: 'it''s: fine'"), "it's: fine");
    assert_eq!(description("description: 'a' # note"), "a");
}

#[test]
fn a_double_quoted_value_with_each_escape() {
    assert_eq!(
        description(r#"description: "a\\b \"c\" d\ne\tf: g""#),
        "a\\b \"c\" d\ne\tf: g"
    );
}

#[test]
fn an_unknown_escape_is_kept_as_written() {
    assert_eq!(description(r#"description: "a\db""#), "a\\db");
}

#[test]
fn an_unclosed_quote_does_not_parse() {
    assert_eq!(
        invalid(&header("name: n\ndescription: 'open")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: n\ndescription: \"open")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: n\ndescription: \"ends with a slash\\")),
        Invalid::DoesNotParse
    );
}

#[test]
fn text_after_a_closing_quote_does_not_parse() {
    assert_eq!(
        invalid(&header("name: n\ndescription: 'a' b")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: n\ndescription: \"a\" b")),
        Invalid::DoesNotParse
    );
}

#[test]
fn a_literal_block_keeps_its_line_breaks() {
    assert_eq!(
        description("description: |\n    one\n      two\n\n    three"),
        "one\n  two\n\nthree"
    );
}

#[test]
fn a_literal_block_with_chomping_marks() {
    assert_eq!(description("description: |-\n  one\n  two"), "one\ntwo");
    assert_eq!(description("description: |+\n  one\n  two\n\n"), "one\ntwo");
}

#[test]
fn a_folded_block_joins_lines_with_a_space() {
    assert_eq!(
        description("description: >\n  one\n  two\n\n  three"),
        "one two\nthree"
    );
}

#[test]
fn a_folded_block_with_chomping_marks_and_a_comment() {
    assert_eq!(description("description: >+\n  one\n  two\n\n"), "one two");
    assert_eq!(
        description("description: >- # why\n  one\n  two"),
        "one two"
    );
}

#[test]
fn a_block_keeps_hash_lines() {
    assert_eq!(
        description("description: |\n  # not a comment\n  x"),
        "# not a comment\nx"
    );
}

#[test]
fn a_gt_inside_a_value_is_plain_text() {
    assert_eq!(description("description: > not a block"), "> not a block");
    assert_eq!(description("description: |pipe"), "|pipe");
}

#[test]
fn no_opening_line_does_not_parse() {
    assert_eq!(
        invalid("name: n\ndescription: d\n---\n"),
        Invalid::DoesNotParse
    );
    assert_eq!(invalid(""), Invalid::DoesNotParse);
}

#[test]
fn no_closing_line_does_not_parse() {
    assert_eq!(
        invalid("---\nname: n\ndescription: d\n"),
        Invalid::DoesNotParse
    );
}

#[test]
fn the_fences_must_be_exact() {
    assert_eq!(
        invalid("\u{feff}---\nname: n\ndescription: d\n---\n"),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid("--- \nname: n\ndescription: d\n---\n"),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid("---\nname: n\ndescription: d\n--- \n"),
        Invalid::DoesNotParse
    );
}

#[test]
fn a_column_zero_line_that_is_not_a_key_does_not_parse() {
    assert_eq!(
        invalid(&header("name: n\ndescription: d\nstray words")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: n\ndescription: d\n- item")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: n\ndescription: d\nbad key: x")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: n\ndescription: d\nkey:glued")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("  indented: first\nname: n\ndescription: d")),
        Invalid::DoesNotParse
    );
}

#[test]
fn a_repeated_key_does_not_parse() {
    assert_eq!(
        invalid(&header("name: a\nname: b\ndescription: d")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: a\ndescription: d\nmetadata:\nmetadata:")),
        Invalid::DoesNotParse
    );
}

#[test]
fn a_name_or_description_that_is_a_map_or_list_does_not_parse() {
    assert_eq!(
        invalid(&header("name:\n  first: a\ndescription: d")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: n\ndescription:\n  - a\n  - b")),
        Invalid::DoesNotParse
    );
    assert_eq!(
        invalid(&header("name: n\ndescription:\n  -\n  - b")),
        Invalid::DoesNotParse
    );
}

#[test]
fn a_missing_or_blank_name_or_description() {
    assert_eq!(invalid(&header("description: d")), Invalid::NoName);
    assert_eq!(invalid(&header("name:\ndescription: d")), Invalid::NoName);
    assert_eq!(
        invalid(&header("name: '  '\ndescription: d")),
        Invalid::NoName
    );
    assert_eq!(invalid(&header("name: n")), Invalid::NoDescription);
    assert_eq!(
        invalid(&header("name: n\ndescription: \"  \"")),
        Invalid::NoDescription
    );
    assert_eq!(invalid(&header("")), Invalid::NoName);
    assert_eq!(
        invalid(&header("description: '  '\nname: ' a '")),
        Invalid::NoDescription
    );
}

#[test]
fn a_name_is_trimmed_and_a_one_letter_name_counts() {
    let parsed = parse(&header("name: \"  a  \"\ndescription: d")).unwrap();
    assert_eq!(parsed.name, "a");
}

#[test]
fn other_fields_are_ignored_whatever_their_shape() {
    let text = header(
        "name: n\n\
         metadata:\n  author: me\n  tags:\n    - x\n    - y\n\
         description: d\n\
         allowed-tools:\n  - Read\n  - Bash\n\
         license: MIT\n\
         compatibility: 'unclosed\n\
         argument-hint: [file]\n\
         metadata2: |\n  text\n\
         # a comment\n\
         extra: >\n  folded",
    );
    let parsed = parse(&text).unwrap();
    assert_eq!(parsed.name, "n");
    assert_eq!(parsed.description, "d");
}

#[test]
fn disable_model_invocation_reads_true_in_any_case() {
    let flag = |value: &str| {
        parse(&header(&format!(
            "name: n\ndescription: d\ndisable-model-invocation: {value}"
        )))
        .unwrap()
        .model_invocable
    };
    assert!(!flag("true"));
    assert!(!flag("True"));
    assert!(!flag("TRUE"));
    assert!(!flag("'true'"));
    assert!(flag("false"));
    assert!(flag("yes"));
    assert!(flag("truely"));
    assert!(
        parse(&header("name: n\ndescription: d"))
            .unwrap()
            .model_invocable
    );
}

#[test]
fn crlf_line_endings_parse() {
    let text = "---\r\nname: n\r\ndescription: >\r\n  one\r\n  two\r\n---\r\nBody\r\n";
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.name, "n");
    assert_eq!(parsed.description, "one two");
}

#[test]
fn a_body_holding_the_fence_again_is_not_read() {
    let text = "---\nname: n\ndescription: d\n---\nBody\n---\nname: other\n---\n";
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.name, "n");
}

#[test]
fn a_real_world_folded_description_with_metadata_between_fields() {
    let text = header(
        "name: grill\nmetadata:\n  version: \"1\"\ndescription: >\n  Grill the user.\n  Use when: asked.\nallowed-tools: Read",
    );
    let parsed = parse(&text).unwrap();
    assert_eq!(parsed.description, "Grill the user. Use when: asked.");
}

#[test]
fn an_indented_comment_before_the_first_key_is_skipped() {
    let parsed = parse("---\n  # note\nname: n\ndescription: d\n---\n").unwrap();
    assert_eq!(parsed.name, "n");
}

#[test]
fn an_indented_line_before_the_first_key_does_not_parse() {
    assert_eq!(
        invalid("---\n  stray\nname: n\ndescription: d\n---\n"),
        Invalid::DoesNotParse
    );
}

#[test]
fn a_literal_block_keeps_trailing_spaces_inside_the_description() {
    assert_eq!(
        description("description: |\n  first  \n  second"),
        "first  \nsecond"
    );
}

#[test]
fn a_whitespace_only_line_in_a_folded_block_is_a_line_break() {
    assert_eq!(description("description: >\n  a\n     \n  b"), "a\nb");
}
