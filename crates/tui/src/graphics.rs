//! Kitty's graphics escapes and Unicode placeholders (`docs/tui.md`,
//! "Images"). The terminal holds the image and moves it with the text
//! through the placeholders; Fiber keeps no decoded image.

use ratatui::style::Style;
use ratatui::text::Span;

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
#[allow(dead_code, reason = "answers transmit the fetched bytes")]
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

/// The transmit bytes for `id`'s PNG `payload`: the base64 forwarded as
/// kitty's payload in 4096-byte chunks, `m=1` on all but the last.
/// Transmit only: the image shows when its placeholders draw
/// (`docs/tui.md`, "Images").
#[allow(dead_code, reason = "answers transmit the fetched bytes")]
pub(crate) fn transmit(id: u32, base64_png: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunks = base64_png.as_bytes().chunks(CHUNK).peekable();
    if chunks.peek().is_none() {
        return out;
    }
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            out.extend_from_slice(
                format!(
                    "\x1b_Ga=t,U=1,f=100,i={id},q=2,m={more};{}",
                    String::from_utf8_lossy(chunk)
                )
                .as_bytes(),
            );
            first = false;
        } else {
            out.extend_from_slice(
                format!("\x1b_Gm={more};{}", String::from_utf8_lossy(chunk)).as_bytes(),
            );
        }
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// The place bytes for `id` over its `cols` by `rows` grid: the image
/// shows where its placeholders are, moved with the text
/// (`docs/tui.md`, "Images").
#[allow(dead_code, reason = "answers place the shown image")]
pub(crate) fn place(id: u32, cols: u16, rows: u16) -> Vec<u8> {
    format!("\x1b_Ga=p,U=1,i={id},c={cols},r={rows}\x1b\\").into_bytes()
}

/// The delete bytes for `id`: its instances and its stored image go, so
/// a later session never draws under a stale id (`docs/tui.md`,
/// "Images").
#[allow(dead_code, reason = "session changes delete the shown images")]
pub(crate) fn delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

#[cfg(test)]
#[path = "graphics_tests.rs"]
mod tests;
