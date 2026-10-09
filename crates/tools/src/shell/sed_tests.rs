#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use proptest::prelude::*;

use super::{files, prints_only};

fn assert_prints(script: &str) {
    assert!(prints_only(script), "rejected: {script:?}");
}

fn assert_closed_script(script: &str) {
    assert!(!prints_only(script), "reads: {script:?}");
}

#[test]
fn print_only_scripts_read() {
    for script in [
        "320,520p",
        "1,120p",
        "$p",
        "p",
        "0p",
        "9p",
        "12p",
        "10,/re/p",
        "1,$p",
        "/fn main/,/^}/p",
        "/a\\/b/p",
        "/a\\\\/p",
        "//p",
        " 1p",
        "1 p",
        "1\tp",
        "1p ",
        "1p;5p",
        "1p ; 5p",
        "1p\n5p",
        "1p;;2p",
        "1p;",
        "\n1p\n",
    ] {
        assert_prints(script);
    }
}

#[test]
fn hostile_scripts_do_not_print_only() {
    for script in [
        "1w out",
        "1W out",
        "1p;w out",
        "1pw out",
        "1p w out",
        "1p\nw out",
        "1e id",
        "e id",
        "s/x/id/e",
        "s/a/b/",
        "s/a/b/w out",
        "y/a/b/",
        "1r /etc/passwd",
        "1R x",
        "1{p}",
        "1{p;w out}",
        "1d",
        "1q",
        "1=",
        "1l",
        "/x/!p",
        "\\%a%p",
        "/a/Ip",
        "/a/Mp",
        "0~3p",
        "1,+3p",
        "1,~4p",
        "/a\\\\/w x/p",
        "/a/w x",
        "/abc",
        "/a\\",
        "/a\nb/p",
        "1,p",
        ",1p",
        "1 ,2p",
        "1, 2p",
        "1x",
        "1pp",
        "ap",
        "1,2,3p",
        "",
        ";",
        " ",
        "#n",
        "1p # c",
        "0,/re/p",
        "00,/re/p",
        "0,5p",
    ] {
        assert_closed_script(script);
    }
}

