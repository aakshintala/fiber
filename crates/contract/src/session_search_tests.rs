//! [`Label`](super::Label)'s `--json` wire words.

use super::Label;

#[test]
fn labels_name_their_json_words() {
    for (label, word) in [
        (Label::Message, "message"),
        (Label::ToolInput, "tool_input"),
        (Label::ToolOutput, "tool_output"),
    ] {
        assert_eq!(label.as_str(), word, "{label:?}");
    }
}
