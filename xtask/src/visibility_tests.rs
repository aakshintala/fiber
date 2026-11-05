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
fn a_lonely_inline_mod_declaration_is_skipped() {
    let source = concat!(
        "//! lonely\n",
        "//! fixture\n",
        "pub(crate) mod lonely {\n",
        "    fn hidden() {}\n",
        "}\n",
    );
    assert_eq!(single(source), Vec::<String>::new());
}

#[test]
fn a_file_with_only_private_items_passes() {
    assert_eq!(single("//! clean\nfn main() {}\n"), Vec::<String>::new());
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
}

#[test]
fn a_pub_crate_field_is_skipped() {
    let source = concat!("pub(crate) struct S {\n", "    pub(crate) x: u32,\n", "}\n",);
    assert_eq!(single(source), Vec::<String>::new());
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
    // The `mod` opener itself is out of scope; an empty result proves the
    // region held, since a broken region would flag `f`.
    assert_eq!(single(source), Vec::<String>::new());
}

#[test]
fn a_cfg_attributed_opener_still_opens_a_region() {
    let source = concat!(
        "#[cfg(test)]\n",
        "pub(crate) mod seam {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
    );
    assert_eq!(single(source), Vec::<String>::new());
}

#[test]
fn an_unbalanced_file_is_skipped() {
    let source = concat!(
        "pub(crate) mod m {\n",
        "    pub(crate) fn f() {}\n",
        "    // the closing brace never comes\n",
        "    pub(crate) fn g() {}\n",
    );
    assert_eq!(single(source), Vec::<String>::new());
}

#[test]
fn a_stray_close_brace_skips_the_file() {
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
        vec!["crates/x/src/only.rs:3: after is pub(crate) but no other file in x names it"]
    );
}

#[test]
fn a_file_that_does_not_tokenise_is_skipped() {
    assert_eq!(
        single("pub(crate) fn lonely() {}\n\"unterminated\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_paren_delimited_macro_body_is_skipped() {
    let source = concat!(
        "macro_rules! m (\n",
        "    ($x:expr) => { pub(crate) fn generated() {} }\n",
        ")\n",
    );
    assert_eq!(single(source), Vec::<String>::new());
}

#[test]
fn a_macro_two_definition_body_is_skipped() {
    let source = concat!("macro m {\n", "    pub(crate) fn generated() {}\n", "}\n",);
    assert_eq!(single(source), Vec::<String>::new());
}

#[test]
fn a_bare_pub_mod_opens_a_region() {
    let source = concat!(
        "pub mod holder {\n",
        "    pub(crate) fn fire() {}\n",
        "}\n",
        "fn run() { holder::fire(); }\n",
    );
    assert_eq!(single(source), Vec::<String>::new());
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
