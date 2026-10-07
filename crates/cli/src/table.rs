//! The text tables the listing commands print: `fiber models` and
//! `fiber sessions`.

/// One line per row: each column left-aligned and padded to its widest
/// cell, columns two spaces apart, trailing spaces trimmed.
pub(crate) fn pad(rows: &[Vec<String>]) -> Vec<String> {
    let mut widths: Vec<usize> = Vec::new();
    for cells in rows {
        for (index, cell) in cells.iter().enumerate() {
            let width = cell.chars().count();
            match widths.get_mut(index) {
                Some(widest) => *widest = (*widest).max(width),
                None => widths.push(width),
            }
        }
    }
    rows.iter()
        .map(|cells| {
            let mut line = String::new();
            for (index, cell) in cells.iter().enumerate() {
                if index > 0 {
                    line.push_str("  ");
                }
                let width = widths.get(index).copied().unwrap_or(0);
                line.push_str(&format!("{cell:<width$}"));
            }
            line.truncate(line.trim_end_matches(' ').len());
            line
        })
        .collect()
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
