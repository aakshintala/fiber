use std::collections::BTreeMap;

use super::check_diff;

fn sources(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(path, source)| ((*path).to_owned(), (*source).to_owned()))
        .collect()
}

fn added(path: &str, hunk: &str, source: &str) -> Result<Vec<String>, String> {
    check_diff(
        &format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{hunk}"),
        &sources(&[(path, source)]),
    )
}

#[test]
fn rejects_each_ruled_phrase_and_mixed_case() {
    for line in [
        "// changed from old",
        "// Previously this path",
        "// now uses the cache",
        "// fixed the bug where it broke",
        "// FiXeD the BUG where it broke",
    ] {
        let text = format!("{line}\n");
        assert_eq!(
            added(
                "src/lib.rs",
                "@@ -0,0 +1 @@\n+PLACEHOLDER\n"
                    .replace("PLACEHOLDER", line)
                    .as_str(),
                &text
            ),
            Ok(vec!["src/lib.rs:1: comment narrates history".to_owned()]),
            "{line}"
        );
    }
}

#[test]
fn phrase_word_edges_exclude_substrings_and_allow_punctuation() {
    for line in [
        "// unchanged from baseline",
        "// know uses",
        "// previously_named",
        "// xpreviously",
        "// used to",
        "// was changed",
        "// replaced, instead of",
    ] {
        assert!(
            added(
                "src/lib.rs",
                &format!("@@ -0,0 +1 @@\n+{line}\n"),
                &format!("{line}\n")
            )
            .unwrap()
            .is_empty(),
            "{line}"
        );
    }
    for line in ["// (previously)", "// previously!", "// !previously "] {
        assert_eq!(
            added(
                "src/lib.rs",
                &format!("@@ -0,0 +1 @@\n+{line}\n"),
                &format!("{line}\n")
            )
            .unwrap()
            .len(),
            1,
            "{line}"
        );
    }
}

