use super::*;

#[test]
fn accepts_nightly_release_tags() {
    assert!(is_nightly_tag("v0.0.1-nightly.20261104"));
    assert!(is_nightly_tag("v10.20.30-nightly.19991231"));
}

#[test]
fn rejects_a_bare_version() {
    // A version is bare, a tag carries `v`: the bare form is not a tag.
    assert!(!is_nightly_tag("0.0.1-nightly.20261104"));
}

#[test]
fn rejects_stable_tags() {
    assert!(!is_nightly_tag("v0.0.1"));
    assert!(!is_nightly_tag("v1.2.3"));
}

#[test]
fn rejects_malformed_tags() {
    assert!(!is_nightly_tag(""));
    assert!(!is_nightly_tag("v0.0.1-nightly.2026110"));
    assert!(!is_nightly_tag("v0.0.1-nightly.202611044"));
    assert!(!is_nightly_tag("v0.0.1-nightly.2026110a"));
    assert!(!is_nightly_tag("v0.1-nightly.20261104"));
    assert!(!is_nightly_tag("v0.0.1.2-nightly.20261104"));
    assert!(!is_nightly_tag("v0..1-nightly.20261104"));
    assert!(!is_nightly_tag("x0.0.1-nightly.20261104"));
    assert!(!is_nightly_tag("v0.0.1-nightly.20261104-rc1"));
    assert!(!is_nightly_tag("v0.0.1-nightly."));
    assert!(!is_nightly_tag("v0.0.1-nightly.20261104\n"));
}

#[test]
fn rejects_a_signed_base_part() {
    // `"+0".parse::<u64>()` succeeds, so the digit check (not the parse)
    // is what refuses the plus sign.
    assert!(!is_nightly_tag("v+0.0.1-nightly.20261104"));
}

#[test]
fn tag_for_version_adds_one_v() {
    assert_eq!(
        tag_for_version("0.0.1-nightly.20261104"),
        "v0.0.1-nightly.20261104"
    );
}

#[test]
fn is_nightly_mode_takes_the_bare_form() {
    assert_eq!(
        run(&[
            "--is-nightly".to_owned(),
            "0.0.1-nightly.20261104".to_owned()
        ]),
        Ok(true)
    );
    assert_eq!(
        run(&["--is-nightly".to_owned(), "0.0.1".to_owned()]),
        Ok(false)
    );
}

#[test]
fn is_nightly_mode_normalizes_once_inside() {
    // Wiring passes no `v`: a tag form here would double the prefix and
    // must not match.
    assert_eq!(
        run(&[
            "--is-nightly".to_owned(),
            "v0.0.1-nightly.20261104".to_owned()
        ]),
        Ok(false)
    );
}

#[test]
fn run_rejects_bad_arity_and_flags() {
    assert!(run(&[]).is_err());
    assert!(run(&["--is-nightly".to_owned()]).is_err());
    assert!(run(&["--nope".to_owned(), "0.0.1-nightly.20261104".to_owned()]).is_err());
    assert!(
        run(&[
            "--is-nightly".to_owned(),
            "0.0.1-nightly.20261104".to_owned(),
            "extra".to_owned()
        ])
        .is_err()
    );
}

#[test]
fn prev_excluding_returns_the_max_besides_the_exclusion() {
    let tags = [
        "v0.0.1-nightly.20261102",
        "v0.0.1-nightly.20261104",
        "v0.0.1-nightly.20261103",
    ];
    assert_eq!(
        prev_excluding(&tags, "v0.0.1-nightly.20261104"),
        Some("v0.0.1-nightly.20261103".to_owned())
    );
}

#[test]
fn prev_excluding_skips_non_nightly_entries() {
    let tags = [
        "v0.0.1",
        "0.0.1-nightly.20261105",
        "v0.0.1-nightly.2026110",
        "v0.0.1-nightly.20261103",
    ];
    assert_eq!(
        prev_excluding(&tags, "v0.0.1-nightly.20261104"),
        Some("v0.0.1-nightly.20261103".to_owned())
    );
}

#[test]
fn prev_excluding_orders_bases_numerically() {
    // A lexical max would keep v0.9.0 (`9` sorts past `1`); the numeric
    // max crosses the base.
    let tags = ["v0.9.0-nightly.20261231", "v0.10.0-nightly.20260101"];
    assert_eq!(
        prev_excluding(&tags, "v0.10.0-nightly.20260102"),
        Some("v0.10.0-nightly.20260101".to_owned())
    );
}

#[test]
fn prev_excluding_exclusion_is_exact_on_the_tag_form() {
    let tags = ["v0.0.1-nightly.20261103", "v0.0.1-nightly.20261104"];
    // An already-tagged exclusion doubles the prefix inside and matches
    // nothing, so the max survives.
    assert_eq!(
        prev_excluding(&tags, "vv0.0.1-nightly.20261104"),
        Some("v0.0.1-nightly.20261104".to_owned())
    );
}

#[test]
fn prev_excluding_reports_nothing_left() {
    assert_eq!(prev_excluding(&[], "v0.0.1-nightly.20261104"), None);
    assert_eq!(
        prev_excluding(&["v0.0.1-nightly.20261104"], "v0.0.1-nightly.20261104"),
        None
    );
    assert_eq!(
        prev_excluding(&["v0.0.1", "bare"], "v0.0.1-nightly.20261104"),
        None
    );
}

#[test]
fn prev_excluding_command_reads_a_tag_listing() {
    let input = "v0.0.1-nightly.20261102\nv0.0.1-nightly.20261104\nv0.0.1-nightly.20261103\n";
    assert_eq!(
        prev_excluding_command("0.0.1-nightly.20261104", input),
        Ok("v0.0.1-nightly.20261103".to_owned())
    );
}

