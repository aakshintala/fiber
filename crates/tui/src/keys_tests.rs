//! Tests for the byte parser: bytes read as strokes, and strokes map
//! through `default_event` to the keys and edits the app matches.

use super::{Button, Edit, Event, Key, Mouse, MouseKind, Parser, Reply, default_event};
use crate::stroke::{Code, Mods, Stroke};

/// Feeds `chunks` in order, mapping every stroke through `default_event`
/// and concatenating every read's events.
fn feed_all(chunks: &[&[u8]]) -> Vec<Event> {
    let mut parser = Parser::default();
    let mut out = Vec::new();
    for chunk in chunks {
        for event in parser.feed(chunk) {
            match event {
                Event::Stroke(stroke) => {
                    if let Some(mapped) = default_event(&stroke) {
                        out.push(mapped);
                    }
                }
                Event::Key(_) | Event::Edit(_) | Event::Mouse(_) | Event::Reply(_) => {
                    out.push(event)
                }
            }
        }
    }
    out
}

/// Feeds `chunks` in order, returning the strokes before `default_event`
/// maps them.
fn feed_strokes(chunks: &[&[u8]]) -> Vec<Stroke> {
    let mut parser = Parser::default();
    let mut out = Vec::new();
    for chunk in chunks {
        for event in parser.feed(chunk) {
            if let Event::Stroke(stroke) = event {
                out.push(stroke);
            }
        }
    }
    out
}

/// A stroke with no modifiers.
fn plain(code: Code) -> Stroke {
    Stroke {
        code,
        mods: Mods::NONE,
    }
}

/// A stroke with modifiers.
fn modified(code: Code, mods: Mods) -> Stroke {
    Stroke { code, mods }
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
    assert_eq!(parser.feed(b"~"), vec![Event::Stroke(plain(Code::PageUp))]);
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
    assert_eq!(
        parser.feed(&text[1..]),
        vec![Event::Stroke(plain(Code::Char('é')))]
    );
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
    assert_eq!(parser.feed(b"F"), vec![Event::Stroke(plain(Code::End))]);
}

