//! Tests for a reply's shared render slot: clones share one slot until
//! either changes its text.

use super::Reply;

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
