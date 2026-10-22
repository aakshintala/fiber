use super::*;

#[test]
fn stable_parses_three_plain_components() {
    assert_eq!(parse_stable("v0.1.10"), Some([0, 1, 10]));
    assert_eq!(parse_stable("v0.1.0"), Some([0, 1, 0]));
}

#[test]
fn stable_rejects_suffixes_names_and_bad_numbers() {
    assert_eq!(parse_stable("v0.1.0-rc1"), None);
    assert_eq!(parse_stable("v0.1.0-nightly.20261023"), None);
    assert_eq!(parse_stable("docs-x"), None);
    assert_eq!(parse_stable("v01.2.3"), None);
    assert_eq!(parse_stable("v99999999999999999999.0.0"), None);
    assert_eq!(parse_stable("v0.1"), None);
    assert_eq!(parse_stable("v0.1.0.0"), None);
}

#[test]
fn nightly_parses_an_eight_digit_date() {
    assert_eq!(
        parse_nightly("v0.4.1-nightly.20261101"),
        Some(([0, 4, 1], 20261101))
    );
    assert_eq!(parse_nightly("v0.4.1-nightly.2026110"), None);
    assert_eq!(parse_nightly("v0.4.1-nightly.202611011"), None);
    assert_eq!(parse_nightly("v01.4.1-nightly.20261101"), None);
    assert_eq!(parse_nightly("v0.4.1"), None);
}

#[test]
fn a_numeric_tie_breaks_by_tag_name() {
    assert!(beats::<[u64; 3]>(Some((&[0, 1, 0], "a")), &[0, 1, 0], "b"));
    assert!(!beats::<[u64; 3]>(Some((&[0, 1, 0], "b")), &[0, 1, 0], "a"));
    assert!(!beats::<[u64; 3]>(Some((&[0, 1, 0], "a")), &[0, 1, 0], "a"));
}

#[test]
fn the_first_tag_has_nothing_to_beat() {
    assert!(beats::<[u64; 3]>(None, &[0, 1, 0], "v0.1.0"));
}

#[test]
fn select_is_empty_without_release_tags() {
    assert_eq!(select("docs-x\nv0.1.0-rc1\n"), (None, None));
}

#[test]
fn rewrite_leaves_external_absolute_and_anchor_only_links_alone() {
    assert_eq!(
        rewrite("docs/user", "https://example.com/x.md", "v0.4.0"),
        None
    );
    assert_eq!(rewrite("docs/user", "#two-words", "v0.4.0"), None);
    assert_eq!(rewrite("docs/user", "/fiber/user/", "v0.4.0"), None);
}

#[test]
fn rewrite_maps_docs_markdown_to_its_page() {
    assert_eq!(
        rewrite("docs/user", "other.md", "v0.4.0"),
        Some("other.html".to_owned())
    );
    assert_eq!(
        rewrite("docs/user", "other.md#two-words", "v0.4.0"),
        Some("other.html#two-words".to_owned())
    );
    assert_eq!(
        rewrite("docs/user", "README.md", "v0.4.0"),
        Some("index.html".to_owned())
    );
    assert_eq!(
        rewrite("docs/user", "../releasing.md#installing", "v0.4.0"),
        Some("../releasing.html#installing".to_owned())
    );
    assert_eq!(
        rewrite("docs/deep/nested", "../../top.md", "v0.4.0"),
        Some("../../top.html".to_owned())
    );
    assert_eq!(
        rewrite("docs", "user/README.md", "v0.4.0"),
        Some("user/index.html".to_owned())
    );
}

#[test]
fn rewrite_sends_paths_leaving_docs_to_github_at_the_tag() {
    assert_eq!(
        rewrite("docs", "../crates/x.rs", "v0.4.0"),
        Some("https://github.com/aakshintala/fiber/blob/v0.4.0/crates/x.rs".to_owned())
    );
    assert_eq!(
        rewrite("docs/user", "../crates/x.rs", "v0.4.0"),
        None,
        "one level up from docs/user/ is still inside docs/"
    );
    assert_eq!(
        rewrite("docs/user", "../../crates/main/src/cli.rs", "v0.4.0"),
        Some("https://github.com/aakshintala/fiber/blob/v0.4.0/crates/main/src/cli.rs".to_owned())
    );
    assert_eq!(
        rewrite("docs", "../research/sandbox/", "v0.4.1-nightly.20261101"),
        Some(
            "https://github.com/aakshintala/fiber/tree/v0.4.1-nightly.20261101/research/sandbox"
                .to_owned()
        )
    );
    assert_eq!(
        rewrite("docs", "../README.md", "v0.4.0"),
        Some("https://github.com/aakshintala/fiber/blob/v0.4.0/README.md".to_owned())
    );
}

#[test]
fn rewrite_leaves_in_docs_directories_and_assets_alone() {
    assert_eq!(rewrite("docs", "user/", "v0.4.0"), None);
    assert_eq!(rewrite("docs/user", "shot.png", "v0.4.0"), None);
}

#[test]
fn page_name_maps_readmes_to_indexes_and_nothing_at_the_root() {
    assert_eq!(
        page_name("docs/user/install.md"),
        Some("user/install.html".to_owned())
    );
    assert_eq!(
        page_name("docs/user/README.md"),
        Some("user/index.html".to_owned())
    );
    assert_eq!(page_name("docs/README.md"), None);
    assert_eq!(page_name("other/install.md"), None);
}

#[test]
fn relative_url_climbs_and_strips_shared_prefixes() {
    assert_eq!(relative_url("user", "other.html"), "../other.html");
    assert_eq!(relative_url("user", "user/index.html"), "index.html");
    assert_eq!(relative_url("deep/nested", "top.html"), "../../top.html");
    assert_eq!(relative_url("", "top.html"), "top.html");
}

#[test]
fn escape_quotes_a_title_for_html() {
    assert_eq!(escape("a&b<c>d\"e"), "a&amp;b&lt;c&gt;d&quot;e");
}
