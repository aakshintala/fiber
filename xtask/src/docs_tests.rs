use super::*;
use crate::test_dir::TestDir;

const TARGET: &str = "# Target

## The gate

## `unsafe`

```
## Not a heading
```
";

fn check(source: &str) -> Vec<String> {
    let dir = TestDir::new("docs");
    dir.write("docs/target.md", TARGET);
    dir.write("docs/source.md", source);
    dir.write("scripts/.keep", "");
    check_file(dir.path(), "docs/source.md").unwrap()
}

#[test]
fn links_citations_and_paths_that_resolve_pass() {
    let source =
        "[t](target.md#the-gate) [u](target.md#unsafe) [s](#top) [x](https://example.com/nope)
[up](../docs/target.md) [dot](./target.md \"title\")
(`docs/target.md`, \"The gate\") (`target.md`, \"`unsafe`\")
`scripts/` `docs/<area>.md` `docs/*.md` `docs/a b`
[ref][r] [Collapsed][] [also ref][R]

[r]: target.md#the-gate
[collapsed]: <target.md>
# Top
";
    assert_eq!(check(source), Vec::<String>::new());
}

#[test]
fn a_link_to_a_missing_file_or_anchor_fails() {
    assert_eq!(
        check("[a](missing.md) [b](target.md#nope) [c](#nope)\n"),
        [
            "docs/source.md:1: link to missing.md: no such file",
            "docs/source.md:1: link to target.md#nope: no such anchor",
            "docs/source.md:1: link to #nope: no such anchor",
        ]
    );
}

#[test]
fn a_reference_link_to_a_missing_file_or_anchor_fails() {
    let source = "See [a][gone] and [b][bad-anchor].\n\n[gone]: missing.md\n  [bad-anchor]: target.md#nope\n";
    assert_eq!(
        check(source),
        [
            "docs/source.md:3: link to missing.md: no such file",
            "docs/source.md:4: link to target.md#nope: no such anchor",
        ]
    );
}

#[test]
fn a_definition_whose_target_is_on_the_next_line_is_checked() {
    let source = "A [shortcut] and a [collapsed][].\n\n[shortcut]:\n  missing.md\n[collapsed]:\ntarget.md#nope\n[fine]:\n   target.md#the-gate \"title\"\n";
    assert_eq!(
        check(source),
        [
            "docs/source.md:3: link to missing.md: no such file",
            "docs/source.md:5: link to target.md#nope: no such anchor",
        ]
    );
}

#[test]
fn a_definition_with_no_target_is_not_a_link() {
    assert_eq!(check("[x]:\n\n[x]:"), Vec::<String>::new());
}

#[test]
fn a_reference_with_no_definition_fails() {
    assert_eq!(
        check("[a][nowhere] and [Else][]\n"),
        [
            "docs/source.md:1: reference [nowhere]: no definition",
            "docs/source.md:1: reference [else]: no definition"
        ]
    );
}

#[test]
fn brackets_that_are_not_a_reference_are_ignored() {
    assert_eq!(
        check("x][[y] [z][ ] [shortcut]\n\n[z]: target.md\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_reference_label_may_break_across_lines() {
    assert_eq!(
        check("[a][b\nc] [d][e\nf]\n\n[e f]: target.md\n"),
        ["docs/source.md:1: reference [b c]: no definition"]
    );
}

#[test]
fn a_code_span_that_is_not_a_markdown_file_is_not_a_citation() {
    assert_eq!(
        check("`not md`, \"The gate\" and `a b.md`, \"x\"\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_citation_of_a_missing_heading_fails_even_across_lines() {
    let source =
        "Intro\n(`docs/target.md`,\n\"The\nold gate\")\n(`docs/target.md`, \"Not a heading\")\n";
    assert_eq!(
        check(source),
        [
            "docs/source.md:2: citation of docs/target.md, \"The old gate\": no such heading",
            "docs/source.md:5: citation of docs/target.md, \"Not a heading\": no such heading",
        ]
    );
}

#[test]
fn a_citation_of_a_missing_file_fails() {
    assert_eq!(
        check("(`docs/gone.md`, \"The gate\")\n"),
        [
            "docs/source.md:1: citation of docs/gone.md: no such file",
            "docs/source.md:1: path docs/gone.md: does not exist",
        ]
    );
}

#[test]
fn a_file_name_without_a_quoted_heading_is_not_a_citation() {
    assert_eq!(
        check("`gone.md`, then \"The gate\" and `gone.md`,\"x\" and `gone.md` \"x\"\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_backticked_path_that_does_not_exist_fails() {
    assert_eq!(
        check("`not a path` `scripts/check`\n"),
        ["docs/source.md:1: path scripts/check: does not exist"]
    );
}

#[test]
fn fenced_code_is_ignored() {
    assert_eq!(
        check("```\n[a](missing.md) `scripts/nope`\n```\n~~~\n[b][none]\n~~~\n"),
        Vec::<String>::new()
    );
}

#[test]
fn anchors_follow_github() {
    assert_eq!(
        slug("Tests and development tools"),
        "tests-and-development-tools"
    );
    assert_eq!(slug("`unsafe`"), "unsafe");
    assert_eq!(slug("What a panic leaves"), "what-a-panic-leaves");
    assert_eq!(slug("snake_case, and-dashes"), "snake_case-and-dashes");
    let found: Vec<String> = anchors("# A\n## A\n### A ##\n").into_iter().collect();
    assert_eq!(found, ["a", "a-1", "a-2"]);
}

#[test]
fn headings_need_a_space_and_at_most_six_hashes() {
    assert_eq!(
        headings("#Not\n####### Not\n###### Six\n#\tTab\n"),
        ["Six", "Tab"]
    );
}

#[test]
fn the_checked_files_are_docs_and_the_root_guides() {
    let dir = TestDir::new("files");
    dir.write("docs/b.md", "");
    dir.write("docs/adr/a.md", "");
    dir.write("docs/notes.txt", "");
    dir.write("README.md", "");
    dir.write("research/x.md", "");
    assert_eq!(
        checked_files(dir.path()).unwrap(),
        ["docs/adr/a.md", "docs/b.md", "README.md"]
    );
}
