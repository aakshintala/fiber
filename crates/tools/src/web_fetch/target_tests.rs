//! Tests beside [`super`]: what a URL declares, how a redirect's `location`
//! joins, and which hosts and addresses fetch refuses.

use std::net::IpAddr;

use super::{blocked_addr, blocked_name, host_and_port, join, parse, prefix, subject};

fn subject_of(url: &str) -> String {
    subject(&parse(url).unwrap())
}

fn prefix_of(url: &str) -> String {
    prefix(&parse(url).unwrap())
}

fn joined(base: &str, location: &str) -> String {
    join(&parse(base).unwrap(), location)
}

fn blocked(address: &str) -> bool {
    blocked_addr(address.parse::<IpAddr>().unwrap())
}

#[test]
fn a_bare_host_has_the_root_path() {
    assert_eq!(subject_of("https://example.com"), "https://example.com/");
    assert_eq!(prefix_of("https://example.com"), "https://example.com/");
}

#[test]
fn the_path_and_query_are_kept_and_the_prefix_is_the_host() {
    assert_eq!(
        subject_of("https://example.com/a/b?x=1&y=2"),
        "https://example.com/a/b?x=1&y=2"
    );
    assert_eq!(
        prefix_of("https://example.com/a/b?x=1"),
        "https://example.com/"
    );
}

#[test]
fn a_query_with_no_path_has_the_root_path() {
    assert_eq!(
        subject_of("http://example.com?x=1"),
        "http://example.com/?x=1"
    );
}

#[test]
fn the_fragment_is_dropped() {
    assert_eq!(
        subject_of("https://example.com/a#top"),
        "https://example.com/a"
    );
    assert_eq!(
        subject_of("https://example.com#top"),
        "https://example.com/"
    );
    assert_eq!(
        subject_of("https://example.com/a?x=1#top?y"),
        "https://example.com/a?x=1"
    );
}

#[test]
fn the_port_is_kept_as_written() {
    assert_eq!(
        subject_of("http://localhost:8080/x"),
        "http://localhost:8080/x"
    );
    assert_eq!(
        prefix_of("http://localhost:8080/x"),
        "http://localhost:8080/"
    );
    assert_eq!(
        subject_of("https://example.com:443/"),
        "https://example.com:443/"
    );
}

#[test]
fn the_scheme_and_host_are_lowercased_and_the_path_is_not() {
    assert_eq!(
        subject_of("HTTPS://Example.COM/Path/Q?K=V"),
        "https://example.com/Path/Q?K=V"
    );
    assert_eq!(prefix_of("HTTP://LocalHost:80/X"), "http://localhost:80/");
}

#[test]
fn an_ipv6_literal_keeps_its_brackets() {
    assert_eq!(subject_of("http://[::1]:8080/x"), "http://[::1]:8080/x");
    assert_eq!(prefix_of("http://[FE80::1]/x"), "http://[fe80::1]/");
}

#[test]
fn surrounding_whitespace_is_not_part_of_the_url() {
    assert_eq!(
        subject_of("  https://example.com/a \n"),
        "https://example.com/a"
    );
}

#[test]
fn a_url_that_is_not_http_or_https_with_a_host_is_refused() {
    for url in [
        "example.com",
        "example.com/path",
        "/just/a/path",
        "ftp://example.com/",
        "file:///etc/passwd",
        "mailto:a@example.com",
        "https://",
        "http:///path",
        "http://:80/",
        "http:foo",
        "",
        "   ",
        "http://exa mple.com/",
        "javascript:alert(1)",
    ] {
        assert!(parse(url).is_err(), "{url:?} was accepted");
    }
}

#[test]
fn a_refusal_names_the_url() {
    let why = parse("ftp://example.com/").unwrap_err();
    assert!(why.contains("ftp://example.com/"), "{why}");
    assert!(why.contains("http"), "{why}");
}

#[test]
fn the_host_and_port_default_by_scheme() {
    assert_eq!(
        host_and_port(&parse("http://Example.com/").unwrap()),
        ("example.com".to_owned(), 80)
    );
    assert_eq!(
        host_and_port(&parse("https://example.com").unwrap()),
        ("example.com".to_owned(), 443)
    );
    assert_eq!(
        host_and_port(&parse("https://example.com:8443/").unwrap()),
        ("example.com".to_owned(), 8443)
    );
    assert_eq!(
        host_and_port(&parse("http://[::1]:9/").unwrap()),
        ("::1".to_owned(), 9)
    );
}

#[test]
fn an_absolute_location_replaces_the_url() {
    assert_eq!(
        joined("https://a.test/x/y", "http://b.test/z"),
        "http://b.test/z"
    );
    assert_eq!(
        joined("https://a.test/x", "https://b.test"),
        "https://b.test"
    );
}

#[test]
fn a_location_with_another_scheme_is_kept_for_the_caller_to_refuse() {
    assert_eq!(
        joined("https://a.test/x", "ftp://b.test/z"),
        "ftp://b.test/z"
    );
    assert_eq!(
        joined("https://a.test/x", "mailto:a@b.test"),
        "mailto:a@b.test"
    );
}

#[test]
fn a_scheme_relative_location_takes_the_current_scheme() {
    assert_eq!(joined("https://a.test/x", "//b.test/z"), "https://b.test/z");
    assert_eq!(
        joined("http://a.test/x", "//b.test:81/z?q"),
        "http://b.test:81/z?q"
    );
}

