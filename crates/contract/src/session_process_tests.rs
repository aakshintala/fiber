//! The session-host argv predicate: the `fiber` executable with exactly one
//! of the session-hosting subcommands, and nothing else.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use super::is_session_process;

fn argv(words: &[&[u8]]) -> Vec<Vec<u8>> {
    words.iter().map(|word| word.to_vec()).collect()
}

#[test]
fn fiber_with_each_host_subcommand_is_a_session_process() {
    for sub in [
        b"session".as_slice(),
        b"ask".as_slice(),
        b"extension-case".as_slice(),
    ] {
        assert!(
            is_session_process(&argv(&[b"fiber".as_slice(), sub, b"--id=s_1".as_slice()])),
            "{sub:?}"
        );
        assert!(
            is_session_process(&argv(&[b"/usr/local/bin/fiber".as_slice(), sub])),
            "{sub:?}"
        );
    }
}

#[test]
fn other_executables_and_subcommands_are_not_session_processes() {
    for words in [
        [].as_slice(),
        [b"fiber".as_slice()].as_slice(),
        [b"fiber".as_slice(), b"sessionx"].as_slice(),
        [b"fiber".as_slice(), b"sessions"].as_slice(),
        [b"fiber".as_slice(), b"ask-user"].as_slice(),
        [b"fiber".as_slice(), b"extension"].as_slice(),
        [b"fiber".as_slice(), b"extension-case-x"].as_slice(),
        [b"myfiber".as_slice(), b"session"].as_slice(),
        [b"fiber-backup".as_slice(), b"ask"].as_slice(),
        [b"sh".as_slice(), b"session"].as_slice(),
        [b"python3".as_slice(), b"ask"].as_slice(),
    ] {
        assert!(!is_session_process(&argv(words)), "{words:?}");
    }
}

#[test]
fn non_utf8_argv_compares_by_bytes() {
    assert!(is_session_process(&argv(&[
        b"/bin/fiber".as_slice(),
        b"session".as_slice(),
        [0xff, 0xfe].as_slice(),
    ])));
    assert!(!is_session_process(&argv(&[
        b"fib\xffr".as_slice(),
        b"session".as_slice(),
    ])));
    assert!(!is_session_process(&argv(&[
        b"fiber".as_slice(),
        b"sessio\xff".as_slice(),
    ])));
}

#[test]
fn an_executable_path_matches_by_basename_only() {
    assert!(is_session_process(&argv(&[
        b"/a/b/fiber".as_slice(),
        b"ask".as_slice(),
    ])));
    assert!(!is_session_process(&argv(&[
        b"/a/fiber/b".as_slice(),
        b"ask".as_slice(),
    ])));
    assert!(!is_session_process(&argv(&[
        b"".as_slice(),
        b"ask".as_slice()
    ])));
}
