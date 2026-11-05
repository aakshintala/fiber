//! Unit tests for the visibility check: each declaration kind, the
//! use-search edges, and the inline-`mod` region skip.

use super::*;

fn file(rel: &str, source: &str) -> RustFile {
    RustFile {
        krate: "x".to_owned(),
        path: format!("crates/x/{rel}"),
        rel: rel.to_owned(),
        source: source.to_owned(),
    }
}

fn failures(files: Vec<RustFile>) -> Vec<String> {
    super::check_with_baseline(&files, &[])
}

fn single(source: &str) -> Vec<String> {
    failures(vec![file("src/only.rs", source)])
}

#[test]
fn a_pub_crate_fn_named_in_no_other_file_is_flagged() {
    assert_eq!(
        single("pub(crate) fn solo() -> u32 { 1 }\n"),
        vec!["crates/x/src/only.rs:1: solo is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn naming_the_fn_in_a_second_file_of_the_crate_passes() {
    let files = vec![
        file(
            "src/only.rs",
            "pub(crate) fn solo() -> u32 { 1 }\npub(crate) fn lonely() {}\n",
        ),
        file("src/lib.rs", "//! b\nfn run() { solo(); }\n"),
    ];
    // `solo` is named elsewhere so only the outside `lonely` reports: the
    // pass is targeted, and a no-op checker returns empty here.
    assert_eq!(
        failures(files),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_name_match_in_another_files_comment_passes() {
    let files = vec![
        file(
            "src/only.rs",
            "pub(crate) fn solo() -> u32 { 1 }\npub(crate) fn lonely() {}\n",
        ),
        file("src/lib.rs", "//! b\n// solo is defined elsewhere\n"),
    ];
    // The comment still counts as a use of `solo`; `lonely` proves the
    // checker ran, and a no-op checker returns empty here.
    assert_eq!(
        failures(files),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_reexport_in_another_file_passes() {
    let files = vec![
        file(
            "src/only.rs",
            "pub(crate) fn solo() -> u32 { 1 }\npub(crate) fn lonely() {}\n",
        ),
        file("src/lib.rs", "pub use crate::only::solo;\n"),
    ];
    // The re-export names `solo`; `lonely` proves the checker ran, and a
    // no-op checker returns empty here.
    assert_eq!(
        failures(files),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_declaration_inside_macro_rules_is_skipped() {
    let source = concat!(
        "pub(crate) fn lonely() {}\n",
        "macro_rules! m {\n",
        "    pub(crate) fn generated() {}\n",
        "}\n",
    );
    // `generated` is skipped but the outside `lonely` still reports: the
    // skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:1: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_name_used_only_inside_a_macro_body_in_another_file_passes() {
    let files = vec![
        file(
            "src/only.rs",
            "pub(crate) fn solo() -> u32 { 1 }\npub(crate) fn lonely() {}\n",
        ),
        file(
            "src/macros.rs",
            "macro_rules! m {\n    ($x:expr) => { solo($x) }\n}\n",
        ),
    ];
    // The macro body still counts as a use of `solo`; `lonely` proves the
    // checker ran, and a no-op checker returns empty here.
    assert_eq!(
        failures(files),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_pub_super_fn_named_from_a_child_module_file_passes() {
    let files = vec![
        file(
            "src/outer.rs",
            "//! outer\npub(super) fn boost() -> u32 { 1 }\npub(crate) fn lonely() {}\n",
        ),
        file("src/inner.rs", "//! inner\nfn f() { super::boost(); }\n"),
    ];
    // `boost` is named from the child module so only the outside `lonely`
    // reports: the pass is targeted, and a no-op checker returns empty here.
    assert_eq!(
        failures(files),
        vec!["crates/x/src/outer.rs:3: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_pub_crate_fn_inside_a_private_inline_mod_passes() {
    let source = concat!(
        "pub(crate) fn lonely() {}\n",
        "mod holder {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
        "fn run() { holder::fire(); }\n",
    );
    // `fire` hides inside the inline mod but the outside `lonely` still
    // reports: the skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:1: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_twin_cfg_seam_pair_passes_when_another_file_names_it() {
    let seamed = concat!(
        "//! seamed\n",
        "#[cfg(not(test))]\n",
        "pub(crate) mod seam {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
        "#[cfg(test)]\n",
        "pub(crate) mod seam {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
        "pub(crate) fn lonely() {}\n",
    );
    let files = vec![
        file("src/seamed.rs", seamed),
        file("src/lib.rs", "//! b\nfn uses_seam() { seam::setup(); }\n"),
    ];
    // Both `fire` copies hide inside the cfg seam mods but the outside
    // `lonely` still reports: the skip is targeted, and a no-op checker
    // returns empty here.
    assert_eq!(
        failures(files),
        vec!["crates/x/src/seamed.rs:10: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_lonely_inline_mod_declaration_is_skipped() {
    let source = concat!(
        "//! lonely\n",
        "//! fixture\n",
        "pub(crate) fn outside() {}\n",
        "pub(crate) mod lonely {\n",
        "    fn hidden() {}\n",
        "}\n",
    );
    // The `mod` opener itself is out of scope but the outside `outside`
    // still reports: the skip is targeted, and a no-op checker returns
    // empty here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:3: outside is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_file_with_only_private_items_passes() {
    // The private items stay silent but the outside `lonely` still reports,
    // so a no-op checker returning empty fails here.
    assert_eq!(
        single("//! clean\npub(crate) fn lonely() {}\nfn main() {}\n"),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn each_plain_item_kind_is_flagged_when_lonely() {
    assert_eq!(
        single("pub(crate) const LIMIT: u32 = 1;\n"),
        vec!["crates/x/src/only.rs:1: LIMIT is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) static COUNT: u32 = 0;\n"),
        vec!["crates/x/src/only.rs:1: COUNT is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) static mut FLAG: bool = false;\n"),
        vec!["crates/x/src/only.rs:1: FLAG is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn type_definitions_are_skipped() {
    assert_eq!(single("pub(crate) struct Solo;\n"), Vec::<String>::new());
    assert_eq!(
        single("pub(crate) enum Choice { A }\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        single("pub(crate) union Bits { a: u32, b: f32 }\n"),
        Vec::<String>::new()
    );
    assert_eq!(single("pub(crate) trait Serve {}\n"), Vec::<String>::new());
    assert_eq!(
        single("pub(crate) type Alias = u32;\n"),
        Vec::<String>::new()
    );
    assert_eq!(single("pub(crate) mod sib;\n"), Vec::<String>::new());
    assert_eq!(
        single("pub(crate) use other::thing;\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        single("pub(crate) use other::thing as tool;\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        single("pub(crate) use other::{a, b};\n"),
        Vec::<String>::new()
    );
    assert_eq!(single("pub(crate) use other::*;\n"), Vec::<String>::new());
    assert_eq!(single("extern crate foo;\n"), Vec::<String>::new());
    // Positive control: a checkable item beside a skipped type still
    // reports, so a no-op checker returning empty fails here.
    assert_eq!(
        single("pub(crate) struct Solo;\npub(crate) fn lonely() {}\n"),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_pub_crate_field_is_skipped() {
    let source = concat!(
        "pub(crate) struct S {\n",
        "    pub(crate) x: u32,\n",
        "}\n",
        "pub(crate) fn lonely() {}\n",
    );
    // The field stays skipped but the outside `lonely` still reports: the
    // skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:4: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_lonely_pub_super_fn_reports_the_super_spelling() {
    assert_eq!(
        single("pub(super) fn up() -> u32 { 1 }\n"),
        vec!["crates/x/src/only.rs:1: up is pub(super) but no other file in x names it"]
    );
}

#[test]
fn a_one_line_inline_mod_hides_its_items() {
    let files = vec![
        file(
            "src/tiny_holder.rs",
            "pub(crate) mod tiny { pub(crate) fn f() {} }\npub(crate) fn lonely() {}\n",
        ),
        file("src/lib.rs", "//! b\nfn t() { tiny::go(); }\n"),
    ];
    // `f` hides inside the one-line mod but the outside `lonely` still
    // reports: the skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        failures(files),
        vec!["crates/x/src/tiny_holder.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn nested_inline_mods_hide_inner_items() {
    let source = concat!(
        "pub(crate) mod outer {\n",
        "    pub(crate) mod inner {\n",
        "        pub(crate) fn deep() {}\n",
        "    }\n",
        "}\n",
        "pub(crate) fn lonely() {}\n",
    );
    let files = vec![
        file("src/outer.rs", source),
        file("src/lib.rs", "//! b\nfn o() { outer::go(); }\n"),
    ];
    // `deep` hides inside the nested mods but the outside `lonely` still
    // reports: the skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        failures(files),
        vec!["crates/x/src/outer.rs:6: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn braces_in_strings_and_comments_do_not_close_a_region() {
    let source = concat!(
        "pub(crate) mod m {\n",
        "    fn s() { let _ = \"}\"; }\n",
        "    // }\n",
        "    pub(crate) fn f() {}\n",
        "}\n",
        "pub(crate) fn lonely() {}\n",
    );
    // The `mod` opener itself is out of scope; `lonely` reporting while `f`
    // stays hidden proves the region held, and a no-op checker returns empty
    // here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:6: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_cfg_attributed_opener_still_opens_a_region() {
    let source = concat!(
        "#[cfg(test)]\n",
        "pub(crate) mod seam {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
        "pub(crate) fn lonely() {}\n",
    );
    // `fire` hides behind the attributed opener but the outside `lonely`
    // still reports: the skip is targeted, and a no-op checker returns empty
    // here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:5: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn an_unbalanced_file_is_skipped() {
    let source = concat!(
        "pub(crate) mod m {\n",
        "    pub(crate) fn f() {}\n",
        "    // the closing brace never comes\n",
        "    pub(crate) fn g() {}\n",
    );
    // The whole file fails to tokenise so nothing in it reports; `lonely`
    // in the other file proves the checker ran, and a no-op checker returns
    // empty here.
    let files = vec![
        file("src/broken.rs", source),
        file("src/other.rs", "pub(crate) fn lonely() {}\n"),
    ];
    assert_eq!(
        failures(files),
        vec!["crates/x/src/other.rs:1: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_stray_close_brace_skips_the_file() {
    // The stray brace fails the whole file so nothing in it reports;
    // `lonely` in the other file proves the checker ran, and a no-op checker
    // returns empty here.
    let files = vec![
        file("src/broken.rs", "}\n    pub(crate) fn g() {}\n"),
        file("src/other.rs", "pub(crate) fn lonely() {}\n"),
    ];
    assert_eq!(
        failures(files),
        vec!["crates/x/src/other.rs:1: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_declaration_after_a_closed_region_is_checked() {
    let source = concat!("pub(crate) mod m {\n", "}\n", "pub(crate) fn after() {}\n",);
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:3: after is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_file_that_does_not_tokenise_is_skipped() {
    // The unterminated string fails the whole file so nothing in it reports;
    // `lonely` in the other file proves the checker ran, and a no-op checker
    // returns empty here.
    let files = vec![
        file(
            "src/broken.rs",
            "pub(crate) fn lonely() {}\n\"unterminated\n",
        ),
        file("src/other.rs", "pub(crate) fn lonely2() {}\n"),
    ];
    assert_eq!(
        failures(files),
        vec!["crates/x/src/other.rs:1: lonely2 is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_paren_delimited_macro_body_is_skipped() {
    let source = concat!(
        "pub(crate) fn lonely() {}\n",
        "macro_rules! m (\n",
        "    ($x:expr) => { pub(crate) fn generated() {} }\n",
        ")\n",
    );
    // `generated` is skipped but the outside `lonely` still reports: the
    // skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:1: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_macro_two_definition_body_is_skipped() {
    let source = concat!(
        "pub(crate) fn lonely() {}\n",
        "macro m {\n",
        "    pub(crate) fn generated() {}\n",
        "}\n",
    );
    // `generated` is skipped but the outside `lonely` still reports: the
    // skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:1: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_bare_pub_mod_opens_a_region() {
    let source = concat!(
        "pub(crate) fn lonely() {}\n",
        "pub mod holder {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
        "fn run() { holder::fire(); }\n",
    );
    // `fire` hides inside the bare-`pub` mod but the outside `lonely` still
    // reports: the skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:1: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn qualified_fn_forms_are_flagged() {
    assert_eq!(
        single("pub(crate) const fn f() -> u32 { 1 }\n"),
        vec!["crates/x/src/only.rs:1: f is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) unsafe fn f() {}\n"),
        vec!["crates/x/src/only.rs:1: f is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) extern \"C\" fn f();\n"),
        vec!["crates/x/src/only.rs:1: f is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn other_visibility_spellings_are_skipped() {
    assert_eq!(single("pub fn f() {}\n"), Vec::<String>::new());
    assert_eq!(single("pub(in crate) fn f() {}\n"), Vec::<String>::new());
    assert_eq!(
        single("pub(crate) fn r#type() {}\n"),
        vec!["crates/x/src/only.rs:1: type is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_pub_followed_by_a_non_paren_group_is_not_a_restricted_visibility() {
    // Neither bracket shape is a visibility qualifier, so nothing in the
    // fixture reports; the outside `lonely` proves the checker ran, and a
    // no-op checker returns empty here.
    assert_eq!(
        single("pub [crate] fn f() {}\npub(crate) fn lonely() {}\n"),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub {crate} fn f() {}\npub(crate) fn lonely() {}\n"),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_mod_name_followed_by_a_non_brace_group_is_not_an_inline_module() {
    assert_eq!(
        single("mod m (pub(crate) fn f() {})\n"),
        vec!["crates/x/src/only.rs:1: f is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("mod m [pub(crate) fn f() {}]\n"),
        vec!["crates/x/src/only.rs:1: f is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_macro_rules_header_with_an_ident_where_the_bang_belongs_is_not_a_macro_definition() {
    // `m` sits where `!` belongs, so this is not a macro definition and the
    // body is still checked. Kills the `is_punct` → `true` mutant, whose
    // only call site is the `!` check: under it the header reads as a
    // definition and the body is wrongly skipped.
    assert_eq!(
        single("macro_rules m foo {\n    pub(crate) fn generated() {}\n}\n"),
        vec!["crates/x/src/only.rs:2: generated is pub(crate) but no other file in x names it"]
    );
}

fn baseline(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| (*entry).to_owned()).collect()
}

#[test]
fn a_baselined_site_is_skipped() {
    let files = vec![file(
        "src/only.rs",
        "pub(crate) fn solo() -> u32 { 1 }\npub(crate) fn lonely() {}\n",
    )];
    // The baseline holds `solo` but the outside `lonely` still reports: the
    // skip is targeted, and a no-op checker returns empty here.
    assert_eq!(
        super::check_with_baseline(&files, &baseline(&["crates/x/src/only.rs:solo"])),
        vec!["crates/x/src/only.rs:2: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_flagged_site_missing_from_the_baseline_is_still_reported() {
    let files = vec![
        file("src/kept.rs", "pub(crate) fn kept() {}\n"),
        file("src/held.rs", "pub(crate) fn held() {}\n"),
    ];
    assert_eq!(
        super::check_with_baseline(&files, &baseline(&["crates/x/src/held.rs:held"])),
        vec!["crates/x/src/kept.rs:1: kept is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_baseline_entry_matching_no_flagged_site_fails_the_check() {
    let files = vec![file("src/only.rs", "//! clean\nfn main() {}\n")];
    assert_eq!(
        super::check_with_baseline(&files, &baseline(&["crates/x/src/only.rs:gone"])),
        vec!["crates/x/src/only.rs:gone is in the visibility baseline but matches no flagged site"]
    );
}

#[test]
fn a_baselined_site_named_elsewhere_is_reported_stale() {
    let files = vec![
        file("src/only.rs", "pub(crate) fn solo() -> u32 { 1 }\n"),
        file("src/lib.rs", "//! b\nfn run() { solo(); }\n"),
    ];
    assert_eq!(
        super::check_with_baseline(&files, &baseline(&["crates/x/src/only.rs:solo"])),
        vec!["crates/x/src/only.rs:solo is in the visibility baseline but matches no flagged site"]
    );
}

#[test]
fn blank_baseline_lines_are_ignored() {
    assert_eq!(super::parse_baseline("\n  \n\t\n"), Vec::<String>::new());
    assert_eq!(
        super::parse_baseline("crates/x/src/only.rs:solo\n\n  \n"),
        vec!["crates/x/src/only.rs:solo".to_owned()]
    );
}

#[test]
fn a_missing_baseline_file_means_empty() {
    let dir = fakes::TempDir::new("visibility-no-baseline");
    assert_eq!(super::load_baseline_from(dir.path()), Vec::<String>::new());
}

#[test]
fn a_baseline_file_above_the_working_directory_applies() {
    let dir = fakes::TempDir::new("visibility-baseline-subdir");
    dir.write(
        "xtask/src/visibility_baseline.txt",
        "crates/x/src/only.rs:solo\n",
    );
    let sub = dir.path().join("crates").join("x").join("src");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(
        super::load_baseline_from(&sub),
        vec!["crates/x/src/only.rs:solo".to_owned()]
    );
}
