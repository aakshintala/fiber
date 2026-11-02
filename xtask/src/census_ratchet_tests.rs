use super::*;

// Rule sources: ticket #3982's approval comment (the census pattern is the
// focused scan's fifth `rg` command, substring semantics with no word
// boundary) and the plan's Interfaces (`count_sites` counts occurrences of
// the six forms per line, leftmost non-overlapping, `\s` as
// `char::is_whitespace`). Every count asserted below pins one of the six
// forms against the census regex.

#[test]
fn kind_type_counts_with_varied_spacing() {
    let cases: &[(&str, usize)] = &[
        ("struct A { kind: String }\n", 1),
        ("struct A { kind:String }\n", 1),
        ("struct A { kind:  String }\n", 1),
        ("struct A { kind:   String }\n", 1),
        ("struct A { kind:\tString }\n", 1),
        ("struct A { kind:  String, other: u32 }\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}

#[test]
fn code_type_counts_string_and_str_with_varied_spacing() {
    let cases: &[(&str, usize)] = &[
        ("struct A { code: String }\n", 1),
        ("struct A { code:String }\n", 1),
        ("struct A { code:   String }\n", 1),
        ("struct A { code: &str }\n", 1),
        ("struct A { code:&str }\n", 1),
        ("struct A { code:   &str }\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}

#[test]
fn as_str_counts_for_each_name() {
    let cases: &[(&str, usize)] = &[
        ("let k = item.kind.as_str();\n", 1),
        ("let k = item.code.as_str();\n", 1),
        ("let k = item.view.as_str();\n", 1),
        ("let k = item.format.as_str();\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}

#[test]
fn compare_counts_equals_and_not_equals_for_each_name() {
    let cases: &[(&str, usize)] = &[
        ("if item.kind == \"open\" {\n", 1),
        ("if item.kind != \"open\" {\n", 1),
        ("if item.kind==\"open\" {\n", 1),
        ("if item.kind!=\"open\" {\n", 1),
        ("if item.kind  ==  \"open\" {\n", 1),
        ("if item.code == \"open\" {\n", 1),
        ("if item.code != \"open\" {\n", 1),
        ("if item.view == \"open\" {\n", 1),
        ("if item.view != \"open\" {\n", 1),
        ("if item.format == \"open\" {\n", 1),
        ("if item.format != \"open\" {\n", 1),
        ("if item.code  !=  \"open\" {\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}

#[test]
fn index_counts_for_each_name() {
    let cases: &[(&str, usize)] = &[
        ("let k = map[\"kind\"];\n", 1),
        ("let k = map[\"code\"];\n", 1),
        ("let k = map[\"view\"];\n", 1),
        ("let k = map[\"format\"];\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}

#[test]
fn get_counts_for_each_name() {
    let cases: &[(&str, usize)] = &[
        ("let k = map.get(\"kind\");\n", 1),
        ("let k = map.get(\"code\");\n", 1),
        ("let k = map.get(\"view\");\n", 1),
        ("let k = map.get(\"format\");\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}

#[test]
fn two_forms_on_one_line_count_two() {
    let cases: &[(&str, usize)] = &[
        ("fn f(kind: String, code: String) {}\n", 2),
        ("if item.kind == \"a\" && item.code == \"b\" {\n", 2),
        ("let k = map.get(\"kind\"); let c = map.get(\"code\");\n", 2),
        ("fn f(kind: String) {}\nfn g(code: &str) {}\n", 2),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}

#[test]
fn names_match_inside_longer_words() {
    // The census pattern has no word boundary: `error_kind: String` and
    // `exit_code: String` hold the `kind:` / `code:` form.
    let cases: &[(&str, usize)] = &[
        ("struct A { error_kind: String }\n", 1),
        ("struct A { exit_code: String }\n", 1),
        ("struct A { exit_code: &str }\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}

#[test]
fn near_misses_count_zero() {
    let cases: &[&str] = &[
        "if item.kind_of == \"open\" {\n",
        "if item.formatted == \"open\" {\n",
        "if item.kind = \"open\" {\n",
        "struct A { kind: &str }\n",
        "fn f(code: &'a str) {}\n",
        "struct A { code: Option<String> }\n",
        "let k = item.kind.as_str;\n",
        "let k = map.get(\"other\");\n",
        "let k = map[\"other\"];\n",
        "fn on_event(name: &str) {\n    match name {\n        \"kind\" => {}\n        _ => {}\n    }\n}\n",
    ];
    for source in cases {
        assert_eq!(count_sites(source), 0, "source: {source:?}");
    }
}

#[test]
fn a_site_in_a_string_or_a_comment_counts() {
    let cases: &[(&str, usize)] = &[
        ("let marker = \"kind: String\";\n", 1),
        ("// struct A { kind: String }\n", 1),
        ("/* code: &str */\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_sites(source), *expected, "source: {source:?}");
    }
}
