//! Tests for the work counters.

use super::{add, take};

#[test]
fn every_field_round_trips_through_take() {
    take();
    add(|work| {
        work.reply_renders += 1;
        work.turn_rows += 2;
        work.page_counts += 3;
        work.index_pages += 4;
        work.usage_summaries += 5;
        work.grapheme_widths += 6;
        work.search_pages += 7;
        work.search_order_pages += 8;
        work.shell_visits += 9;
        work.renderer_calls += 10;
        work.recall_builds += 11;
        work.path_lowercases += 12;
    });
    let work = take();
    assert_eq!(work.reply_renders, 1);
    assert_eq!(work.turn_rows, 2);
    assert_eq!(work.page_counts, 3);
    assert_eq!(work.index_pages, 4);
    assert_eq!(work.usage_summaries, 5);
    assert_eq!(work.grapheme_widths, 6);
    assert_eq!(work.search_pages, 7);
    assert_eq!(work.search_order_pages, 8);
    assert_eq!(work.shell_visits, 9);
    assert_eq!(work.renderer_calls, 10);
    assert_eq!(work.recall_builds, 11);
    assert_eq!(work.path_lowercases, 12);
    assert_eq!(take(), super::Work::default());
}
