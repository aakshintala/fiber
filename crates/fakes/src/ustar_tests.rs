use std::io::Read;

use super::{archive, checksum, gzip, header, sha256};

/// The header's checksum field, as a number.
fn stored(block: &[u8; 512]) -> u64 {
    let field = String::from_utf8_lossy(&block[148..154]).into_owned();
    u64::from_str_radix(&field, 8).unwrap()
}

/// Every byte of the header, with the checksum field counted as spaces.
fn summed(block: &[u8; 512]) -> u64 {
    block
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(*b)
            }
        })
        .sum()
}

#[test]
fn a_header_carries_the_magic_and_a_matching_checksum() {
    let block = header("dir/file.txt", b'0', 5, 0o644, "");
    assert_eq!(&block[257..263], b"ustar\0");
    assert_eq!(&block[263..265], b"00");
    assert_eq!(&block[0..12], b"dir/file.txt");
    assert_eq!(block[156], b'0');
    assert_eq!(&block[124..135], b"00000000005");
    assert_eq!(&block[100..107], b"0000644");
    assert_eq!(stored(&block), summed(&block));
}

#[test]
fn a_long_name_is_split_into_prefix_and_name() {
    let long = format!("{}/{}", "p".repeat(120), "n".repeat(90));
    let block = header(&long, b'0', 0, 0o644, "");
    assert_eq!(&block[0..90], "n".repeat(90).as_bytes());
    assert_eq!(block[90], 0);
    assert_eq!(&block[345..465], "p".repeat(120).as_bytes());
}

#[test]
fn a_long_name_splits_at_the_last_slash_that_fits_both_fields() {
    // Split at the last `/`, the prefix would be 161 bytes; at the one
    // before it, the prefix is 100 and the name 71.
    let name = format!("{}/{}", "q".repeat(60), "n".repeat(10));
    let long = format!("{}/{name}", "p".repeat(100));
    let block = header(&long, b'0', 0, 0o644, "");
    assert_eq!(&block[0..71], name.as_bytes());
    assert_eq!(block[71], 0);
    assert_eq!(&block[345..445], "p".repeat(100).as_bytes());
    assert_eq!(block[445], 0);
}

#[test]
fn a_symlink_header_holds_its_target() {
    let block = header("link", b'2', 0, 0o777, "sub/target");
    assert_eq!(&block[157..167], b"sub/target");
    assert_eq!(block[156], b'2');
}

#[test]
fn checksum_recomputes_after_a_change() {
    let mut block = header("a", b'0', 0, 0o644, "");
    block[0] = b'b';
    assert_ne!(stored(&block), summed(&block));
    checksum(&mut block);
    assert_eq!(stored(&block), summed(&block));
}

#[test]
fn an_archive_pads_each_member_and_ends_with_two_zero_blocks() {
    let bytes = archive(&[(header("a", b'0', 3, 0o644, ""), b"abc")]);
    assert_eq!(bytes.len(), 512 * 4);
    assert_eq!(&bytes[512..515], b"abc");
    assert!(bytes[515..].iter().all(|b| *b == 0));
}

#[test]
fn gzip_round_trips_through_flate2() {
    let input = b"hello, archive".repeat(100);
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&gzip(&input)[..])
        .read_to_end(&mut out)
        .unwrap();
    assert_eq!(out, input);
}

#[test]
fn sha256_is_lowercase_hex() {
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
