//! The unpacker against streams built with `fakes::ustar`: what lands on
//! disk, and every refusal, which writes nothing outside `into`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use fakes::ustar::{archive, checksum, gzip, header};

use super::{LIMITS, Limits, unpack};
use crate::Error;

/// A temporary tree holding `into`, the directory unpacked into, and an
/// empty `sentinel` beside it that no refusal may write to.
struct Out {
    held: fakes::TempDir,
}

impl Out {
    fn new() -> Self {
        let held = fakes::TempDir::new("fiber-unpack");
        fs::create_dir(held.path().join("into")).unwrap();
        fs::create_dir(held.path().join("sentinel")).unwrap();
        Self { held }
    }

    fn target(&self) -> PathBuf {
        self.held.path().join("into")
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.target().join(rel)
    }

    /// Nothing beside `into` changed: the root holds only `into` and an
    /// empty `sentinel`.
    fn outside_untouched(&self) {
        let mut names: Vec<String> = fs::read_dir(self.held.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["into", "sentinel"]);
        let sentinel = self.held.path().join("sentinel");
        assert_eq!(fs::read_dir(sentinel).unwrap().count(), 0);
    }
}

fn file(name: &str, data: &[u8]) -> [u8; 512] {
    header(name, b'0', data.len() as u64, 0o644, "")
}

fn dir(name: &str) -> [u8; 512] {
    header(name, b'5', 0, 0o755, "")
}

fn link(name: &str, target: &str) -> [u8; 512] {
    header(name, b'2', 0, 0o777, target)
}

/// `unpack` on its own thread, failing the test after ten seconds: a loop
/// that stops making progress fails the test rather than hanging it.
fn timed(gz: &[u8], into: &Path, name: &str, limits: &Limits) -> Result<(), Error> {
    let (gz, into, name) = (gz.to_vec(), into.to_path_buf(), name.to_owned());
    let limits = Limits {
        bytes: limits.bytes,
        members: limits.members,
    };
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        send.send(unpack(&gz, &into, &name, &limits)).unwrap_or(());
    });
    receive
        .recv_timeout(Duration::from_secs(10))
        .expect("unpack returns within ten seconds")
}

fn run(tar: &[u8], limits: &Limits) -> (Out, Result<(), Error>) {
    let out = Out::new();
    let result = timed(&gzip(tar), &out.target(), "fixture.tar.gz", limits);
    (out, result)
}

fn ok(members: &[([u8; 512], &[u8])]) -> Out {
    let (out, result) = run(&archive(members), &LIMITS);
    result.unwrap();
    out.outside_untouched();
    out
}

/// Unpacking `tar` is refused with a message holding `why`, and nothing
/// outside `into` changed.
fn refused_tar(tar: &[u8], limits: &Limits, why: &str) -> Out {
    let (out, result) = run(tar, limits);
    let err = result.unwrap_err();
    assert!(matches!(err, Error::BadArchive { .. }), "{err:?}");
    let text = err.to_string();
    assert!(text.starts_with("`fixture.tar.gz`: "), "{text}");
    assert!(text.contains(why), "{text} lacks {why}");
    out.outside_untouched();
    out
}

