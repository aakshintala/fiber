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
    super::check(&files)
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
        file("src/only.rs", "pub(crate) fn solo() -> u32 { 1 }\n"),
        file("src/lib.rs", "//! b\nfn run() { solo(); }\n"),
    ];
    assert_eq!(failures(files), Vec::<String>::new());
}

#[test]
fn a_name_match_in_another_files_comment_passes() {
    let files = vec![
        file("src/only.rs", "pub(crate) fn solo() -> u32 { 1 }\n"),
        file("src/lib.rs", "//! b\n// solo is defined elsewhere\n"),
    ];
    assert_eq!(failures(files), Vec::<String>::new());
}

#[test]
fn a_reexport_in_another_file_passes() {
    let files = vec![
        file("src/only.rs", "pub(crate) fn solo() -> u32 { 1 }\n"),
        file("src/lib.rs", "pub use crate::only::solo;\n"),
    ];
    assert_eq!(failures(files), Vec::<String>::new());
}

#[test]
fn a_declaration_inside_macro_rules_is_skipped() {
    let source = concat!(
        "macro_rules! m {\n",
        "    pub(crate) fn generated() {}\n",
        "}\n",
    );
    assert_eq!(single(source), Vec::<String>::new());
}

#[test]
fn a_name_used_only_inside_a_macro_body_in_another_file_passes() {
    let files = vec![
        file("src/only.rs", "pub(crate) fn solo() -> u32 { 1 }\n"),
        file(
            "src/macros.rs",
            "macro_rules! m {\n    ($x:expr) => { solo($x) }\n}\n",
        ),
    ];
    assert_eq!(failures(files), Vec::<String>::new());
}

#[test]
fn a_pub_super_fn_named_from_a_child_module_file_passes() {
    let files = vec![
        file(
            "src/outer.rs",
            "//! outer\npub(super) fn boost() -> u32 { 1 }\n",
        ),
        file("src/inner.rs", "//! inner\nfn f() { super::boost(); }\n"),
    ];
    assert_eq!(failures(files), Vec::<String>::new());
}

#[test]
fn a_pub_crate_fn_inside_a_private_inline_mod_passes() {
    let source = concat!(
        "mod holder {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
        "fn run() { holder::fire(); }\n",
    );
    assert_eq!(single(source), Vec::<String>::new());
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
    );
    let files = vec![
        file("src/seamed.rs", seamed),
        file("src/lib.rs", "//! b\nfn uses_seam() { seam::setup(); }\n"),
    ];
    assert_eq!(failures(files), Vec::<String>::new());
}

#[test]
fn a_lonely_inline_mod_declaration_is_flagged() {
    let source = concat!(
        "//! lonely\n",
        "//! fixture\n",
        "pub(crate) mod lonely {\n",
        "    fn hidden() {}\n",
        "}\n",
    );
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:3: lonely is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_union_named_in_no_other_file_is_flagged() {
    assert_eq!(
        single("pub(crate) union Bits { a: u32, b: f32 }\n"),
        vec!["crates/x/src/only.rs:1: Bits is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_file_with_only_private_items_passes() {
    assert_eq!(single("//! clean\nfn main() {}\n"), Vec::<String>::new());
}

#[test]
fn each_plain_item_kind_is_flagged_when_lonely() {
    assert_eq!(
        single("pub(crate) struct Solo;\n"),
        vec!["crates/x/src/only.rs:1: Solo is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) enum Choice { A }\n"),
        vec!["crates/x/src/only.rs:1: Choice is pub(crate) but no other file in x names it"]
    );
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
    assert_eq!(
        single("pub(crate) type Alias = u32;\n"),
        vec!["crates/x/src/only.rs:1: Alias is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) trait Serve {}\n"),
        vec!["crates/x/src/only.rs:1: Serve is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) mod sib;\n"),
        vec!["crates/x/src/only.rs:1: sib is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_pub_crate_use_names_its_final_segment() {
    assert_eq!(
        single("pub(crate) use other::thing;\n"),
        vec!["crates/x/src/only.rs:1: thing is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) use other::thing as tool;\n"),
        vec!["crates/x/src/only.rs:1: tool is pub(crate) but no other file in x names it"]
    );
    assert_eq!(
        single("pub(crate) use other::{a, b};\n"),
        Vec::<String>::new()
    );
    assert_eq!(single("pub(crate) use other::*;\n"), Vec::<String>::new());
    assert_eq!(single("extern crate foo;\n"), Vec::<String>::new());
}

#[test]
fn a_pub_crate_field_is_flagged_with_its_line() {
    let source = concat!("pub(crate) struct S {\n", "    pub(crate) x: u32,\n", "}\n",);
    assert_eq!(
        single(source),
        vec![
            "crates/x/src/only.rs:1: S is pub(crate) but no other file in x names it",
            "crates/x/src/only.rs:2: x is pub(crate) but no other file in x names it",
        ]
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
            "pub(crate) mod tiny { pub(crate) fn f() {} }\n",
        ),
        file("src/lib.rs", "//! b\nfn t() { tiny::go(); }\n"),
    ];
    assert_eq!(failures(files), Vec::<String>::new());
}

#[test]
fn nested_inline_mods_hide_inner_items() {
    let source = concat!(
        "pub(crate) mod outer {\n",
        "    pub(crate) mod inner {\n",
        "        pub(crate) fn deep() {}\n",
        "    }\n",
        "}\n",
    );
    let files = vec![
        file("src/outer.rs", source),
        file("src/lib.rs", "//! b\nfn o() { outer::go(); }\n"),
    ];
    assert_eq!(failures(files), Vec::<String>::new());
}

#[test]
fn braces_in_strings_and_comments_do_not_close_a_region() {
    let source = concat!(
        "pub(crate) mod m {\n",
        "    fn s() { let _ = \"}\"; }\n",
        "    // }\n",
        "    pub(crate) fn f() {}\n",
        "}\n",
    );
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:1: m is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_cfg_attributed_opener_still_opens_a_region() {
    let source = concat!(
        "#[cfg(test)]\n",
        "pub(crate) mod seam {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
    );
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:2: seam is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn an_unclosed_region_skips_to_end_of_file() {
    let source = concat!(
        "pub(crate) mod m {\n",
        "    pub(crate) fn f() {}\n",
        "    // the closing brace never comes\n",
        "    pub(crate) fn g() {}\n",
    );
    assert_eq!(
        single(source),
        vec!["crates/x/src/only.rs:1: m is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_depth_anomaly_skips_to_end_of_file() {
    assert_eq!(
        single("}\n    pub(crate) fn g() {}\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_declaration_after_a_closed_region_is_checked() {
    let source = concat!("pub(crate) mod m {\n", "}\n", "pub(crate) fn after() {}\n",);
    assert_eq!(
        single(source),
        vec![
            "crates/x/src/only.rs:1: m is pub(crate) but no other file in x names it",
            "crates/x/src/only.rs:3: after is pub(crate) but no other file in x names it",
        ]
    );
}