#[test]
fn phrase_spacing_is_spaces_or_tabs_but_never_newlines() {
    for line in [
        "// now  uses",
        "// now\tuses",
        "// fixed the\tbug where",
        "// fixed the \tbug where",
    ] {
        assert_eq!(
            added(
                "src/lib.rs",
                &format!("@@ -0,0 +1 @@\n+{line}\n"),
                &format!("{line}\n")
            )
            .unwrap()
            .len(),
            1,
            "{line}"
        );
    }
    let source = "// now\n// uses\n";
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -0,0 +1,2 @@\n+// now\n+// uses\n";
    assert!(
        check_diff(diff, &sources(&[("src/lib.rs", source)]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn scans_only_added_comment_text_and_reports_each_line_once() {
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n // previously old\n-fn f() {}\n+fn f() {} // now uses cache; previously was old\n";
    assert_eq!(
        check_diff(
            diff,
            &sources(&[(
                "src/lib.rs",
                "// previously old\nfn f() {} // now uses cache; previously was old\n"
            )])
        )
        .unwrap(),
        ["src/lib.rs:2: comment narrates history"]
    );
    for line in [
        r#"let s = "// now uses cache";"#,
        r#"let s = "escaped \" // now uses cache";"#,
    ] {
        assert!(
            added(
                "src/lib.rs",
                &format!("@@ -0,0 +1 @@\n+{line}\n"),
                &format!("{line}\n")
            )
            .unwrap()
            .is_empty(),
            "{line}"
        );
    }
}

#[test]
fn recognizes_rust_line_doc_block_and_shell_comments() {
    for (path, line, source) in [
        (
            "src/lib.rs",
            "/// previously documented",
            "/// previously documented\n",
        ),
        (
            "src/lib.rs",
            "/* previously documented */",
            "/* previously documented */\n",
        ),
        (
            "scripts/run.sh",
            "echo ok # now uses cache",
            "echo ok # now uses cache\n",
        ),
    ] {
        assert_eq!(
            added(path, &format!("@@ -0,0 +1 @@\n+{line}\n"), source)
                .unwrap()
                .len(),
            1,
            "{line}"
        );
    }
    for (path, line) in [
        ("src/lib.rs", "let s = r##\"// now uses cache\"##;"),
        ("src/lib.rs", r"let c = '\u{7f}'; // ordinary note"),
        ("src/lib.rs", "/* now */ let x = 1; /* uses */"),
        ("scripts/run.sh", "printf '%s' '# previously'"),
    ] {
        let source = format!("{line}\n");
        assert!(
            added(path, &format!("@@ -0,0 +1 @@\n+{line}\n"), &source)
                .unwrap()
                .is_empty(),
            "{line}"
        );
    }
    let line = r"let c = '\u{7f}'; // now uses cache";
    assert_eq!(
        added(
            "src/lib.rs",
            &format!("@@ -0,0 +1 @@\n+{line}\n"),
            &format!("{line}\n")
        )
        .unwrap()
        .len(),
        1
    );
    let nested = "/* outer /* nested */ 🧵 previously */\n";
    assert_eq!(
        added(
            "src/lib.rs",
            "@@ -0,0 +1 @@\n+/* outer /* nested */ 🧵 previously */\n",
            nested
        )
        .unwrap()
        .len(),
        1
    );
    let raw = "let s = r##\"\nquote: \" // previously\ntext\"##;\n";
    let raw_diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -0,0 +2 @@\n+quote: \" // previously\n";
    assert!(
        check_diff(raw_diff, &sources(&[("src/lib.rs", raw)]))
            .unwrap()
            .is_empty()
    );
    let source = "cat <<'EOF'\n# now uses cache\nEOF\n";
    let diff = "diff --git a/scripts/check b/scripts/check\n--- a/scripts/check\n+++ b/scripts/check\n@@ -0,0 +1,3 @@\n+cat <<'EOF'\n+# now uses cache\n+EOF\n";
    assert!(
        check_diff(diff, &sources(&[("scripts/check", source)]))
            .unwrap()
            .is_empty()
    );
    let source = "cat <<-A <<B\n# now uses cache\n\tA\n# previously\nB\n";
    let diff = "diff --git a/scripts/check b/scripts/check\n--- a/scripts/check\n+++ b/scripts/check\n@@ -0,0 +1,5 @@\n+cat <<-A <<B\n+# now uses cache\n+\tA\n+# previously\n+B\n";
    assert!(
        check_diff(diff, &sources(&[("scripts/check", source)]))
            .unwrap()
            .is_empty()
    );
    let line = "cat <<< '# now uses cache' # previously";
    assert_eq!(
        added(
            "scripts/check",
            &format!("@@ -0,0 +1 @@\n+{line}\n"),
            &format!("{line}\n")
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn patch_line_accounting_handles_context_deletions_and_multiple_hunks() {
    let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n // previously old\n-old();\n+new();\n@@ -2,0 +3 @@\n+// now uses cache\ndiff --git a/src/b.rs b/src/b.rs\n--- /dev/null\n+++ b/src/b.rs\n@@ -0,0 +1 @@\n+// changed from old\n";
    assert_eq!(
        check_diff(
            diff,
            &sources(&[
                ("src/a.rs", "// previously old\nnew();\n// now uses cache\n"),
                ("src/b.rs", "// changed from old\n")
            ])
        )
        .unwrap()
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>(),
        [
            "src/a.rs:3: comment narrates history".to_owned(),
            "src/b.rs:1: comment narrates history".to_owned()
        ]
        .into_iter()
        .collect()
    );
    let header_like = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -0,0 +1,2 @@\n++++ /dev/null\n+// previously\n";
    assert_eq!(
        check_diff(
            header_like,
            &sources(&[("src/a.rs", "+++ /dev/null\n// previously\n")])
        )
        .unwrap(),
        ["src/a.rs:2: comment narrates history"]
    );
}

#[test]
fn zero_additions_deletions_no_newline_markers_and_raw_strings_are_clean() {
    let source = "let text = r#\"\n// previously\n\"#;\n";
    let raw_line = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,0 +2 @@\n+// previously\n";
    assert!(
        check_diff(raw_line, &sources(&[("src/a.rs", source)]))
            .unwrap()
            .is_empty()
    );
    let diff = "diff --git a/src/deleted.rs b/src/deleted.rs\n--- a/src/deleted.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-// previously\n\\ No newline at end of file\ndiff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -0,0 +1,2 @@\n+// clean\n+// ordinary\n\\ No newline at end of file\ndiff --git a/src/zero.rs b/src/zero.rs\n--- a/src/zero.rs\n+++ b/src/zero.rs\n@@ -1 +0,0 @@\n-// previously\n";
    assert!(
        check_diff(diff, &sources(&[("src/a.rs", "// clean\n// ordinary")]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ignores_non_source_paths_and_checks_hunk_structure_and_ranges() {
    let diff = "diff --git a/notes.txt b/notes.txt\n--- a/notes.txt\n+++ b/notes.txt\n@@ -0,0 +1 @@\n+previously\n";
    assert!(check_diff(diff, &BTreeMap::new()).unwrap().is_empty());

    let zero_new_side = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +0,0 @@\n-// previously\n";
    assert!(
        check_diff(zero_new_side, &sources(&[("src/a.rs", "")]))
            .unwrap()
            .is_empty()
    );
    let malformed_range = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -0,0 +0,1 @@\n+// previously\n";
    assert!(
        check_diff(
            malformed_range,
            &sources(&[("src/a.rs", "// previously\n")])
        )
        .is_err()
    );

    let shifted = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,0 +2 @@\n+// previously\n";
    assert_eq!(
        check_diff(
            shifted,
            &sources(&[("src/a.rs", "ordinary\n// previously\n")])
        )
        .unwrap(),
        ["src/a.rs:2: comment narrates history"]
    );
}

#[test]
fn shell_lexing_handles_hash_quotes_escapes_and_heredoc_terminators() {
    for (line, source) in [
        ("echo x#previously", "echo x#previously\n"),
        (r##"echo "# previously""##, "echo \"# previously\"\n"),
        (
            r##"echo "escaped \\" # previously""##,
            "echo \"escaped \\\\ # previously\"\n",
        ),
        ("echo ok # previously", "echo ok # previously\n"),
    ] {
        let result = added(
            "scripts/check",
            &format!("@@ -0,0 +1 @@\n+{line}\n"),
            source,
        )
        .unwrap();
        if line == "echo ok # previously" {
            assert_eq!(result, ["scripts/check:1: comment narrates history"]);
        } else {
            assert!(result.is_empty(), "{line}: {result:?}");
        }
    }

    let source = "cat <<'END' <<-TWO\n# data\nEND\n# more data\n\tTWO\n# previously real comment\n";
    let diff = "diff --git a/scripts/check b/scripts/check\n--- a/scripts/check\n+++ b/scripts/check\n@@ -0,0 +6 @@\n+# previously real comment\n";
    assert_eq!(
        check_diff(diff, &sources(&[("scripts/check", source)])).unwrap(),
        ["scripts/check:6: comment narrates history"]
    );

    let invalid = "cat <<\n# previously real comment\n";
    assert_eq!(
        added(
            "scripts/check",
            "@@ -0,0 +2 @@\n+# previously real comment\n",
            invalid
        )
        .unwrap(),
        ["scripts/check:2: comment narrates history"]
    );
}

#[test]
fn multiline_nested_block_comment_closes_before_trailing_comment() {
    let source = "/* outer\n /* nested */\nstill comment */ let x = 1; // now uses cache\n";
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -0,0 +3 @@\n+still comment */ let x = 1; // now uses cache\n";
    assert_eq!(
        check_diff(diff, &sources(&[("src/lib.rs", source)])).unwrap(),
        ["src/lib.rs:3: comment narrates history"]
    );
}

#[test]
fn malformed_patch_is_an_error_not_a_clean_result() {
    let bad = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n+// now uses cache\n";
    assert!(check_diff(bad, &sources(&[("src/a.rs", "// now uses cache\n")])).is_err());
    let old_count_mismatch = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n+// clean\n@@ -2,0 +2 @@\n+// ordinary\n";
    assert!(
        check_diff(
            old_count_mismatch,
            &sources(&[("src/a.rs", "// clean\n// ordinary\n")])
        )
        .is_err()
    );
    let new_count_mismatch = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n+// clean\n";
    assert!(check_diff(new_count_mismatch, &sources(&[("src/a.rs", "// clean\n")])).is_err());
    let missing_source = "diff --git a/src/missing.rs b/src/missing.rs\n--- a/src/missing.rs\n+++ b/src/missing.rs\n@@ -0,0 +1 @@\n+// previously\n";
    assert!(
        check_diff(missing_source, &BTreeMap::new())
            .unwrap_err()
            .contains("src/missing.rs: source unavailable")
    );
}