#[test]
fn prev_excluding_command_errors_naming_the_exclusion() {
    let error = prev_excluding_command("0.0.1-nightly.20261104", "").unwrap_err();
    assert!(error.contains("v0.0.1-nightly.20261104"), "{error}");
    let error =
        prev_excluding_command("0.0.1-nightly.20261104", "v0.0.1-nightly.20261104\n").unwrap_err();
    assert!(error.contains("v0.0.1-nightly.20261104"), "{error}");
}

#[test]
fn parse_fiber_version_strips_prefix_and_commit() {
    assert_eq!(
        parse_fiber_version("fiber 0.0.1-nightly.20261104 (abc1234)"),
        Ok("0.0.1-nightly.20261104".to_owned())
    );
    assert_eq!(
        parse_fiber_version("fiber 0.0.1-nightly.20261104 (abc1234)\n"),
        Ok("0.0.1-nightly.20261104".to_owned())
    );
}

#[test]
fn parse_fiber_version_keeps_a_bare_line_without_commit() {
    assert_eq!(
        parse_fiber_version("fiber 0.0.1-nightly.20261104"),
        Ok("0.0.1-nightly.20261104".to_owned())
    );
}

#[test]
fn parse_fiber_version_rejects_anything_without_the_prefix() {
    let error = parse_fiber_version("0.0.1-nightly.20261104").unwrap_err();
    assert!(error.contains("0.0.1-nightly.20261104"), "{error}");
    let error = parse_fiber_version("").unwrap_err();
    assert!(error.contains("expected"), "{error}");
    let error = parse_fiber_version("fiber ").unwrap_err();
    assert!(error.contains("expected"), "{error}");
}

#[test]
fn parse_version_mode_wires_the_line_through() {
    assert_eq!(
        run(&[
            "--parse-version".to_owned(),
            "fiber 0.0.1-nightly.20261104 (abc1234)".to_owned()
        ]),
        Ok(true)
    );
    assert!(
        run(&[
            "--parse-version".to_owned(),
            "0.0.1-nightly.20261104".to_owned()
        ])
        .is_err()
    );
}

#[test]
fn prev_excluding_mode_reads_supplied_lines() {
    use std::io::Cursor;
    assert_eq!(
        run_with(
            &[
                "--prev-excluding".to_owned(),
                "0.0.1-nightly.20261104".to_owned()
            ],
            &mut Cursor::new(
                "v0.0.1-nightly.20261102\nv0.0.1-nightly.20261104\nv0.0.1-nightly.20261103\n"
            ),
        ),
        Ok(true)
    );
    assert!(
        run_with(
            &[
                "--prev-excluding".to_owned(),
                "0.0.1-nightly.20261104".to_owned()
            ],
            &mut Cursor::new(""),
        )
        .is_err()
    );
}

#[test]
fn check_extensions_mode_reads_supplied_lines() {
    use std::io::Cursor;
    assert_eq!(
        run_with(
            &[
                "--check-extensions".to_owned(),
                "0.0.1-nightly.20261104".to_owned()
            ],
            &mut Cursor::new("fx 0.0.1-nightly.20261104 abc1234\n"),
        ),
        Ok(true)
    );
    assert!(
        run_with(
            &[
                "--check-extensions".to_owned(),
                "0.0.1-nightly.20261104".to_owned()
            ],
            &mut Cursor::new("fx 0.0.1-nightly.20261103 abc1234\n"),
        )
        .is_err()
    );
}

#[test]
fn count_nightly_mode_reads_supplied_lines() {
    use std::io::Cursor;
    assert_eq!(
        run_with(
            &["--count-nightly".to_owned()],
            &mut Cursor::new("refs/tags/v0.0.1-nightly.20261104\nrefs/tags/v0.0.1\n"),
        ),
        Ok(true)
    );
    assert!(
        run_with(
            &["--count-nightly".to_owned(), "extra".to_owned()],
            &mut Cursor::new(""),
        )
        .is_err()
    );
}

#[test]
fn run_rejects_usage_with_the_usage_line() {
    let error = run(&[]).unwrap_err();
    assert!(error.contains("cargo xtask nightly-pair"), "{error}");
}

#[test]
fn check_extension_list_accepts_matching_second_fields() {
    let list = "fx 0.0.1-nightly.20261104 abc1234\n\
        other 0.0.1-nightly.20261104 repository proj path hash\n\
        \n\
        \n";
    assert_eq!(check_extension_list(list, "0.0.1-nightly.20261104"), Ok(()));
}

#[test]
fn check_extension_list_rejects_a_mismatched_version_naming_the_line() {
    let list = "fx 0.0.1-nightly.20261104 abc1234\nother 0.0.1-nightly.20261103 abc1234\n";
    let error = check_extension_list(list, "0.0.1-nightly.20261104").unwrap_err();
    assert!(error.contains("other 0.0.1-nightly.20261103"), "{error}");
}

#[test]
fn check_extension_list_rejects_a_short_line() {
    let error = check_extension_list("fx\n", "0.0.1-nightly.20261104").unwrap_err();
    assert!(error.contains("fx"), "{error}");
}

#[test]
fn count_nightly_refs_counts_only_strict_nightly_refs() {
    let input = "refs/tags/v0.0.1-nightly.20261104\n\
        refs/tags/v0.0.1\n\
        refs/tags/v0.0.1-nightly.2026110\n\
        refs/tags/v0.0.1-nightly.20261104^{}\n\
        \n";
    assert_eq!(count_nightly_refs(input), 1);
    assert_eq!(count_nightly_refs(""), 0);
    assert_eq!(
        count_nightly_refs(
            "refs/tags/v0.0.1-nightly.20261103\nrefs/tags/v0.0.1-nightly.20261104\n"
        ),
        2
    );
}
