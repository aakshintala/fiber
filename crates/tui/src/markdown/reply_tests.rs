//! Tests for a reply's shared render slot: clones share one slot until
//! either changes its text.

use super::Reply;

const TABLE: &str = "| a | b |\n|---|---|\n| c | d |\n";

#[test]
fn rendering_a_clone_fills_the_originals_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    let clone = reply.clone();
    assert_eq!(reply.cached_at(), None);
    clone.rendered(60);
    assert_eq!(reply.cached_at(), Some(60));
    assert_eq!(clone.cached_at(), Some(60));
}

#[test]
fn a_clones_push_leaves_the_originals_text() {
    let reply = Reply::new("hello".to_owned(), 1);
    let mut clone = reply.clone();
    clone.push(" there");
    assert_eq!(
        clone.rendered(60),
        Reply::new("hello there".to_owned(), 2).rendered(60)
    );
    assert_eq!(
        reply.rendered(60),
        Reply::new("hello".to_owned(), 3).rendered(60)
    );
}

#[test]
fn a_clones_set_leaves_the_originals_text() {
    let reply = Reply::new("hello".to_owned(), 1);
    let mut clone = reply.clone();
    clone.set("goodbye".to_owned());
    assert_eq!(
        clone.rendered(60),
        Reply::new("goodbye".to_owned(), 2).rendered(60)
    );
    assert_eq!(
        reply.rendered(60),
        Reply::new("hello".to_owned(), 3).rendered(60)
    );
}

#[test]
fn a_new_width_renders_again_for_both_clones() {
    let reply = Reply::new("hello".to_owned(), 1);
    let clone = reply.clone();
    reply.rendered(60);
    clone.rendered(80);
    assert_eq!(reply.cached_at(), Some(80));
    assert_eq!(clone.cached_at(), Some(80));
    assert_eq!(reply.rendered(80), clone.rendered(80));
}

#[test]
fn push_empties_only_the_changed_replys_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    reply.rendered(60);
    let mut clone = reply.clone();
    clone.push("!");
    assert_eq!(clone.cached_at(), None);
    assert_eq!(reply.cached_at(), Some(60));
}

#[test]
fn set_empties_only_the_changed_replys_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    reply.rendered(60);
    let mut clone = reply.clone();
    clone.set("goodbye".to_owned());
    assert_eq!(clone.cached_at(), None);
    assert_eq!(reply.cached_at(), Some(60));
}

#[test]
fn a_table_in_the_source_reads_as_table_sensitive() {
    let reply = Reply::new(TABLE.to_owned(), 1);
    assert_eq!(reply.table_cached(), None);
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
    // The answer is cached: parsing runs once.
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
}

#[test]
fn table_looking_text_without_a_table_reads_as_width_stable() {
    // Fenced code with pipes is no table, as the parser knows blocks.
    let fenced = Reply::new("```\n| a | b |\n|---|---|\n```\n".to_owned(), 1);
    assert!(!fenced.has_table());
    assert_eq!(fenced.table_cached(), Some(false));
    // Escaped pipes are no table either.
    let escaped = Reply::new("a \\| b \\| c\n".to_owned(), 2);
    assert!(!escaped.has_table());
    // Strikethrough alone, parsed with the renderer's options, is none.
    let struck = Reply::new("~~gone~~\n".to_owned(), 3);
    assert!(!struck.has_table());
    // Plain prose stays width-stable.
    let prose = Reply::new("just some words\n".to_owned(), 4);
    assert!(!prose.has_table());
}

#[test]
fn a_clone_shares_the_table_answer_until_it_changes_its_text() {
    let reply = Reply::new(TABLE.to_owned(), 1);
    let clone = reply.clone();
    assert!(clone.has_table());
    // The clone's parse filled the shared slot.
    assert_eq!(reply.table_cached(), Some(true));
    let mut clone = reply.clone();
    clone.set("plain words\n".to_owned());
    assert!(!clone.has_table());
    // The original still holds its own answer.
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
}

#[test]
fn push_and_set_invalidate_the_table_answer() {
    // Pushing a table onto prose introduces one.
    let mut reply = Reply::new("plain words\n".to_owned(), 1);
    assert!(!reply.has_table());
    reply.push(TABLE);
    assert_eq!(reply.table_cached(), None);
    assert!(reply.has_table());
    // Setting prose back removes it.
    reply.set("plain words\n".to_owned());
    assert_eq!(reply.table_cached(), None);
    assert!(!reply.has_table());
    // A push that leaves the answer detaches the clone's slot.
    let mut clone = Reply::new("plain words\n".to_owned(), 2);
    let shared = clone.clone();
    clone.push(" more\n");
    assert_eq!(clone.table_cached(), None);
    assert_eq!(shared.table_cached(), None);
    assert!(!clone.has_table());
    assert!(!shared.has_table());
}
