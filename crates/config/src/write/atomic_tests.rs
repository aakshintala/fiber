//! The [`super::atomic_error`] phase mapping: a post-rename directory-sync
//! failure (carrying [`support::atomic::Unsynced`]) becomes
//! [`ConfigError::Unsynced`] with the new file already in place, anything
//! else becomes [`ConfigError::Io`]. The owned inner error is recovered
//! whole, so its kind and platform error survive the mapping.

use std::io;
use std::path::Path;

use super::atomic_error;
use crate::error::ConfigError;

#[test]
fn unsynced_display_shows_the_inner_message() {
    let marker = support::atomic::Unsynced(io::Error::other("sync lost"));
    assert_eq!(marker.to_string(), "sync lost");
}

#[test]
fn unsynced_source_is_the_inner_error() {
    use std::error::Error as _;
    let marker = support::atomic::Unsynced(io::Error::other("sync lost"));
    let source = marker.source().expect("Unsynced carries its inner error");
    assert_eq!(source.to_string(), "sync lost");
}

#[test]
fn atomic_error_maps_an_unsynced_payload_to_config_unsynced() {
    let file = Path::new("/dest/file");
    let inner = io::Error::new(io::ErrorKind::TimedOut, "sync slow");
    let error = atomic_error(
        file,
        io::Error::new(inner.kind(), support::atomic::Unsynced(inner)),
    );
    let ConfigError::Unsynced { file, source } = error else {
        panic!("an Unsynced payload maps to ConfigError::Unsynced")
    };
    assert_eq!(file, Path::new("/dest/file"));
    assert_eq!(source.kind(), io::ErrorKind::TimedOut);
    assert_eq!(source.to_string(), "sync slow");
}

#[test]
fn atomic_error_preserves_the_inner_platform_error() {
    let file = Path::new("/dest/file");
    let inner = io::Error::from_raw_os_error(5);
    let raw = inner.raw_os_error();
    let error = atomic_error(
        file,
        io::Error::new(inner.kind(), support::atomic::Unsynced(inner)),
    );
    let ConfigError::Unsynced { source, .. } = error else {
        panic!("an Unsynced payload maps to ConfigError::Unsynced")
    };
    assert_eq!(source.raw_os_error(), raw);
}

#[test]
fn atomic_error_maps_a_plain_error_to_io_with_the_destination() {
    let file = Path::new("/dest/file");
    let error = atomic_error(file, io::Error::new(io::ErrorKind::NotFound, "gone"));
    let ConfigError::Io { file, source } = error else {
        panic!("a plain error maps to ConfigError::Io")
    };
    assert_eq!(file, Path::new("/dest/file"));
    assert_eq!(source.kind(), io::ErrorKind::NotFound);
}
