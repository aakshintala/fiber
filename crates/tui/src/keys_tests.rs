//! Tests for the byte parser.

use super::{Event, Key, Parser, Reply};

/// Feeds `chunks` in order, concatenating every read's events.
fn feed_all(chunks: &[&[u8]]) -> Vec<Event> {
    let mut parser = Parser::default();
    let mut out = Vec::new();
    for chunk in chunks {
        out.extend(parser.feed(chunk));
    }
    out
}

#[test]
fn printable_characters_become_chars() {
    assert_eq!(
        feed_all(&[b"hi"]),
        vec![Event::Key(Key::Char('h')), Event::Key(Key::Char('i')),]
    );
}

#[test]
fn backspace_enter_ctrl_c_and_esc() {
    assert_eq!(
        feed_all(&[&[0x7fu8] as &[u8]]),
        vec![Event::Key(Key::Backspace)]
    );
    assert_eq!(
        feed_all(&[&[0x08u8] as &[u8]]),
        vec![Event::Key(Key::Backspace)]
    );
    assert_eq!(feed_all(&[b"\r".as_slice()]), vec![Event::Key(Key::Enter)]);
    assert_eq!(
        feed_all(&[&[0x03u8] as &[u8]]),
        vec![Event::Key(Key::CtrlC)]
    );
    assert_eq!(feed_all(&[&[0x1bu8] as &[u8]]), vec![Event::Key(Key::Esc)]);
}

#[test]
fn page_up_page_down_and_end() {
    assert_eq!(feed_all(&[b"\x1b[5~"]), vec![Event::Key(Key::PageUp)]);
    assert_eq!(feed_all(&[b"\x1b[6~"]), vec![Event::Key(Key::PageDown)]);
    assert_eq!(feed_all(&[b"\x1b[F"]), vec![Event::Key(Key::End)]);
    assert_eq!(feed_all(&[b"\x1b[4~"]), vec![Event::Key(Key::End)]);
    assert_eq!(feed_all(&[b"\x1bOF"]), vec![Event::Key(Key::End)]);
}

#[test]
fn one_chunk_with_two_keys() {
    assert_eq!(
        feed_all(&[b"a\x1b[5~"]),
        vec![Event::Key(Key::Char('a')), Event::Key(Key::PageUp),]
    );
}

#[test]
fn esc_followed_by_a_sequence_in_one_read_is_no_esc() {
    assert_eq!(feed_all(&[b"\x1b[5~"]), vec![Event::Key(Key::PageUp)],);
}

#[test]
fn a_csi_split_across_two_reads_is_held() {
    let mut parser = Parser::default();
    assert!(parser.feed(b"\x1b[5").is_empty());
    assert_eq!(parser.feed(b"~"), vec![Event::Key(Key::PageUp)],);
}

#[test]
fn kitty_and_device_attributes_replies() {
    assert_eq!(
        feed_all(&[b"\x1b[?5u"]),
        vec![Event::Reply(Reply::KittyFlags(5))],
    );
    assert_eq!(
        feed_all(&[b"\x1b[?1;2c"]),
        vec![Event::Reply(Reply::DeviceAttributes)],
    );
}

#[test]
fn utf8_split_across_reads() {
    let text = "é".as_bytes();
    let mut parser = Parser::default();
    assert!(parser.feed(&text[..1]).is_empty());
    assert_eq!(parser.feed(&text[1..]), vec![Event::Key(Key::Char('é'))],);
}

#[test]
fn unknown_csi_is_dropped() {
    assert!(feed_all(&[b"\x1b[99X"]).is_empty());
    assert_eq!(feed_all(&[b"\x1b[99Xa"]), vec![Event::Key(Key::Char('a'))],);
}

#[test]
fn esc_o_split_across_reads_is_held() {
    let mut parser = Parser::default();
    assert!(parser.feed(b"\x1bO").is_empty());
    assert_eq!(parser.feed(b"F"), vec![Event::Key(Key::End)]);
}