#[test]
fn esc_with_any_other_byte_is_dropped_whole() {
    // Alt+z names a stroke nothing binds, so only `a` maps through; an SS3
    // key this slice does not bind (F2) likewise maps to nothing.
    assert_eq!(feed_all(&[b"\x1bza"]), vec![Event::Key(Key::Char('a'))]);
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
        assert_eq!(
            parser.feed(&bytes[last..]),
            vec![Event::Stroke(plain(Code::Char(ch)))]
        );
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
        b"\x1b[99;3u",
        b"\x1b[99;7u",
        b"\x1b[99;13u",
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
        vec![Event::Stroke(plain(Code::Char('a')))]
    );
    assert!(parser.feed(b"st\nsec").is_empty());
    assert_eq!(
        parser.feed(b"ond\x1b[201~b"),
        vec![
            Event::Edit(Edit::Paste("first\nsecond".to_owned())),
            Event::Stroke(plain(Code::Char('b'))),
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
                Event::Stroke(plain(Code::Char('z'))),
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
    assert!(!parser.kitty());
    parser.set_kitty();
    assert!(parser.kitty());
    assert_eq!(
        parser.feed(b"a\x1b"),
        vec![Event::Stroke(plain(Code::Char('a')))]
    );
    assert_eq!(
        parser.feed(b"[200~x\x1b[201~"),
        edit(Edit::Paste("x".to_owned()))
    );
    assert!(parser.feed(b"\x1b").is_empty());
    assert_eq!(parser.feed(b"[27u"), vec![Event::Stroke(plain(Code::Esc))]);
    // An ESC with bytes after it in the same read is not held.
    assert_eq!(
        parser.feed(b"\x1ba"),
        vec![Event::Stroke(modified(Code::Char('a'), Mods::ALT))]
    );
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
fn ctrl_g_and_ctrl_r_in_legacy_and_kitty_forms() {
    assert_eq!(feed_all(&[b"\x07"]), vec![Event::Key(Key::CtrlG)]);
    assert_eq!(feed_all(&[b"\x12"]), vec![Event::Key(Key::CtrlR)]);
    assert_eq!(feed_all(&[b"\x1b[103;5u"]), vec![Event::Key(Key::CtrlG)]);
    assert_eq!(feed_all(&[b"\x1b[114;5u"]), vec![Event::Key(Key::CtrlR)]);
    // A lock key changes nothing; another modifier or a plain code is no
    // binding.
    assert_eq!(feed_all(&[b"\x1b[114;69u"]), vec![Event::Key(Key::CtrlR)]);
    for bytes in [b"\x1b[103;3u".as_slice(), b"\x1b[103;6u", b"\x1b[114;7u"] {
        assert!(feed_all(&[bytes]).is_empty(), "{bytes:?}");
    }
}

#[test]
fn esc_x_in_one_read_is_alt_x() {
    assert_eq!(feed_all(&[b"\x1bx"]), vec![Event::Key(Key::AltX)]);
    // With kitty's flags, Alt+X is `CSI 120;3u`.
    assert_eq!(feed_all(&[b"\x1b[120;3u"]), vec![Event::Key(Key::AltX)]);
    // A plain `x` through kitty reads as the stroke now, and types `x`.
    assert_eq!(feed_all(&[b"\x1b[120u"]), vec![Event::Key(Key::Char('x'))]);
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

#[test]
fn esc_esc_before_another_byte_is_no_alt_arrow() {
    // The two ESCs are dropped and the parser reads on.
    assert_eq!(
        feed_all(&[b"\x1b\x1bxa"]),
        vec![Event::Key(Key::Char('x')), Event::Key(Key::Char('a'))]
    );
    assert_eq!(feed_all(&[b"\x1b\x1bx"]), vec![Event::Key(Key::Char('x'))]);
}

/// One mouse event at 0-based `col`, `row`.
fn mouse(kind: MouseKind, col: u16, row: u16) -> Event {
    Event::Mouse(Mouse { kind, col, row })
}

#[test]
fn sgr_presses_and_releases() {
    assert_eq!(
        feed_all(&[b"\x1b[<0;10;5M"]),
        vec![mouse(MouseKind::Press(Button::Left), 9, 4)]
    );
    assert_eq!(
        feed_all(&[b"\x1b[<1;12;3M"]),
        vec![mouse(MouseKind::Press(Button::Middle), 11, 2)]
    );
    assert_eq!(
        feed_all(&[b"\x1b[<2;1;1M"]),
        vec![mouse(MouseKind::Press(Button::Right), 0, 0)]
    );
    // A release is any `m`, whatever button it names.
    assert_eq!(
        feed_all(&[b"\x1b[<0;10;5m"]),
        vec![mouse(MouseKind::Release, 9, 4)]
    );
    assert_eq!(
        feed_all(&[b"\x1b[<3;10;5m"]),
        vec![mouse(MouseKind::Release, 9, 4)]
    );
    // A press of no button is no event.
    assert!(feed_all(&[b"\x1b[<3;10;5M"]).is_empty());
}

#[test]
fn sgr_motion_drag_and_wheel() {
    assert_eq!(
        feed_all(&[b"\x1b[<35;3;2M"]),
        vec![mouse(MouseKind::Motion, 2, 1)]
    );
    assert_eq!(
        feed_all(&[b"\x1b[<32;3;2M"]),
        vec![mouse(MouseKind::Drag(Button::Left), 2, 1)]
    );
    assert_eq!(
        feed_all(&[b"\x1b[<34;3;2M"]),
        vec![mouse(MouseKind::Drag(Button::Right), 2, 1)]
    );
    assert_eq!(
        feed_all(&[b"\x1b[<64;1;1M"]),
        vec![mouse(MouseKind::WheelUp, 0, 0)]
    );
    assert_eq!(
        feed_all(&[b"\x1b[<65;1;1M"]),
        vec![mouse(MouseKind::WheelDown, 0, 0)]
    );
    // Horizontal wheels, and buttons 8 and up, are no events.
    assert!(feed_all(&[b"\x1b[<66;1;1M"]).is_empty());
    assert!(feed_all(&[b"\x1b[<67;1;1M"]).is_empty());
    assert!(feed_all(&[b"\x1b[<128;1;1M"]).is_empty());
}

#[test]
fn sgr_modifier_bits_are_ignored() {
    for cb in [8, 16, 24] {
        let report = format!("\x1b[<{cb};1;1M");
        assert_eq!(
            feed_all(&[report.as_bytes()]),
            vec![mouse(MouseKind::Press(Button::Left), 0, 0)],
            "{report:?}"
        );
    }
    assert_eq!(
        feed_all(&[b"\x1b[<51;4;4M"]),
        vec![mouse(MouseKind::Motion, 3, 3)]
    );
    assert_eq!(
        feed_all(&[b"\x1b[<80;1;1M"]),
        vec![mouse(MouseKind::WheelUp, 0, 0)]
    );
}

#[test]
fn shift_mouse_reports_are_dropped() {
    // A Shift report stays the terminal's native selection: no event.
    for (name, report) in [
        ("Shift press", "\x1b[<4;3;2M"),
        ("Shift drag", "\x1b[<36;3;2M"),
        ("Shift release", "\x1b[<4;3;2m"),
        ("Shift wheel", "\x1b[<68;3;2M"),
        ("Shift with Ctrl", "\x1b[<20;3;2M"),
    ] {
        assert!(feed_all(&[report.as_bytes()]).is_empty(), "{name}");
    }
    // Alt and Ctrl still parse.
    for (name, report) in [
        ("Alt press", "\x1b[<8;3;2M"),
        ("Ctrl press", "\x1b[<16;3;2M"),
    ] {
        assert_eq!(
            feed_all(&[report.as_bytes()]),
            vec![mouse(MouseKind::Press(Button::Left), 2, 1)],
            "{name}"
        );
    }
}

#[test]
fn sgr_coordinates_up_to_u16_max() {
    assert_eq!(
        feed_all(&[b"\x1b[<0;65535;65535M"]),
        vec![mouse(MouseKind::Press(Button::Left), 65534, 65534)]
    );
}

#[test]
fn a_malformed_sgr_report_is_dropped_and_the_rest_read() {
    // A non-digit parameter byte; a letter would end the CSI itself.
    let malformed: [&[u8]; 11] = [
        b"\x1b[<0;0;5M",
        b"\x1b[<+0;1;1M",
        b"\x1b[<0;5;0M",
        b"\x1b[<0;70000;1M",
        b"\x1b[<0;1;65536M",
        b"\x1b[<0;1M",
        b"\x1b[<0;1;1;1M",
        b"\x1b[<?;1;1M",
        b"\x1b[<0:1;1;1M",
        b"\x1b[<0;;1M",
        b"\x1b[<0;1;1X",
    ];
    for report in malformed {
        let mut bytes = report.to_vec();
        bytes.push(b'a');
        assert_eq!(
            feed_all(&[&bytes]),
            vec![Event::Key(Key::Char('a'))],
            "{report:?}"
        );
    }
    // Without the `<` it is no SGR report, even when the parameters after
    // the first byte would read as one.
    assert!(feed_all(&[b"\x1b[0;1;1M"]).is_empty());
    assert!(feed_all(&[b"\x1b[10;10;5M"]).is_empty());
}

#[test]
fn an_sgr_report_split_across_reads_is_one_event() {
    let mut parser = Parser::default();
    assert!(parser.feed(b"\x1b[<0;1").is_empty());
    assert_eq!(
        parser.feed(b"2;3M"),
        vec![mouse(MouseKind::Press(Button::Left), 11, 2)]
    );
}

#[test]
fn alt_p_r_and_digits_parse_in_legacy_and_kitty_forms() {
    let mut cases: Vec<(Vec<u8>, Key)> = vec![
        (b"\x1bp".to_vec(), Key::AltP),
        (b"\x1b[112;3u".to_vec(), Key::AltP),
        (b"\x1br".to_vec(), Key::AltR),
        (b"\x1b[114;3u".to_vec(), Key::AltR),
    ];
    for digit in 1..=9u8 {
        cases.push((vec![0x1b, b'0' + digit], Key::AltDigit(digit)));
        cases.push((
            format!("\x1b[{};3u", 48 + u32::from(digit)).into_bytes(),
            Key::AltDigit(digit),
        ));
    }
    for (bytes, key) in cases {
        assert_eq!(feed_all(&[&bytes]), vec![Event::Key(key)], "{bytes:?}");
    }
}

#[test]
fn escape_zero_is_still_dropped() {
    assert_eq!(feed_all(&[b"\x1b0"]), Vec::new());
    // Kitty's ⌥0 and ⌥: are not digits one to nine.
    assert_eq!(feed_all(&[b"\x1b[48;3u"]), Vec::new());
    assert_eq!(feed_all(&[b"\x1b[58;3u"]), Vec::new());
}

#[test]
fn ctrl_f_and_cmd_f_parse() {
    assert_eq!(
        feed_all(&[&[0x06u8] as &[u8]]),
        vec![Event::Key(Key::CtrlF)]
    );
    assert_eq!(feed_all(&[b"\x1b[102;5u"]), vec![Event::Key(Key::CtrlF)]);
    assert_eq!(feed_all(&[b"\x1b[102;9u"]), vec![Event::Key(Key::CtrlF)]);
    // Alt+F stays a word move, and another modifier is no binding.
    assert_eq!(feed_all(&[b"\x1b[102;3u"]), edit(Edit::WordRight));
    assert!(feed_all(&[b"\x1b[102;13u"]).is_empty());
}

/// Legacy control bytes read as their strokes.
#[test]
fn legacy_control_bytes_name_strokes() {
    let ctrl = Mods::CTRL;
    let cases: &[(u8, Stroke)] = &[
        (0x00, modified(Code::Space, ctrl)),
        (0x01, modified(Code::Char('a'), ctrl)),
        (0x02, modified(Code::Char('b'), ctrl)),
        (0x03, modified(Code::Char('c'), ctrl)),
        (0x04, modified(Code::Char('d'), ctrl)),
        (0x05, modified(Code::Char('e'), ctrl)),
        (0x06, modified(Code::Char('f'), ctrl)),
        (0x07, modified(Code::Char('g'), ctrl)),
        (0x08, plain(Code::Backspace)),
        (0x09, plain(Code::Tab)),
        (0x0a, modified(Code::Char('j'), ctrl)),
        (0x0b, modified(Code::Char('k'), ctrl)),
        (0x0c, modified(Code::Char('l'), ctrl)),
        (0x0d, plain(Code::Enter)),
        (0x0e, modified(Code::Char('n'), ctrl)),
        (0x0f, modified(Code::Char('o'), ctrl)),
        (0x10, modified(Code::Char('p'), ctrl)),
        (0x11, modified(Code::Char('q'), ctrl)),
        (0x12, modified(Code::Char('r'), ctrl)),
        (0x13, modified(Code::Char('s'), ctrl)),
        (0x14, modified(Code::Char('t'), ctrl)),
        (0x15, modified(Code::Char('u'), ctrl)),
        (0x16, modified(Code::Char('v'), ctrl)),
        (0x17, modified(Code::Char('w'), ctrl)),
        (0x18, modified(Code::Char('x'), ctrl)),
        (0x19, modified(Code::Char('y'), ctrl)),
        (0x1a, modified(Code::Char('z'), ctrl)),
        (0x1b, plain(Code::Esc)),
        (0x1c, modified(Code::Char('\\'), ctrl)),
        (0x1d, modified(Code::Char(']'), ctrl)),
        (0x1e, modified(Code::Char('^'), ctrl)),
        (0x1f, modified(Code::Char('_'), ctrl)),
        (0x7f, plain(Code::Backspace)),
    ];
    for (byte, want) in cases {
        assert_eq!(
            feed_strokes(&[&[*byte] as &[u8]]),
            vec![*want],
            "{byte:#04x}"
        );
    }
}

/// `ESC` with a byte in the same read reads as Alt with that key's stroke.
#[test]
fn esc_with_a_byte_names_alt_with_that_stroke() {
    let alt = Mods::ALT;
    let cases: &[(&[u8], Option<Stroke>)] = &[
        (b"\x1ba", Some(modified(Code::Char('a'), alt))),
        (b"\x1bA", Some(modified(Code::Char('a'), alt | Mods::SHIFT))),
        (b"\x1b\x7f", Some(modified(Code::Backspace, alt))),
        (
            b"\x1b\x03",
            Some(modified(Code::Char('c'), alt | Mods::CTRL)),
        ),
        (b"\x1b\x00", Some(modified(Code::Space, alt | Mods::CTRL))),
        (b"\x1b ", Some(modified(Code::Space, alt))),
        (b"\x1b0", Some(modified(Code::Char('0'), alt))),
        (b"\x1b\xc3\xa9", Some(modified(Code::Char('\u{e9}'), alt))),
        // A modified arrow under the second ESC is no Alt arrow.
        (b"\x1b\x1b[1;5A", None),
        // A character split off the ESC is held for the next read.
        (b"\x1b\xc3", None),
    ];
    for (bytes, want) in cases {
        assert_eq!(
            feed_strokes(&[*bytes]),
            (*want).into_iter().collect::<Vec<_>>(),
            "{bytes:?}"
        );
    }
}

/// `CSI` arrows, Home and End read plain and with `1;<modifiers>`.
#[test]
fn csi_letters_name_strokes() {
    let (shift, alt, ctrl, super_) = (Mods::SHIFT, Mods::ALT, Mods::CTRL, Mods::SUPER);
    let cases: &[(&[u8], Option<Stroke>)] = &[
        (b"\x1b[A", Some(plain(Code::Up))),
        (b"\x1b[B", Some(plain(Code::Down))),
        (b"\x1b[C", Some(plain(Code::Right))),
        (b"\x1b[D", Some(plain(Code::Left))),
        (b"\x1b[H", Some(plain(Code::Home))),
        (b"\x1b[F", Some(plain(Code::End))),
        (b"\x1b[1;1A", Some(plain(Code::Up))),
        (b"\x1b[1;2A", Some(modified(Code::Up, shift))),
        (b"\x1b[1;3A", Some(modified(Code::Up, alt))),
        (b"\x1b[1;4A", Some(modified(Code::Up, shift | alt))),
        (b"\x1b[1;5A", Some(modified(Code::Up, ctrl))),
        (b"\x1b[1;6A", Some(modified(Code::Up, shift | ctrl))),
        (b"\x1b[1;7A", Some(modified(Code::Up, alt | ctrl))),
        (b"\x1b[1;8A", Some(modified(Code::Up, shift | alt | ctrl))),
        (b"\x1b[1;9A", Some(modified(Code::Up, super_))),
        (b"\x1b[1;10A", Some(modified(Code::Up, shift | super_))),
        (b"\x1b[1;11A", Some(modified(Code::Up, alt | super_))),
        (
            b"\x1b[1;12A",
            Some(modified(Code::Up, shift | alt | super_)),
        ),
        (b"\x1b[1;13A", Some(modified(Code::Up, ctrl | super_))),
        // Hyper and meta name no stroke.
        (b"\x1b[1;17A", None),
        (b"\x1b[1;33A", None),
        // Locks change no binding, and a zero field reads as no modifiers.
        (b"\x1b[1;65A", Some(plain(Code::Up))),
        (b"\x1b[1;129A", Some(plain(Code::Up))),
        (b"\x1b[1;0A", Some(plain(Code::Up))),
        (b"\x1b[1;3C", Some(modified(Code::Right, alt))),
        (b"\x1b[1;5C", Some(modified(Code::Right, ctrl))),
        (b"\x1b[1;9C", Some(modified(Code::Right, super_))),
        (b"\x1b[1;9D", Some(modified(Code::Left, super_))),
        (b"\x1b[1;5H", Some(modified(Code::Home, ctrl))),
        (b"\x1b[1;3F", Some(modified(Code::End, alt))),
        // Not `1` before the semicolon, or no digits at all, is nothing.
        (b"\x1b[2;3C", None),
        (b"\x1b[1;+3C", None),
        (b"\x1b[5A", None),
        // A modified `CSI P` reads as nothing.
        (b"\x1b[1;2P", None),
    ];
    for (bytes, want) in cases {
        assert_eq!(
            feed_strokes(&[*bytes]),
            (*want).into_iter().collect::<Vec<_>>(),
            "{bytes:?}"
        );
    }
}

/// `CSI n~` reads as its key, plain and with `;<modifiers>`.
#[test]
fn csi_tilde_names_strokes() {
    let (shift, alt, ctrl, super_) = (Mods::SHIFT, Mods::ALT, Mods::CTRL, Mods::SUPER);
    let cases: &[(&[u8], Option<Stroke>)] = &[
        (b"\x1b[2~", Some(plain(Code::Insert))),
        (b"\x1b[3~", Some(plain(Code::Delete))),
        (b"\x1b[5~", Some(plain(Code::PageUp))),
        (b"\x1b[6~", Some(plain(Code::PageDown))),
        (b"\x1b[1~", Some(plain(Code::Home))),
        (b"\x1b[7~", Some(plain(Code::Home))),
        (b"\x1b[4~", Some(plain(Code::End))),
        (b"\x1b[8~", Some(plain(Code::End))),
        (b"\x1b[11~", Some(plain(Code::F(1)))),
        (b"\x1b[12~", Some(plain(Code::F(2)))),
        (b"\x1b[13~", Some(plain(Code::F(3)))),
        (b"\x1b[14~", Some(plain(Code::F(4)))),
        (b"\x1b[15~", Some(plain(Code::F(5)))),
        (b"\x1b[17~", Some(plain(Code::F(6)))),
        (b"\x1b[18~", Some(plain(Code::F(7)))),
        (b"\x1b[19~", Some(plain(Code::F(8)))),
        (b"\x1b[20~", Some(plain(Code::F(9)))),
        (b"\x1b[21~", Some(plain(Code::F(10)))),
        (b"\x1b[23~", Some(plain(Code::F(11)))),
        (b"\x1b[24~", Some(plain(Code::F(12)))),
        // Numbers outside the list read as nothing.
        (b"\x1b[9~", None),
        (b"\x1b[10~", None),
        (b"\x1b[16~", None),
        (b"\x1b[22~", None),
        (b"\x1b[25~", None),
        (b"\x1b[0~", None),
        (b"\x1b[3;5~", Some(modified(Code::Delete, ctrl))),
        (b"\x1b[5;2~", Some(modified(Code::PageUp, shift))),
        (b"\x1b[11;3~", Some(modified(Code::F(1), alt))),
        (b"\x1b[15;9~", Some(modified(Code::F(5), super_))),
        (b"\x1b[3;65~", Some(plain(Code::Delete))),
        (b"\x1b[3;17~", None),
        (b"\x1b[3;+5~", None),
    ];
    for (bytes, want) in cases {
        assert_eq!(
            feed_strokes(&[*bytes]),
            (*want).into_iter().collect::<Vec<_>>(),
            "{bytes:?}"
        );
    }
}

/// `SS3` letters read as their strokes.
#[test]
fn ss3_names_strokes() {
    let cases: &[(&[u8], Stroke)] = &[
        (b"\x1bOA", plain(Code::Up)),
        (b"\x1bOB", plain(Code::Down)),
        (b"\x1bOC", plain(Code::Right)),
        (b"\x1bOD", plain(Code::Left)),
        (b"\x1bOH", plain(Code::Home)),
        (b"\x1bOF", plain(Code::End)),
        (b"\x1bOP", plain(Code::F(1))),
        (b"\x1bOQ", plain(Code::F(2))),
        (b"\x1bOR", plain(Code::F(3))),
        (b"\x1bOS", plain(Code::F(4))),
    ];
    for (bytes, want) in cases {
        assert_eq!(feed_strokes(&[*bytes]), vec![*want], "{bytes:?}");
    }
}

/// Kitty `CSI code[;modifiers] u` reads as its stroke.
#[test]
fn kitty_codes_name_strokes() {
    let (shift, alt, ctrl, super_) = (Mods::SHIFT, Mods::ALT, Mods::CTRL, Mods::SUPER);
    let cases: &[(&[u8], Option<Stroke>)] = &[
        (b"\x1b[13u", Some(plain(Code::Enter))),
        (b"\x1b[13;1u", Some(plain(Code::Enter))),
        (b"\x1b[13;2u", Some(modified(Code::Enter, shift))),
        (b"\x1b[27u", Some(plain(Code::Esc))),
        (b"\x1b[127u", Some(plain(Code::Backspace))),
        (b"\x1b[127;3u", Some(modified(Code::Backspace, alt))),
        (b"\x1b[9u", Some(plain(Code::Tab))),
        (b"\x1b[9;2u", Some(modified(Code::Tab, shift))),
        (b"\x1b[32u", Some(plain(Code::Space))),
        (b"\x1b[32;5u", Some(modified(Code::Space, ctrl))),
        (b"\x1b[99u", Some(plain(Code::Char('c')))),
        (b"\x1b[103u", Some(plain(Code::Char('g')))),
        (b"\x1b[111u", Some(plain(Code::Char('o')))),
        (b"\x1b[114u", Some(plain(Code::Char('r')))),
        (b"\x1b[120u", Some(plain(Code::Char('x')))),
        (b"\x1b[99;5u", Some(modified(Code::Char('c'), ctrl))),
        (b"\x1b[102;9u", Some(modified(Code::Char('f'), super_))),
        // An uppercase code arrives with shift held.
        (b"\x1b[65u", Some(modified(Code::Char('a'), shift))),
        (b"\x1b[97;2u", Some(modified(Code::Char('a'), shift))),
        (b"\x1b[65;2u", Some(modified(Code::Char('a'), shift))),
        // Shift on a character that is not a letter is dropped.
        (b"\x1b[63;2u", Some(plain(Code::Char('?')))),
        (b"\x1b[48u", Some(plain(Code::Char('0')))),
        (b"\x1b[43;5u", Some(modified(Code::Char('+'), ctrl))),
        (b"\x1b[233u", Some(plain(Code::Char('\u{e9}')))),
        (b"\x1b[233;2u", Some(modified(Code::Char('\u{e9}'), shift))),
        // Private-use codes, hyper and meta, controls and surrogates read
        // as nothing; locks and sub-fields change nothing.
        (b"\x1b[57344u", None),
        (b"\x1b[57441u", None),
        (b"\x1b[99;17u", None),
        (b"\x1b[13;17u", None),
        (b"\x1b[99;65u", Some(plain(Code::Char('c')))),
        (b"\x1b[99:67;5:1u", Some(modified(Code::Char('c'), ctrl))),
        (b"\x1b[55296u", None),
        (b"\x1b[5u", None),
        (b"\x1b[15u", None),
    ];
    for (bytes, want) in cases {
        assert_eq!(
            feed_strokes(&[*bytes]),
            (*want).into_iter().collect::<Vec<_>>(),
            "{bytes:?}"
        );
    }
}

/// Characters read as their strokes: uppercase with shift, space as `Space`.
#[test]
fn characters_name_strokes() {
    let cases: &[(&[u8], Stroke)] = &[
        (b"a", plain(Code::Char('a'))),
        (b"A", modified(Code::Char('a'), Mods::SHIFT)),
        (b" ", plain(Code::Space)),
        ("\u{e9}".as_bytes(), plain(Code::Char('\u{e9}'))),
        (
            "\u{c9}".as_bytes(),
            modified(Code::Char('\u{e9}'), Mods::SHIFT),
        ),
        ("\u{4e2d}".as_bytes(), plain(Code::Char('\u{4e2d}'))),
        // No single uppercase, so no shift: `ß` stays `ß`, `ẞ` stays `ẞ`.
        ("\u{df}".as_bytes(), plain(Code::Char('\u{df}'))),
        ("\u{1e9e}".as_bytes(), plain(Code::Char('\u{1e9e}'))),
        (b"?", plain(Code::Char('?'))),
    ];
    for (bytes, want) in cases {
        assert_eq!(feed_strokes(&[*bytes]), vec![*want], "{bytes:?}");
    }
}

/// The space bar reads as a stroke that types a space.
#[test]
fn space_types_a_space() {
    assert_eq!(feed_strokes(&[b" "]), vec![plain(Code::Space)]);
    assert_eq!(
        default_event(&plain(Code::Space)),
        Some(Event::Key(Key::Char(' ')))
    );
}

/// Every bound stroke maps to its key or edit, and anything else to nothing.
#[test]
fn default_event_matches_the_parity_table() {
    let key = |key: Key| Some(Event::Key(key));
    let edit = |edit: Edit| Some(Event::Edit(edit));
    let (shift, alt, ctrl, super_) = (Mods::SHIFT, Mods::ALT, Mods::CTRL, Mods::SUPER);
    let cases: &[(Stroke, Option<Event>)] = &[
        (plain(Code::Enter), key(Key::Enter)),
        (plain(Code::Esc), key(Key::Esc)),
        (plain(Code::Backspace), key(Key::Backspace)),
        (plain(Code::Tab), key(Key::Tab)),
        (modified(Code::Tab, shift), key(Key::BackTab)),
        (plain(Code::Space), key(Key::Char(' '))),
        (plain(Code::PageUp), key(Key::PageUp)),
        (plain(Code::PageDown), key(Key::PageDown)),
        (plain(Code::End), key(Key::End)),
        (plain(Code::Up), key(Key::Up)),
        (plain(Code::Down), key(Key::Down)),
        (plain(Code::F(1)), key(Key::F1)),
        (modified(Code::Char('c'), ctrl), key(Key::CtrlC)),
        (modified(Code::Char('o'), ctrl), key(Key::CtrlO)),
        (modified(Code::Char('g'), ctrl), key(Key::CtrlG)),
        (modified(Code::Char('r'), ctrl), key(Key::CtrlR)),
        (modified(Code::Char('f'), ctrl), key(Key::CtrlF)),
        (modified(Code::Char('f'), super_), key(Key::CtrlF)),
        (modified(Code::Char('j'), ctrl), edit(Edit::CtrlJ)),
        (modified(Code::Char('a'), alt), key(Key::AltA)),
        (modified(Code::Char('x'), alt), key(Key::AltX)),
        (modified(Code::Up, alt), key(Key::AltUp)),
        (modified(Code::Down, alt), key(Key::AltDown)),
        (modified(Code::Char('p'), alt), key(Key::AltP)),
        (modified(Code::Char('r'), alt), key(Key::AltR)),
        (modified(Code::Char('1'), alt), key(Key::AltDigit(1))),
        (modified(Code::Char('5'), alt), key(Key::AltDigit(5))),
        (modified(Code::Char('9'), alt), key(Key::AltDigit(9))),
        (plain(Code::Char('a')), key(Key::Char('a'))),
        (plain(Code::Char('\u{e9}')), key(Key::Char('\u{e9}'))),
        (plain(Code::Char('?')), key(Key::Char('?'))),
        (plain(Code::Char('0')), key(Key::Char('0'))),
        (plain(Code::Char('5')), key(Key::Char('5'))),
        (plain(Code::Char('\u{df}')), key(Key::Char('\u{df}'))),
        (plain(Code::Char('\u{1e9e}')), key(Key::Char('\u{1e9e}'))),
        (modified(Code::Char('a'), shift), key(Key::Char('A'))),
        (
            modified(Code::Char('\u{e9}'), shift),
            key(Key::Char('\u{c9}')),
        ),
        (plain(Code::Left), edit(Edit::Left)),
        (plain(Code::Right), edit(Edit::Right)),
        (modified(Code::Enter, shift), edit(Edit::ShiftEnter)),
        (modified(Code::Left, alt), edit(Edit::WordLeft)),
        (modified(Code::Left, ctrl), edit(Edit::WordLeft)),
        (modified(Code::Char('b'), alt), edit(Edit::WordLeft)),
        (modified(Code::Right, alt), edit(Edit::WordRight)),
        (modified(Code::Right, ctrl), edit(Edit::WordRight)),
        (modified(Code::Char('f'), alt), edit(Edit::WordRight)),
        (modified(Code::Backspace, alt), edit(Edit::DeleteWord)),
        (modified(Code::Left, super_), edit(Edit::LineStart)),
        (modified(Code::Right, super_), edit(Edit::LineEnd)),
        (plain(Code::Delete), edit(Edit::Delete)),
        // Anything else reads as nothing.
        (modified(Code::Char('a'), ctrl), None),
        (modified(Code::Space, ctrl), None),
        (modified(Code::Char('o'), alt), None),
        (modified(Code::Space, shift), None),
        (plain(Code::F(2)), None),
        (plain(Code::Insert), None),
        (plain(Code::Home), None),
        (modified(Code::Tab, ctrl), None),
        (modified(Code::Up, ctrl), None),
        (modified(Code::Enter, alt), None),
        (modified(Code::Char('s'), super_), None),
        (modified(Code::Char('0'), alt), None),
        (modified(Code::Tab, alt), None),
        (
            modified(Code::Char('\u{130}'), shift),
            key(Key::Char('\u{130}')),
        ),
        // A shifted letter with no single uppercase reads as nothing.
        (modified(Code::Char('\u{149}'), shift), None),
        (modified(Code::Char('+'), ctrl), None),
    ];
    for (stroke, want) in cases {
        assert_eq!(&default_event(stroke), want, "{stroke:?}");
    }
}
