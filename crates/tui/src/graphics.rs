//! Kitty's graphics escapes and Unicode placeholders (`docs/tui.md`,
//! "Images"). The terminal holds the image and moves it with the text
//! through the placeholders; Fiber keeps no decoded image.

use ratatui::style::Style;
use ratatui::text::Span;

pub(crate) mod logo;

/// The placeholder's base character: the terminal shows the image where
/// these cells are (`docs/tui.md`, "Images").
const PLACEHOLDER: char = '\u{10EEEE}';

/// The row/column diacritics: the first 64 entries of the specification's
/// rowcolumn-diacritics table, in order.
const DIA: [char; 64] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}',
    '\u{346}', '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}',
    '\u{35B}', '\u{363}', '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}',
    '\u{36A}', '\u{36B}', '\u{36C}', '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}',
    '\u{485}', '\u{486}', '\u{487}', '\u{592}', '\u{593}', '\u{594}', '\u{595}', '\u{597}',
    '\u{598}', '\u{599}', '\u{59C}', '\u{59D}', '\u{59E}', '\u{59F}', '\u{5A0}', '\u{5A1}',
    '\u{5A8}', '\u{5A9}', '\u{5AB}', '\u{5AC}', '\u{5AF}', '\u{5C4}', '\u{610}', '\u{611}',
    '\u{612}', '\u{613}', '\u{614}', '\u{615}', '\u{616}', '\u{617}', '\u{657}', '\u{658}',
];

/// The base64 chunk an escape carries at most.
const CHUNK: usize = 4096;

/// Whether `symbol` is a placeholder cell: it starts with the base
/// character, with the row and column diacritics after it.
pub(crate) fn is_placeholder(symbol: &str) -> bool {
    symbol.starts_with(PLACEHOLDER)
}

/// One placeholder cell for (`row`, `col`): the base character with
/// the row diacritic then the column diacritic. A cell past the
/// diacritic table reads as the bare base character, never panicking.
/// The style comes from the look, which alone names colours
/// (`docs/tui.md`, "Themes").
pub(crate) fn cell(row: usize, col: usize, style: Style) -> Span<'static> {
    let row = DIA.get(row).copied().unwrap_or(PLACEHOLDER);
    let col = DIA.get(col).copied().unwrap_or(PLACEHOLDER);
    Span::styled(format!("{PLACEHOLDER}{row}{col}"), style)
}

/// The transmit bytes for `id`'s PNG `payload`: one entry per whole
/// 4096-byte chunk, `m=1` on all but the last, so the writer thread sends
/// each chunk between loop writes without splitting an escape.
/// Transmit only: the image shows when its placeholders draw
/// (`docs/tui.md`, "Images"). The transmit carries no `U=1`: that flag
/// binds a placement to its Unicode placeholders. Concatenating the
/// chunks reproduces the one-write bytes.
pub(crate) fn transmit(id: u32, base64_png: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunks = base64_png.as_bytes().chunks(CHUNK).peekable();
    if chunks.peek().is_none() {
        return out;
    }
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        let mut piece = Vec::new();
        if first {
            piece.extend_from_slice(
                format!(
                    "\x1b_Ga=t,f=100,i={id},q=2,m={more};{}",
                    String::from_utf8_lossy(chunk)
                )
                .as_bytes(),
            );
            first = false;
        } else {
            piece.extend_from_slice(
                format!("\x1b_Gm={more};{}", String::from_utf8_lossy(chunk)).as_bytes(),
            );
        }
        piece.extend_from_slice(b"\x1b\\");
        out.push(piece);
    }
    out
}

/// The place bytes for `id` over its `cols` by `rows` grid: the image
/// shows where its placeholders are, moved with the text
/// (`docs/tui.md`, "Images").
pub(crate) fn place(id: u32, cols: u16, rows: u16) -> Vec<u8> {
    format!("\x1b_Ga=p,U=1,i={id},c={cols},r={rows}\x1b\\").into_bytes()
}