fn refused(members: &[([u8; 512], &[u8])], why: &str) -> Out {
    refused_tar(&archive(members), &LIMITS, why)
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_file_a_directory_and_a_symlink_land() {
    // The file's 5 bytes are padded to 512, so the next header is read at
    // the right place.
    let out = ok(&[
        (file("a.txt", b"hello"), b"hello"),
        (dir("d/"), b""),
        (link("l", "a.txt"), b""),
    ]);
    assert_eq!(fs::read(out.at("a.txt")).unwrap(), b"hello");
    assert!(fs::symlink_metadata(out.at("d")).unwrap().is_dir());
    assert_eq!(fs::read_link(out.at("l")).unwrap(), Path::new("a.txt"));
}

#[test]
fn a_file_of_whole_blocks_is_followed_by_the_next_header() {
    let data = vec![b'x'; 1024];
    let out = ok(&[(file("big", &data), &data), (file("after", b"1"), b"1")]);
    assert_eq!(fs::read(out.at("big")).unwrap(), data);
    assert_eq!(fs::read(out.at("after")).unwrap(), b"1");
}

#[test]
fn the_prefix_field_is_joined_to_the_name() {
    let long = format!("{}/{}", "p".repeat(120), "n".repeat(90));
    let out = ok(&[(file(&long, b"x"), b"x")]);
    assert_eq!(fs::read(out.at(&long)).unwrap(), b"x");
}

#[test]
fn dot_and_trailing_slash_components_are_dropped() {
    let out = ok(&[
        (dir("./"), b""),
        (file("./x", b"1"), b"1"),
        (dir("y/"), b""),
        (file("y/./z", b"2"), b"2"),
    ]);
    assert_eq!(fs::read(out.at("x")).unwrap(), b"1");
    assert_eq!(fs::read(out.at("y/z")).unwrap(), b"2");
}

#[test]
fn implicit_parents_are_made_with_mode_0700() {
    let out = ok(&[(file("p/q/r", b"1"), b"1")]);
    assert_eq!(fs::read(out.at("p/q/r")).unwrap(), b"1");
    assert_eq!(mode(&out.at("p")), 0o700);
    assert_eq!(mode(&out.at("p/q")), 0o700);
}

#[test]
fn modes_come_from_the_owner_bits_not_the_header_or_umask() {
    let member = |name: &str, kind: u8, m: u32| header(name, kind, 0, m, "");
    let out = ok(&[
        (member("all", b'0', 0o777), b""),
        (member("plain", b'0', 0o644), b""),
        (member("owner-x", b'0', 0o744), b""),
        (member("group-x", b'0', 0o654), b""),
        (member("d", b'5', 0o755), b""),
    ]);
    for (name, want) in [
        ("all", 0o700),
        ("plain", 0o600),
        ("owner-x", 0o700),
        ("group-x", 0o600),
        ("d", 0o700),
    ] {
        assert_eq!(mode(&out.at(name)), want, "{name}");
    }
}

#[test]
fn an_existing_directory_entry_is_accepted() {
    let out = ok(&[(file("d/a", b"1"), b"1"), (dir("d/"), b"")]);
    assert_eq!(fs::read(out.at("d/a")).unwrap(), b"1");
}

#[test]
fn a_size_ending_in_a_space_or_nul_is_octal() {
    let mut block = file("a", b"abcde");
    block[124..136].copy_from_slice(b"00000000005 ");
    checksum(&mut block);
    let out = ok(&[(block, b"abcde")]);
    assert_eq!(fs::read(out.at("a")).unwrap(), b"abcde");
}

#[test]
fn a_header_without_ustar_magic_and_version_is_refused() {
    for (magic, version) in [
        (&b"\0\0\0\0\0\0"[..], &b"00"[..]),
        (b"ustar\0", b"01"),
        (b"ustar ", b" \0"),
        (b"ustaR\0", b"00"),
    ] {
        let mut block = file("a", b"");
        block[257..263].copy_from_slice(magic);
        block[263..265].copy_from_slice(version);
        checksum(&mut block);
        refused(&[(block, b"")], "has no ustar magic and version 00");
    }
}

#[test]
fn a_header_whose_checksum_does_not_match_is_refused() {
    let mut block = file("a", b"");
    block[0] = b'b';
    refused(&[(block, b"")], "checksum that does not match");
}

#[test]
fn a_size_or_mode_that_is_not_octal_is_refused() {
    let mut size = file("a", b"");
    size[124..136].copy_from_slice(b"0000000000x\0");
    checksum(&mut size);
    refused(&[(size, b"")], "size that is not octal");
    let mut eight = file("a", b"");
    eight[124..136].copy_from_slice(b"00000000008\0");
    checksum(&mut eight);
    refused(&[(eight, b"")], "size that is not octal");
    let mut empty = file("a", b"");
    empty[124..136].copy_from_slice(&[0; 12]);
    checksum(&mut empty);
    refused(&[(empty, b"")], "size that is not octal");
    let mut mode = file("a", b"");
    mode[100..108].copy_from_slice(b"00006z4\0");
    checksum(&mut mode);
    refused(&[(mode, b"")], "mode that is not octal");
}

#[test]
fn a_symlink_or_directory_with_a_size_is_refused() {
    let tar = archive(&[(header("l", b'2', 1, 0o777, "a"), b"x")]);
    refused_tar(&tar, &LIMITS, "`l` is a directory or symlink with a size");
    let tar = archive(&[(header("d", b'5', 1, 0o755, ""), b"x")]);
    refused_tar(&tar, &LIMITS, "`d` is a directory or symlink with a size");
}

#[test]
fn a_stream_that_ends_early_is_refused() {
    let full = archive(&[(file("a", &[b'x'; 1000]), &[b'x'; 1000])]);
    refused_tar(&full[..100], &LIMITS, "ends inside a header");
    refused_tar(&full[..512 + 600], &LIMITS, "ends inside `a`");
    // The data, whole, then no end blocks.
    refused_tar(
        &full[..512 + 1024],
        &LIMITS,
        "ends before its two end blocks",
    );
    // One zero block, then the end: not looped on.
    refused_tar(
        &full[..512 + 1024 + 512],
        &LIMITS,
        "ends before its two end blocks",
    );
    refused_tar(&full[..512 + 1024 + 100], &LIMITS, "ends inside a header");
}

#[test]
fn a_stream_that_ends_inside_a_file_of_whole_blocks_is_refused_there() {
    // No padding follows a 512-byte file, so only the data read sees it end.
    let full = archive(&[(file("a", &[b'x'; 512]), &[b'x'; 512])]);
    refused_tar(&full[..512 + 100], &LIMITS, "ends inside `a`");
    refused_tar(&full[..512 + 511], &LIMITS, "ends inside `a`");
}

#[test]
fn a_stream_that_ends_inside_a_files_padding_is_refused_there() {
    // The data is whole and the padding is not.
    let full = archive(&[(file("a", b"hello"), b"hello")]);
    refused_tar(&full[..512 + 5], &LIMITS, "ends inside `a`");
    refused_tar(&full[..1023], &LIMITS, "ends inside `a`");
}

#[test]
fn the_release_limits_are_256_mib_and_100_000_members() {
    assert_eq!(LIMITS.bytes, 268_435_456);
    assert_eq!(LIMITS.members, 100_000);
}

#[test]
fn data_after_one_zero_block_is_refused() {
    let mut tar = archive(&[(file("a", b"1"), b"1")]);
    tar.truncate(1024 + 512);
    tar.extend_from_slice(&file("b", b""));
    tar.extend_from_slice(&[0; 1024]);
    refused_tar(&tar, &LIMITS, "data follows the first end block");
}

#[test]
fn every_type_but_file_directory_and_symlink_is_refused() {
    for kind in *b"13467xgLKSA" {
        let why = format!("`m` has type `{}`", char::from(kind));
        refused(&[(header("m", kind, 0, 0o644, "a"), b"")], &why);
    }
}

#[test]
fn a_nul_type_is_a_file() {
    let out = ok(&[(header("a", 0, 1, 0o644, ""), b"1")]);
    assert_eq!(fs::read(out.at("a")).unwrap(), b"1");
}

#[test]
fn a_name_that_leaves_into_is_refused() {
    for (name, why) in [
        ("", "has an empty name"),
        (".", "has an empty name"),
        ("/abs", "is absolute"),
        ("../x", "has a `..` component"),
        ("a/../../x", "has a `..` component"),
        ("a/..", "has a `..` component"),
    ] {
        refused(&[(file(name, b""), b"")], why);
    }
    refused(&[(link("", "a"), b"")], "has an empty name");
    refused(&[(dir("/abs/"), b"")], "is absolute");
}

#[test]
fn a_link_target_that_leaves_its_directory_is_refused() {
    for (target, why) in [
        ("", "has an empty link target"),
        ("/etc", "has an absolute link target"),
        ("..", "has a link target with `..`"),
        ("a/../outside", "has a link target with `..`"),
    ] {
        refused(&[(link("l", target), b"")], why);
    }
}

#[test]
fn a_chain_of_links_out_of_the_tree_is_refused_at_its_first_link() {
    let out = refused(
        &[
            (dir("sub/"), b""),
            (link("sub/a", ".."), b""),
            (link("sub/b", "a/../outside"), b""),
        ],
        "`sub/a` has a link target with `..`",
    );
    assert!(fs::symlink_metadata(out.at("sub/a")).is_err());
}

#[test]
fn a_member_whose_mode_denies_the_owner_is_refused() {
    refused(
        &[(header("d", b'5', 0, 0o555, ""), b"")],
        "`d` is a directory without owner rwx",
    );
    refused(
        &[(header("d", b'5', 0, 0o600, ""), b"")],
        "`d` is a directory without owner rwx",
    );
    refused(
        &[(header("f", b'0', 0, 0o444, ""), b"")],
        "`f` is a file without owner rw",
    );
    refused(
        &[(header("f", b'0', 0, 0o244, ""), b"")],
        "`f` is a file without owner rw",
    );
}

#[test]
fn a_pax_header_is_refused_before_the_member_it_hides() {
    let smuggled = file("../escape", b"");
    refused(
        &[
            (header("pax", b'x', 0, 0o644, ""), b""),
            (file("safe", &smuggled), &smuggled),
        ],
        "`pax` has type `x`",
    );
}

#[test]
fn a_file_through_a_symlink_is_refused() {
    let out = refused(
        &[
            (dir("sub/"), b""),
            (link("link", "sub"), b""),
            (file("link/x", b"1"), b"1"),
        ],
        "`link/x` passes through `link`, which is not a directory",
    );
    assert!(!out.at("sub/x").exists());
}

#[test]
fn a_directory_over_a_symlink_is_refused() {
    refused(
        &[
            (dir("sub/"), b""),
            (link("link", "sub"), b""),
            (dir("link/"), b""),
        ],
        "`link/` is in the archive twice",
    );
}

#[test]
fn a_file_under_a_file_is_refused() {
    refused(
        &[(file("f", b"1"), b"1"), (file("f/x", b"2"), b"2")],
        "`f/x` passes through `f`, which is not a directory",
    );
}

#[test]
fn a_name_already_written_is_refused() {
    let out = refused(
        &[(file("a", b"1"), b"1"), (file("a", b"2"), b"2")],
        "`a` is in the archive twice",
    );
    assert_eq!(fs::read(out.at("a")).unwrap(), b"1");
    refused(
        &[(link("s", "a"), b""), (file("s", b"2"), b"2")],
        "`s` is in the archive twice",
    );
    let out = refused(
        &[(link("d", "nowhere"), b""), (file("d", b"2"), b"2")],
        "`d` is in the archive twice",
    );
    assert!(!out.at("nowhere").exists());
    refused(
        &[(file("a", b"1"), b"1"), (link("a", "b"), b"")],
        "`a` is in the archive twice",
    );
}

#[test]
fn the_uncompressed_size_is_capped() {
    // 512 + 3,584 + 1,024 = 5,120 bytes.
    let data = vec![b'x'; 3584];
    let tar = archive(&[(file("a", &data), &data)]);
    assert_eq!(tar.len(), 5120);
    let limits = Limits {
        bytes: 4096,
        members: 10,
    };
    refused_tar(&tar, &limits, "holds more than 4096 bytes uncompressed");
    // 512 + 2,560 + 1,024 = 4,096 bytes: exactly the cap.
    let data = vec![b'x'; 2560];
    let tar = archive(&[(file("a", &data), &data)]);
    assert_eq!(tar.len(), 4096);
    let (out, result) = run(&tar, &limits);
    result.unwrap();
    assert_eq!(fs::read(out.at("a")).unwrap(), data);
    // One byte under the archive is over the cap; one byte over is not.
    let under = Limits {
        bytes: 4095,
        members: 10,
    };
    refused_tar(&tar, &under, "holds more than 4095 bytes uncompressed");
    let over = Limits {
        bytes: 4097,
        members: 10,
    };
    run(&tar, &over).1.unwrap();
}

#[test]
fn the_member_count_is_capped() {
    let limits = Limits {
        bytes: LIMITS.bytes,
        members: 2,
    };
    let two = archive(&[(file("a", b""), b""), (file("b", b""), b"")]);
    let (_out, result) = run(&two, &limits);
    result.unwrap();
    let three = archive(&[
        (file("a", b""), b""),
        (file("b", b""), b""),
        (file("c", b""), b""),
    ]);
    let out = refused_tar(&three, &limits, "holds more than 2 members");
    assert!(!out.at("c").exists());
}

#[test]
fn input_that_is_not_gzip_is_refused() {
    let out = Out::new();
    let err = timed(b"not gzip at all", &out.target(), "x.tar.gz", &LIMITS).unwrap_err();
    assert!(matches!(err, Error::BadArchive { .. }), "{err:?}");
    assert!(
        err.to_string().contains("is not a valid gzip stream"),
        "{err}"
    );
}

#[test]
fn a_failed_write_is_an_io_error() {
    let out = Out::new();
    let gone = out.held.path().join("gone");
    let tar = gzip(&archive(&[(file("a", b"1"), b"1")]));
    let err = timed(&tar, &gone, "x.tar.gz", &LIMITS).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
}

/// A label, a change to a valid fixture's gzip bytes, and the refusal.
type Case = (&'static str, Box<dyn Fn(&mut Vec<u8>)>, &'static str);

/// A valid fixture's gzip bytes, then `change` applied to them.
fn gz_changed(tar: &[u8], change: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let mut gz = gzip(tar);
    change(&mut gz);
    gz
}

#[test]
fn the_gzip_trailer_and_what_follows_it_are_checked() {
    let tar = archive(&[(file("a", b"1"), b"1")]);
    let cases: [Case; 6] = [
        (
            "a corrupt CRC32",
            Box::new(|gz| {
                let at = gz.len() - 8;
                gz[at] ^= 1;
            }),
            "is not a valid gzip stream",
        ),
        (
            "a wrong length",
            Box::new(|gz| {
                let at = gz.len() - 4;
                gz[at] ^= 1;
            }),
            "is not a valid gzip stream",
        ),
        (
            "a trailer cut short",
            Box::new(|gz| gz.truncate(gz.len() - 4)),
            "is not a valid gzip stream",
        ),
        (
            "no trailer",
            Box::new(|gz| gz.truncate(gz.len() - 8)),
            "is not a valid gzip stream",
        ),
        (
            "junk after the trailer",
            Box::new(|gz| gz.extend_from_slice(b"junk")),
            "4 bytes follow the gzip stream",
        ),
        (
            "a second gzip member",
            Box::new(|gz| gz.extend_from_slice(&gzip(b"more"))),
            "bytes follow the gzip stream",
        ),
    ];
    for (label, change, why) in cases {
        let out = Out::new();
        let gz = gz_changed(&tar, change);
        let err = timed(&gz, &out.target(), "x.tar.gz", &LIMITS).unwrap_err();
        assert!(matches!(err, Error::BadArchive { .. }), "{label}: {err:?}");
        assert!(err.to_string().contains(why), "{label}: {err}");
        out.outside_untouched();
    }
}

#[test]
fn only_zero_padding_may_follow_the_end_blocks() {
    let mut tar = archive(&[(file("a", b"1"), b"1")]);
    tar.extend_from_slice(&[0; 600]);
    tar.push(1);
    refused_tar(&tar, &LIMITS, "data follows the end blocks");
}

#[test]
fn zero_padding_counts_toward_the_cap() {
    let mut tar = archive(&[(file("a", b"1"), b"1")]);
    let limits = Limits {
        bytes: tar.len() as u64 + 100,
        members: 10,
    };
    tar.extend_from_slice(&[0; 1024]);
    refused_tar(&tar, &limits, "uncompressed");
}

#[test]
fn a_tar_record_of_zero_padding_is_accepted() {
    let mut tar = archive(&[(file("a", b"1"), b"1")]);
    tar.resize(10_240, 0);
    let (out, result) = run(&tar, &LIMITS);
    result.unwrap();
    assert_eq!(fs::read(out.at("a")).unwrap(), b"1");
}

#[test]
fn padding_of_exactly_one_read_is_accepted_and_checked_to_its_end() {
    // 4,096 bytes of padding fill one read exactly; the read after it finds
    // the end. Non-zero data in a second read is still seen.
    let tar = archive(&[(file("a", b"1"), b"1")]);
    let mut exact = tar.clone();
    exact.resize(tar.len() + 4096, 0);
    run(&exact, &LIMITS).1.unwrap();
    let mut after = exact;
    after.push(1);
    refused_tar(&after, &LIMITS, "data follows the end blocks");
}

#[test]
fn a_name_that_is_not_utf8_is_refused() {
    let mut block = file("a", b"");
    block[0] = 0xff;
    checksum(&mut block);
    refused(&[(block, b"")], "is not UTF-8");
}

#[test]
fn a_full_width_name_has_no_terminator() {
    let name = "n".repeat(100);
    let out = ok(&[(file(&name, b"1"), b"1")]);
    assert_eq!(fs::read(out.at(&name)).unwrap(), b"1");
}