#[test]
fn flag_cases_declare_only_files() {
    let files_of = |words: &[&str]| files(words.iter().copied());
    for words in [
        vec!["-n", "1p", "f"],
        vec!["-ne1p", "f"],
        vec!["-ne", "1p", "f"],
        vec!["-nE", "1p", "f"],
        vec!["-nr", "1p", "f"],
        vec!["-n", "-e", "1p", "f"],
        vec!["-e", "1p", "-e", "$p", "f"],
    ] {
        assert_eq!(files_of(&words), Some(vec!["f".to_owned()]), "{words:?}");
    }
    for words in [
        vec!["f", "-e", "1p"],
        vec!["-e", "p", "f", "-e", "/secret/p"],
        vec!["1p", "f", "-n"],
        vec!["-n", "1p", "f", "-E"],
        vec!["1p", "f", "--"],
        vec!["-i", "1p", "f"],
        vec!["-i.bak", "-n", "1p", "f"],
        vec!["-ni", "1p", "f"],
        vec!["-n", "-i", "1p", "f"],
        vec!["-I", "", "1p", "f"],
        vec!["--in-place", "1p", "f"],
        vec!["--in-place=.bak", "1p", "f"],
        vec!["--expression=1p", "f"],
        vec!["--quiet", "1p", "f"],
        vec!["--posix", "-n", "1p", "f"],
        vec!["--debug", "1p", "f"],
        vec!["-f", "s", "f"],
        vec!["-s", "-n", "1p", "f"],
        vec!["-z", "1p", "f"],
        vec!["-u", "1p", "f"],
        vec!["-l", "5", "1p", "f"],
        vec!["-", "1p"],
        vec!["-n", "-e", "1p", "-e", "w out", "f"],
        vec!["-n", "-e", "w out", "1p"],
        vec!["-ne", "w x", "f"],
        vec!["-en", "1p", "f"],
        vec!["-n", "-e"],
        vec!["-n"],
        vec!["-n", "--", "-i", "f"],
        vec!["-n", "0,/re/p", "f"],
        vec!["-n", "00,/re/p", "f"],
        vec!["-n", "0,5p", "f"],
        vec!["-n", "1p", "f", "-e", "w out"],
        vec!["-n", "1p", "f", "-i"],
    ] {
        assert_eq!(files_of(&words), None, "{words:?}");
    }
    assert_eq!(files_of(&["1p"]), Some(Vec::new()));
    assert_eq!(
        files_of(&["-n", "--", "1p", "-n"]),
        Some(vec!["-n".to_owned()])
    );
    assert_eq!(
        files_of(&["-n", "1p", "a", "b"]),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
}

/// `script` with every `/.../` body removed, scanning as the parser does.
fn strip_regex_bodies(script: &str) -> String {
    let mut out = String::new();
    let mut chars = script.chars();
    while let Some(ch) = chars.next() {
        if ch != '/' {
            out.push(ch);
            continue;
        }
        let mut closed = false;
        loop {
            match chars.next() {
                Some('/') => {
                    closed = true;
                    break;
                }
                Some('\\') => {
                    if chars.next().is_none() {
                        break;
                    }
                }
                Some(_) => {}
                None => break,
            }
        }
        out.push_str(if closed { "//" } else { "/" });
    }
    out
}

fn alphabet() -> Vec<char> {
    let mut chars: Vec<char> = ('0'..='9').collect();
    chars.extend([
        '$', '/', '\\', ',', 'p', ';', '\n', ' ', '\t', 'w', 'W', 'e', 'E', 'r', 'R', 's', 'y',
        'i', 'I', 'q', 'd', 'x', '{', '}', '!', '#', '~', '+', '%',
    ]);
    chars
}

proptest! {
    /// `prints_only` always returns, and an accepted script with every
    /// `/.../` body removed holds no letter but `p`.
    #[test]
    fn arbitrary_strings_terminate_and_accepted_ones_hold_only_p(
        script in prop::collection::vec(prop::sample::select(alphabet()), 0..16)
            .prop_map(|chars| chars.into_iter().collect::<String>())
    ) {
        let accepted = prints_only(&script);
        if accepted {
            let stripped = strip_regex_bodies(&script);
            for ch in stripped.chars() {
                prop_assert!(
                    !ch.is_ascii_alphabetic() || ch == 'p',
                    "accepted {script:?} holds {ch:?} outside regex"
                );
            }
        }
    }

    /// Scripts generated from the grammar are always accepted.
    #[test]
    fn generated_scripts_always_print(script in generated_script()) {
        prop_assert!(prints_only(&script), "rejected: {script:?}");
    }
}

fn generated_script() -> impl Strategy<Value = String> {
    let digit = prop::sample::select(vec!['1', '2', '3', '4', '5', '6', '7', '8', '9', '0']);
    let body_piece = prop_oneof![
        prop::char::range('a', 'z').prop_map(|ch| ch.to_string()),
        Just("\\/".to_owned()),
        Just("\\\\".to_owned()),
    ];
    let regex =
        prop::collection::vec(body_piece, 0..4).prop_map(|pieces| format!("/{}/", pieces.concat()));
    let address = prop_oneof![
        prop::collection::vec(digit, 1..3)
            .prop_map(|digits| digits.into_iter().collect::<String>()),
        Just("$".to_owned()),
        regex,
    ];
    let blank = prop_oneof![
        Just(String::new()),
        Just(" ".to_owned()),
        Just("\t".to_owned()),
    ];
    let command =
        (prop::collection::vec(address, 0..3), blank.clone()).prop_map(|(mut addresses, gap)| {
            // A zero-start range is GNU's `0,/re/` form, never a print.
            if addresses.len() == 2 && addresses[0].chars().all(|ch| ch == '0') {
                addresses.remove(0);
            }
            format!("{}{gap}p", addresses.join(","))
        });
    let separator = prop_oneof![Just(";".to_owned()), Just("\n".to_owned())];
    (
        prop::collection::vec(command, 1..5),
        prop::collection::vec((separator, blank.clone()), 0..4),
        blank,
    )
        .prop_map(|(commands, mut joins, trailing)| {
            let mut script = String::new();
            for (index, command) in commands.into_iter().enumerate() {
                if index > 0 {
                    if let Some((sep, gap)) = joins.pop() {
                        script.push_str(&sep);
                        script.push_str(&gap);
                    } else {
                        script.push(';');
                    }
                }
                script.push_str(&command);
            }
            script.push_str(&trailing);
            script
        })
}
