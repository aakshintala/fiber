//! Tests for the byte parser.

use super::{Edit, Event, Key, Parser, Reply};

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
fn ctrl_o_is_its_own_key() {
    assert_eq!(
        feed_all(&[&[0x0fu8, b'a'] as &[u8]]),
        vec![Event::Key(Key::CtrlO), Event::Key(Key::Char('a'))]
    );
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
    // Alt+z, which nothing binds: ESC and the byte after it go; the parser
    // moves on.
    assert_eq!(feed_all(&[b"\x1bza"]), vec![Event::Key(Key::Char('a'))]);
    // An SS3 key this slice does not bind (F2) is dropped.
    assert_eq!(feed_all(&[b"\x1bOQa"]), vec![Event::Key(Key::Char('a'))]);
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
    // Ctrl+A, and C1's first control as two bytes: neither is a key, and
    // the byte after each is read.
    assert_eq!(feed_all(&[b"\x01a"]), vec![Event::Key(Key::Char('a'))]);
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

/// One edit event.
fn edit(edit: Edit) -> Vec<Event> {
    vec![Event::Edit(edit)]
}

#[test]
fn legacy_arrows_left_and_right() {
    assert_eq!(feed_all(&[b"\x1b[D"]), edit(Edit::Left));
    assert_eq!(feed_all(&[b"\x1b[C"]), edit(Edit::Right));
    assert_eq!(feed_all(&[b"\x1bOD"]), edit(Edit::Left));
    assert_eq!(feed_all(&[b"\x1bOC"]), edit(Edit::Right));
    // An explicit "no modifiers" is the plain arrow.
    assert_eq!(feed_all(&[b"\x1b[1;1D"]), edit(Edit::Left));
}

#[test]
fn modified_arrows_move_by_word_or_line() {
    // Alt is 1 + 2, Ctrl 1 + 4, Super 1 + 8.
    assert_eq!(feed_all(&[b"\x1b[1;3D"]), edit(Edit::WordLeft));
    assert_eq!(feed_all(&[b"\x1b[1;3C"]), edit(Edit::WordRight));
    assert_eq!(feed_all(&[b"\x1b[1;5D"]), edit(Edit::WordLeft));
    assert_eq!(feed_all(&[b"\x1b[1;5C"]), edit(Edit::WordRight));
    assert_eq!(feed_all(&[b"\x1b[1;9D"]), edit(Edit::LineStart));
    assert_eq!(feed_all(&[b"\x1b[1;9C"]), edit(Edit::LineEnd));
    // Caps Lock (64) and Num Lock (128) do not change the key.
    assert_eq!(feed_all(&[b"\x1b[1;67D"]), edit(Edit::WordLeft));
    assert_eq!(feed_all(&[b"\x1b[1;133C"]), edit(Edit::WordRight));
    // Shift, and modifiers together, bind nothing here.
    assert!(feed_all(&[b"\x1b[1;2D"]).is_empty());
    assert!(feed_all(&[b"\x1b[1;7D"]).is_empty());
    assert!(feed_all(&[b"\x1b[1;11C"]).is_empty());
    assert!(feed_all(&[b"\x1b[1;13C"]).is_empty());
    assert!(feed_all(&[b"\x1b[1;17C"]).is_empty());
    assert!(feed_all(&[b"\x1b[2;3C"]).is_empty());
    assert!(feed_all(&[b"\x1b[1;+3C"]).is_empty());
}

#[test]
fn delete_and_legacy_word_keys() {
    assert_eq!(feed_all(&[b"\x1b[3~"]), edit(Edit::Delete));
    assert!(feed_all(&[b"\x1b[3;5~"]).is_empty());
    assert_eq!(feed_all(&[b"\x1bb"]), edit(Edit::WordLeft));
    assert_eq!(feed_all(&[b"\x1bf"]), edit(Edit::WordRight));
    assert_eq!(feed_all(&[b"\x1b\x7f"]), edit(Edit::DeleteWord));
}

#[test]
fn line_feed_is_ctrl_j() {
    assert_eq!(feed_all(&[b"\n"]), edit(Edit::CtrlJ));
    assert_eq!(
        feed_all(&[b"a\nb"]),
        vec![
            Event::Key(Key::Char('a')),
            Event::Edit(Edit::CtrlJ),
            Event::Key(Key::Char('b')),
        ]
    );
}

#[test]
fn kitty_keys_read_as_their_legacy_meaning() {
    assert_eq!(feed_all(&[b"\x1b[13u"]), vec![Event::Key(Key::Enter)]);
    assert_eq!(feed_all(&[b"\x1b[13;1u"]), vec![Event::Key(Key::Enter)]);
    assert_eq!(feed_all(&[b"\x1b[13;2u"]), edit(Edit::ShiftEnter));
    assert_eq!(feed_all(&[b"\x1b[27u"]), vec![Event::Key(Key::Esc)]);
    assert_eq!(feed_all(&[b"\x1b[99;5u"]), vec![Event::Key(Key::CtrlC)]);
    assert_eq!(feed_all(&[b"\x1b[111;5u"]), vec![Event::Key(Key::CtrlO)]);
    assert_eq!(feed_all(&[b"\x1b[106;5u"]), edit(Edit::CtrlJ));
    assert_eq!(feed_all(&[b"\x1b[127u"]), vec![Event::Key(Key::Backspace)]);
    assert_eq!(feed_all(&[b"\x1b[127;3u"]), edit(Edit::DeleteWord));
    assert_eq!(feed_all(&[b"\x1b[97;3u"]), vec![Event::Key(Key::AltA)]);
    assert_eq!(feed_all(&[b"\x1b[98;3u"]), edit(Edit::WordLeft));
    assert_eq!(feed_all(&[b"\x1b[102;3u"]), edit(Edit::WordRight));
    assert_eq!(feed_all(&[b"\x1b[9u"]), vec![Event::Key(Key::Tab)]);
    assert_eq!(feed_all(&[b"\x1b[9;2u"]), vec![Event::Key(Key::BackTab)]);
    // A lock modifier, and an event type or alternate key after a colon,
    // do not change the key.
    assert_eq!(feed_all(&[b"\x1b[99;69u"]), vec![Event::Key(Key::CtrlC)]);
    assert_eq!(
        feed_all(&[b"\x1b[99:67;5:1u"]),
        vec![Event::Key(Key::CtrlC)]
    );
}

#[test]
fn kitty_keys_with_other_modifiers_or_codes_drop_silently() {
    for bytes in [
        b"\x1b[13;3u".as_slice(),
        b"\x1b[13;5u",
        b"\x1b[13;9u",
        b"\x1b[27;2u",
        b"\x1b[99u",
        b"\x1b[99;3u",
        b"\x1b[99;7u",
        b"\x1b[99;13u",
        b"\x1b[111u",
        b"\x1b[111;3u",
        b"\x1b[106;3u",
        b"\x1b[127;5u",
        b"\x1b[127;2u",
        b"\x1b[97;5u",
        b"\x1b[97;7u",
        b"\x1b[98;5u",
        b"\x1b[9;3u",
        b"\x1b[9;5u",
        b"\x1b[57441u",
        b"\x1b[:;5u",
        b"\x1b[;5u",
        b"\x1b[99;u",
    ] {
        assert!(feed_all(&[bytes]).is_empty(), "{bytes:?}");
    }
    assert_eq!(
        feed_all(&[b"\x1b[57441;2ua"]),
        vec![Event::Key(Key::Char('a'))]
    );
}

#[test]
fn a_bracketed_paste_is_one_event_with_its_line_breaks() {
    assert_eq!(
        feed_all(&[b"\x1b[200~one\r\ntwo\rthree\nfour\x1b[201~"]),
        edit(Edit::Paste("one\ntwo\nthree\nfour".to_owned()))
    );
}

#[test]
fn a_paste_split_across_three_reads_is_held() {
    let mut parser = Parser::default();
    assert_eq!(
        parser.feed(b"a\x1b[200~fir"),
        vec![Event::Key(Key::Char('a'))]
    );
    assert!(parser.feed(b"st\nsec").is_empty());
    assert_eq!(
        parser.feed(b"ond\x1b[201~b"),
        vec![
            Event::Edit(Edit::Paste("first\nsecond".to_owned())),
            Event::Key(Key::Char('b')),
        ]
    );
}

#[test]
fn the_end_marker_split_anywhere_is_found() {
    let whole = b"\x1b[200~text\x1b[201~z";
    let start = b"\x1b[200~text".len();
    for cut in start..start + b"\x1b[201~".len() {
        let mut parser = Parser::default();
        let (head, tail) = whole.split_at(cut);
        assert!(parser.feed(head).is_empty(), "cut at {cut}");
        assert_eq!(
            parser.feed(tail),
            vec![
                Event::Edit(Edit::Paste("text".to_owned())),
                Event::Key(Key::Char('z')),
            ],
            "cut at {cut}"
        );
    }
}

#[test]
fn the_start_marker_split_across_reads_is_held() {
    let mut parser = Parser::default();
    assert!(parser.feed(b"\x1b[20").is_empty());
    assert!(parser.feed(b"0~x").is_empty());
    assert_eq!(parser.feed(b"\x1b[201~"), edit(Edit::Paste("x".to_owned())));
}

#[test]
fn a_paste_without_its_end_is_held_not_lost() {
    let mut parser = Parser::default();
    assert!(parser.feed(b"\x1b[200~kept").is_empty());
    assert!(parser.feed(b"\x03\x1b[A").is_empty());
    assert!(parser.feed(b" on").is_empty());
    assert_eq!(
        parser.feed(b"\x1b[201~"),
        edit(Edit::Paste("kept[A on".to_owned()))
    );
}

#[test]
fn a_paste_yields_no_keys_and_drops_control_characters() {
    assert_eq!(
        feed_all(&[b"\x1b[200~a\x1b\x03\x7f\tb\x00\n\x1b[201~"]),
        edit(Edit::Paste("a\tb\n".to_owned()))
    );
    // UTF-8, split across reads inside the paste, survives.
    let text = "é€😀".as_bytes();
    let mut parser = Parser::default();
    assert!(parser.feed(b"\x1b[200~").is_empty());
    for byte in text {
        assert!(parser.feed(&[*byte]).is_empty());
    }
    assert_eq!(
        parser.feed(b"\x1b[201~"),
        edit(Edit::Paste("é€😀".to_owned()))
    );
}

#[test]
fn an_empty_paste_is_nothing() {
    assert!(feed_all(&[b"\x1b[200~\x1b[201~"]).is_empty());
    assert!(feed_all(&[b"\x1b[200~\x01\x1b[201~"]).is_empty());
}

#[test]
fn with_kitty_pushed_a_lone_esc_ending_a_read_is_held() {
    // Esc is `CSI 27u` then, so the ESC starts a sequence: here a paste
    // start marker split right after it.
    let mut parser = Parser::default();
    parser.set_kitty();
    assert_eq!(parser.feed(b"a\x1b"), vec![Event::Key(Key::Char('a'))]);
    assert_eq!(
        parser.feed(b"[200~x\x1b[201~"),
        edit(Edit::Paste("x".to_owned()))
    );
    assert!(parser.feed(b"\x1b").is_empty());
    assert_eq!(parser.feed(b"[27u"), vec![Event::Key(Key::Esc)]);
    // An ESC with bytes after it in the same read is not held.
    assert_eq!(parser.feed(b"\x1ba"), vec![Event::Key(Key::AltA)]);
}

#[test]
fn tab_and_shift_tab() {
    assert_eq!(feed_all(&[b"\t"]), vec![Event::Key(Key::Tab)]);
    assert_eq!(feed_all(&[b"\x1b[Z"]), vec![Event::Key(Key::BackTab)]);
    // A modified CSI Z is no plain Shift+Tab.
    assert!(feed_all(&[b"\x1b[1;2Z"]).is_empty());
}

#[test]
fn f1_in_its_three_forms() {
    assert_eq!(feed_all(&[b"\x1bOP"]), vec![Event::Key(Key::F1)]);
    assert_eq!(feed_all(&[b"\x1b[11~"]), vec![Event::Key(Key::F1)]);
    assert_eq!(feed_all(&[b"\x1b[P"]), vec![Event::Key(Key::F1)]);
    // Neighbours are not F1: F2 as `CSI 12~` and a modified `CSI P`.
    assert!(feed_all(&[b"\x1b[12~"]).is_empty());
    assert!(feed_all(&[b"\x1b[1;2P"]).is_empty());
    assert!(feed_all(&[b"\x1b[1~"]).is_empty());
}

#[test]
fn esc_x_in_one_read_is_alt_x() {
    assert_eq!(feed_all(&[b"\x1bx"]), vec![Event::Key(Key::AltX)]);
    assert_eq!(
        feed_all(&[b"\x1b", b"x"]),
        vec![Event::Key(Key::Esc), Event::Key(Key::Char('x'))]
    );
}

#[test]
fn alt_arrows_in_their_csi_form() {
    assert_eq!(feed_all(&[b"\x1b[1;3A"]), vec![Event::Key(Key::AltUp)]);
    assert_eq!(feed_all(&[b"\x1b[1;3B"]), vec![Event::Key(Key::AltDown)]);
    // Split across reads, the sequence is held and still parses.
    assert_eq!(feed_all(&[b"\x1b[1;", b"3A"]), vec![Event::Key(Key::AltUp)]);
    assert_eq!(
        feed_all(&[b"\x1b[1;3", b"B"]),
        vec![Event::Key(Key::AltDown)]
    );
}

#[test]
fn esc_then_an_arrow_in_one_read_is_the_alt_arrow() {
    assert_eq!(feed_all(&[b"\x1b\x1b[A"]), vec![Event::Key(Key::AltUp)]);
    assert_eq!(feed_all(&[b"\x1b\x1b[B"]), vec![Event::Key(Key::AltDown)]);
    assert_eq!(
        feed_all(&[b"\x1b\x1b[", b"A"]),
        vec![Event::Key(Key::AltUp)]
    );
    assert_eq!(
        feed_all(&[b"\x1b\x1b[Ax"]),
        vec![Event::Key(Key::AltUp), Event::Key(Key::Char('x'))]
    );
    // ESC before another sequence is no Alt arrow.
    assert!(feed_all(&[b"\x1b\x1b[5~"]).is_empty());
}