#[test]
fn a_rooted_location_takes_the_current_scheme_and_authority() {
    assert_eq!(
        joined("https://a.test:8443/x/y?q=1", "/z?r=2"),
        "https://a.test:8443/z?r=2"
    );
}

#[test]
fn a_relative_location_replaces_the_last_path_segment() {
    assert_eq!(joined("https://a.test/x/y", "z"), "https://a.test/x/z");
    assert_eq!(joined("https://a.test/x/y/", "z"), "https://a.test/x/y/z");
    assert_eq!(
        joined("https://a.test/x/y?q=1", "z?r=2"),
        "https://a.test/x/z?r=2"
    );
    assert_eq!(joined("https://a.test", "z"), "https://a.test/z");
    assert_eq!(joined("https://a.test/y", "z/w"), "https://a.test/z/w");
}

#[test]
fn a_query_only_location_keeps_the_path() {
    assert_eq!(
        joined("https://a.test/x/y?q=1", "?r=2"),
        "https://a.test/x/y?r=2"
    );
    assert_eq!(joined("https://a.test", "?r=2"), "https://a.test/?r=2");
}

#[test]
fn dot_segments_are_not_normalised() {
    assert_eq!(
        joined("https://a.test/x/y", "../z"),
        "https://a.test/x/../z"
    );
    assert_eq!(joined("https://a.test/x/y", "./z"), "https://a.test/x/./z");
}

#[test]
fn a_location_fragment_is_dropped() {
    assert_eq!(joined("https://a.test/x", "/z#frag"), "https://a.test/z");
    assert_eq!(joined("https://a.test/x", "z#frag"), "https://a.test/z");
    assert_eq!(
        joined("https://a.test/x", "http://b.test/#frag"),
        "http://b.test/"
    );
    assert_eq!(joined("https://a.test/x", "#frag"), "https://a.test/x");
}

#[test]
fn a_location_loses_surrounding_whitespace() {
    assert_eq!(joined("https://a.test/x/y", "  z \t"), "https://a.test/x/z");
}

#[test]
fn the_current_userinfo_is_not_carried_to_a_rooted_location() {
    assert_eq!(
        joined("https://user:pw@a.test/x", "/z"),
        "https://user:pw@a.test/z"
    );
}

#[test]
fn link_local_ipv4_is_blocked_at_its_edges() {
    assert!(blocked("169.254.0.0"));
    assert!(blocked("169.254.169.254"));
    assert!(blocked("169.254.255.255"));
    assert!(!blocked("169.253.255.255"));
    assert!(!blocked("169.255.0.0"));
    assert!(!blocked("168.254.0.1"));
    assert!(!blocked("170.254.0.1"));
}

#[test]
fn link_local_ipv6_is_blocked_at_its_edges() {
    assert!(blocked("fe80::1"));
    assert!(blocked("fe80::"));
    assert!(blocked("febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff"));
    assert!(blocked("febf:ffff::1"));
    assert!(!blocked("fec0::1"));
    assert!(!blocked("fe7f::1"));
    assert!(!blocked("fe00::1"));
}

#[test]
fn an_ipv4_mapped_address_is_judged_as_its_ipv4_address() {
    assert!(blocked("::ffff:169.254.169.254"));
    assert!(blocked("::ffff:a9fe:a9fe"));
    assert!(!blocked("::ffff:10.0.0.1"));
    assert!(!blocked("::ffff:169.253.0.1"));
}

#[test]
fn the_aws_ipv6_metadata_address_is_blocked_and_its_neighbours_are_not() {
    assert!(blocked("fd00:ec2::254"));
    assert!(!blocked("fd00:ec2::253"));
    assert!(!blocked("fd00:ec2::255"));
    assert!(!blocked("fd00:ec3::254"));
    assert!(!blocked("fd01:ec2::254"));
}

#[test]
fn loopback_and_private_addresses_are_allowed() {
    for address in [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.1.1",
        "100.64.0.1",
        "0.0.0.0",
        "::1",
        "::",
        "fc00::1",
        "2001:4860:4860::8888",
        "8.8.8.8",
    ] {
        assert!(!blocked(address), "{address}");
    }
}

#[test]
fn metadata_host_names_are_blocked_whatever_their_case_or_trailing_dot() {
    assert!(blocked_name("metadata.google.internal"));
    assert!(blocked_name("METADATA.GOOGLE.INTERNAL."));
    assert!(blocked_name("Metadata.Google.Internal"));
    assert!(blocked_name("metadata.goog"));
    assert!(blocked_name("metadata.goog."));
}

#[test]
fn other_names_are_not_blocked() {
    assert!(!blocked_name("metadata.google.internal.example.com"));
    assert!(!blocked_name("xmetadata.google.internal"));
    assert!(!blocked_name("google.internal"));
    assert!(!blocked_name("metadata.goog.example"));
    assert!(!blocked_name("metadata.google.internal.."));
    assert!(!blocked_name("example.com"));
    assert!(!blocked_name("localhost"));
    assert!(!blocked_name(""));
}

#[test]
fn a_location_starting_with_a_digit_is_relative() {
    assert_eq!(
        joined("https://a.test/x/y", "1a:x"),
        "https://a.test/x/1a:x"
    );
}

#[test]
fn a_location_whose_scheme_has_a_space_is_relative() {
    assert_eq!(
        joined("https://a.test/x/y", "a b:x"),
        "https://a.test/x/a b:x"
    );
}

#[test]
fn a_location_with_a_symbolic_scheme_is_absolute() {
    assert_eq!(
        joined("https://a.test/x", "web+page.test:x"),
        "web+page.test:x"
    );
}