#[test]
fn esc_with_any_other_byte_is_dropped_whole() {
    // Alt+x: ESC and the byte after it go; the parser moves on.
    assert_eq!(feed_all(&[b"\x1bxa"]), vec![Event::Key(Key::Char('a'))]);
    // An SS3 key this slice does not bind is dropped.
    assert_eq!(feed_all(&[b"\x1bOPa"]), vec![Event::Key(Key::Char('a'))]);
}

#[test]
fn a_csi_c_or_u_without_the_question_mark_is_no_reply() {
    assert!(feed_all(&[b"\x1b[0c"]).is_empty());
    assert!(feed_all(&[b"\x1b[5u"]).is_empty());
    assert!(feed_all(&[b"\x1b[?u"]).is_empty());
    assert!(feed_all(&[b"\x1b[?5;1u"]).is_empty());
    assert!(feed_all(&[b"\x1b[15u"]).is_empty());
    assert!(feed_all(&[b"\x1b[?+5u"]).is_empty());
    assert!(feed_all(&[b"\x1b[1;2F"]).is_empty());
}

#[test]
fn three_and_four_byte_characters_split_across_reads() {
    for text in ["€", "😀"] {
        let bytes = text.as_bytes();
        let mut parser = Parser::default();
        for at in 1..bytes.len() {
            assert!(parser.feed(&bytes[at - 1..at]).is_empty(), "{text} at {at}");
        }
        let last = bytes.len() - 1;
        let ch = text.chars().next().unwrap_or_default();
        assert_eq!(parser.feed(&bytes[last..]), vec![Event::Key(Key::Char(ch))]);
    }
}

#[test]
fn invalid_utf8_drops_one_byte_and_keeps_the_rest() {
    // A lead byte cut off by a byte that does not continue it, at the end
    // of a read: the lead goes, the rest is read.
    assert_eq!(feed_all(&[b"\xe2a"]), vec![Event::Key(Key::Char('a'))]);
    // A lone continuation byte, and a byte no character starts with.
    assert_eq!(feed_all(&[b"\x80\xffb"]), vec![Event::Key(Key::Char('b'))]);
    // A full-length sequence that is not UTF-8.
    assert_eq!(feed_all(&[b"\xc3a"]), vec![Event::Key(Key::Char('a'))]);
}

#[test]
fn a_csi_with_a_stray_byte_drops_esc_bracket_and_reads_on() {
    assert_eq!(feed_all(&[b"\x1b[\x01a"]), vec![Event::Key(Key::Char('a'))]);
}

#[test]
fn control_characters_are_dropped_whole() {
    // A tab, and C1's first control as two bytes: neither is a key, and
    // the byte after each is read.
    assert_eq!(feed_all(&[b"\ta"]), vec![Event::Key(Key::Char('a'))]);
    assert_eq!(feed_all(&[b"\xc2\x80b"]), vec![Event::Key(Key::Char('b'))]);
}

#[test]
fn up_and_down_in_csi_and_ss3_forms() {
    assert_eq!(feed_all(&[b"\x1b[A"]), vec![Event::Key(Key::Up)]);
    assert_eq!(feed_all(&[b"\x1b[B"]), vec![Event::Key(Key::Down)]);
    assert_eq!(feed_all(&[b"\x1bOA"]), vec![Event::Key(Key::Up)]);
    assert_eq!(feed_all(&[b"\x1bOB"]), vec![Event::Key(Key::Down)]);
    // A modified arrow is no plain Up.
    assert!(feed_all(&[b"\x1b[1;2A"]).is_empty());
    assert!(feed_all(&[b"\x1b[1;2B"]).is_empty());
}

#[test]
fn esc_a_in_one_read_is_alt_a() {
    assert_eq!(feed_all(&[b"\x1ba"]), vec![Event::Key(Key::AltA)]);
    assert_eq!(
        feed_all(&[b"\x1bab"]),
        vec![Event::Key(Key::AltA), Event::Key(Key::Char('b'))]
    );
}

#[test]
fn esc_ending_a_read_then_a_is_esc_then_a() {
    assert_eq!(
        feed_all(&[b"\x1b", b"a"]),
        vec![Event::Key(Key::Esc), Event::Key(Key::Char('a'))]
    );
}
