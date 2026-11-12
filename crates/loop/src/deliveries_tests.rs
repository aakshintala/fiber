use super::{DELIVERY_LIMIT, LINE_LIMIT, bound_lines};

#[test]
fn replacement_line_limit_cuts_only_above_the_cap() {
    // docs/tools.md:1085 says each line is cut at 500 characters.
    assert_eq!(LINE_LIMIT, 500);
    for length in [LINE_LIMIT - 1, LINE_LIMIT] {
        let line = "x".repeat(length);
        assert_eq!(bound_lines(&line, ""), line);
    }
    let line = "x".repeat(LINE_LIMIT + 1);
    assert_eq!(
        bound_lines(&line, ""),
        format!("{} [cut]", "x".repeat(LINE_LIMIT))
    );
}

#[test]
fn replacement_delivery_limit_cuts_only_above_the_cap() {
    // docs/tools.md:1086 says each delivery is cut at 3,000 characters.
    assert_eq!(DELIVERY_LIMIT, 3_000);
    for length in [DELIVERY_LIMIT - 1, DELIVERY_LIMIT] {
        let delivery = delivery_of_length(length);
        assert_eq!(delivery.chars().count(), length);
        assert_eq!(bound_lines(&delivery, ""), delivery);
    }
    let delivery = delivery_of_length(DELIVERY_LIMIT + 1);
    assert_eq!(delivery.chars().count(), DELIVERY_LIMIT + 1);
    let prefix: String = delivery.chars().take(DELIVERY_LIMIT).collect();
    assert_eq!(
        bound_lines(&delivery, ""),
        format!("{prefix}\n[cut: 1 more characters]")
    );
}

fn delivery_of_length(total: usize) -> String {
    let mut remaining = total;
    let mut lines = Vec::new();
    while remaining > 0 {
        let line_length = remaining.min(LINE_LIMIT);
        lines.push("x".repeat(line_length));
        remaining -= line_length;
        remaining = remaining.saturating_sub(1);
    }
    lines.join("\n")
}
