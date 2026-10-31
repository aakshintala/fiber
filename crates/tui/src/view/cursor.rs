//! The cursor cell: the covered character drawn in the slab's colour on
//! the text colour, or a block in the text colour past the last character
//! (`docs/tui.md`, "The input box").

use ratatui::style::Style;

use crate::theme::Role;

/// The cursor's symbol and style for `covered`, the complete cell symbol
/// under the caret, or for the past-end position: the covered symbol in
/// the slab's colour on the text colour, or a block in the text colour.
/// No modifier either way: the explicit colours carry the cursor on both
/// themes, and a span naming its own background keeps it over the frame's
/// tint (`docs/tui.md`, "The input box").
pub(crate) fn cursor_cell(covered: Option<&str>, slab: Role, text: Role) -> (String, Style) {
    match covered {
        Some(symbol) => (
            symbol.to_owned(),
            Style::new().fg(slab.color()).bg(text.color()),
        ),
        None => ("█".to_owned(), Style::new().fg(text.color())),
    }
}

/// The complete cell symbol `row` draws at display column `col`: every
/// grapheme whole, so a combining mark or a wide character survives. `None`
/// past the row's last column, where the block draws instead.
pub(crate) fn covered_symbol(row: &str, col: usize) -> Option<String> {
    let mut at: usize = 0;
    let mut found = None;
    crate::text::each_piece(row, |piece, _| {
        let end = at.saturating_add(crate::text::width(piece));
        if col >= at && col < end {
            found = Some(piece.to_owned());
            return false;
        }
        at = end;
        true
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covered_keeps_the_symbol_in_the_slab_colour_on_the_text() {
        let (symbol, style) = cursor_cell(Some("b"), Role::Surface, Role::Text);
        assert_eq!(symbol, "b");
        assert_eq!(
            style,
            Style::new()
                .fg(Role::Surface.color())
                .bg(Role::Text.color())
        );
    }

    #[test]
    fn past_end_is_a_block_in_the_text_colour_with_no_background() {
        let (symbol, style) = cursor_cell(None, Role::Surface, Role::Text);
        assert_eq!(symbol, "█");
        assert_eq!(style, Style::new().fg(Role::Text.color()));
        assert_eq!(style.bg, None);
    }

    #[test]
    fn covered_keeps_a_whole_grapheme() {
        // A combining mark rides with its base, and an emoji ZWJ
        // sequence stays one symbol: the helper never splits what the
        // terminal draws as one cell run.
        let (symbol, _) = cursor_cell(Some("e\u{301}"), Role::Surface, Role::Text);
        assert_eq!(symbol, "e\u{301}");
        let (symbol, _) = cursor_cell(Some("👨‍👩‍👧"), Role::Surface, Role::Text);
        assert_eq!(symbol, "👨‍👩‍👧");
    }

    #[test]
    fn covered_symbol_walks_display_columns() {
        assert_eq!(covered_symbol("abc", 0).as_deref(), Some("a"));
        assert_eq!(covered_symbol("abc", 1).as_deref(), Some("b"));
        assert_eq!(covered_symbol("abc", 3), None);
        assert_eq!(covered_symbol("", 0), None);
    }

    #[test]
    fn covered_symbol_covers_whole_wide_graphemes() {
        assert_eq!(covered_symbol("Ｗx", 0).as_deref(), Some("Ｗ"));
        assert_eq!(covered_symbol("Ｗx", 1).as_deref(), Some("Ｗ"));
        assert_eq!(covered_symbol("Ｗx", 2).as_deref(), Some("x"));
        assert_eq!(covered_symbol("Ｗx", 3), None);
    }

    #[test]
    fn covered_symbol_keeps_a_combining_mark_with_its_base() {
        assert_eq!(covered_symbol("e\u{301}x", 0).as_deref(), Some("e\u{301}"));
        assert_eq!(covered_symbol("e\u{301}x", 1).as_deref(), Some("x"));
    }
}