/// The delete bytes for `id`: its instances and its stored image go, so
/// a later session never draws under a stale id (`docs/tui.md`,
/// "Images").
pub(crate) fn delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

#[cfg(test)]
mod tests {
    //! Tests for kitty's graphics escapes: the place bytes pin `U=1`,
    //! the transmit chunks, and the placeholder cells.

    use ratatui::style::Style;

    use super::{cell, delete, is_placeholder, place, transmit};

    #[test]
    fn place_pins_unicode_placement() {
        // The terminal moves the image with the text, through the Unicode
        // placeholders: always `U=1` under the image's id.
        assert_eq!(place(7, 32, 12), b"\x1b_Ga=p,U=1,i=7,c=32,r=12\x1b\\");
    }

    #[test]
    fn delete_clears_the_image() {
        assert_eq!(delete(7), b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
    }

    #[test]
    fn transmit_carries_no_unicode_placement() {
        // `U=1` binds a placement to its placeholders: the transmit
        // carries the PNG format, the id, quiet and the last-chunk mark.
        assert_eq!(
            transmit(7, "AAAA"),
            vec![b"\x1b_Ga=t,f=100,i=7,q=2,m=0;AAAA\x1b\\".to_vec()]
        );
    }

    #[test]
    fn transmit_empty_yields_no_chunks() {
        // Nothing to send means no chunk: the place still goes on its
        // own, so an empty payload never writes a partial escape.
        assert!(transmit(7, "").is_empty());
    }

    #[test]
    fn transmit_chunks_at_4096() {
        let prefix = "\x1b_Ga=t,f=100,i=7,q=2,m=0;";
        // Exactly one chunk still sends `m=0` once.
        let full = "B".repeat(4096);
        let chunks = transmit(7, &full);
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0],
            [prefix.as_bytes(), full.as_bytes(), b"\x1b\\"].concat()
        );
    }

    #[test]
    fn transmit_marks_every_chunk_but_the_last_with_more() {
        let payload = "B".repeat(4097);
        let chunks = transmit(7, &payload);
        // Dropping the last chunk would lose the tail: both land.
        assert_eq!(chunks.len(), 2);
        let first = String::from_utf8(chunks[0].clone()).expect("escapes are text");
        assert!(first.starts_with("\x1b_Ga=t,f=100,i=7,q=2,m=1;"));
        assert_eq!(
            first.len(),
            "\x1b_Ga=t,f=100,i=7,q=2,m=1;".len() + 4096 + "\x1b\\".len()
        );
        assert_eq!(chunks[1], b"\x1b_Gm=0;B\x1b\\");
        // Concatenating reproduces the one-write bytes.
        let joined: Vec<u8> = chunks.concat();
        let text = String::from_utf8(joined).expect("escapes are text");
        let parts: Vec<&str> = text.split("\x1b\\").collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1], "\x1b_Gm=0;B");
    }

    #[test]
    fn placeholder_cells_carry_row_and_column() {
        let style = Style::new();
        let first = cell(0, 0, style);
        assert_eq!(first.content.as_ref(), "\u{10EEEE}\u{305}\u{305}");
        assert_eq!(first.style, style);
        let next = cell(1, 2, style);
        assert_eq!(next.content.as_ref(), "\u{10EEEE}\u{30D}\u{30E}");
        // Past the diacritic table reads as the bare base character, never
        // panicking.
        assert_eq!(
            cell(64, 64, style).content.as_ref(),
            "\u{10EEEE}\u{10EEEE}\u{10EEEE}"
        );
    }

    #[test]
    fn is_placeholder_names_only_placeholder_cells() {
        assert!(is_placeholder("\u{10EEEE}\u{305}\u{305}"));
        assert!(!is_placeholder(" "));
        assert!(!is_placeholder("▣"));
        assert!(!is_placeholder(""));
    }
}
