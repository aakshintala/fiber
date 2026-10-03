use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;

use fakes::TempDir;

use super::{InspectError, Inspected, ResolveError, inspect, resolve, unsupported_message};

fn canonical(path: &Path) -> std::path::PathBuf {
    fs::canonicalize(path).unwrap()
}

#[test]
fn a_relative_path_joins_the_workspace() {
    let dir = TempDir::new("fiber-resolve-rel");
    fs::write(dir.path().join("a.txt"), "hi").unwrap();
    let got = resolve(dir.path(), "a.txt").unwrap();
    assert_eq!(got, canonical(dir.path()).join("a.txt"));
}

#[test]
fn a_symlink_resolves_to_its_target() {
    let dir = TempDir::new("fiber-resolve-link");
    fs::write(dir.path().join("real.txt"), "hi").unwrap();
    symlink("real.txt", dir.path().join("link")).unwrap();
    let got = resolve(dir.path(), "link").unwrap();
    assert_eq!(got, canonical(dir.path()).join("real.txt"));
}

#[test]
fn a_symlink_to_a_missing_target_resolves_to_that_path() {
    let dir = TempDir::new("fiber-resolve-missing");
    symlink("missing.txt", dir.path().join("link")).unwrap();
    let got = resolve(dir.path(), "link").unwrap();
    assert_eq!(got, canonical(dir.path()).join("missing.txt"));
}

#[test]
fn a_missing_file_resolves_under_the_canonical_existing_prefix() {
    let dir = TempDir::new("fiber-resolve-prefix");
    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    let workspace = dir.path().join("ws");
    symlink(&real, &workspace).unwrap();
    let got = resolve(&workspace, "newdir/file.txt").unwrap();
    assert_eq!(got, canonical(&real).join("newdir").join("file.txt"));
}

#[test]
fn dotdot_in_the_nonexistent_rest_is_invalid_arguments() {
    let dir = TempDir::new("fiber-resolve-dotdot");
    let err = resolve(dir.path(), "nope/../a.txt").unwrap_err();
    match err {
        ResolveError::Arguments(message) => assert!(message.contains("`..`"), "{message}"),
        ResolveError::Tool(message) => panic!("expected invalid_arguments, got {message}"),
    }
}

#[test]
fn dotdot_through_an_existing_directory_resolves() {
    let dir = TempDir::new("fiber-resolve-dotdot-ok");
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("a.txt"), "x").unwrap();
    let got = resolve(dir.path(), "sub/../a.txt").unwrap();
    assert_eq!(got, canonical(dir.path()).join("a.txt"));
}

#[test]
fn a_symlink_loop_is_tool_error() {
    let dir = TempDir::new("fiber-resolve-loop");
    symlink("b", dir.path().join("a")).unwrap();
    symlink("a", dir.path().join("b")).unwrap();
    let err = resolve(dir.path(), "a").unwrap_err();
    match err {
        ResolveError::Tool(message) => assert!(message.contains("symbolic link"), "{message}"),
        ResolveError::Arguments(message) => panic!("expected tool_error, got {message}"),
    }
}

#[test]
fn a_parent_that_is_a_file_is_tool_error() {
    let dir = TempDir::new("fiber-resolve-parent");
    fs::write(dir.path().join("f"), "x").unwrap();
    let err = resolve(dir.path(), "f/child").unwrap_err();
    match err {
        ResolveError::Tool(message) => {
            assert!(
                message.contains("not a directory") || message.contains("could not be resolved"),
                "{message}"
            );
        }
        ResolveError::Arguments(message) => panic!("expected tool_error, got {message}"),
    }
}

#[test]
fn a_regular_file_is_text() {
    let dir = TempDir::new("fiber-class-text");
    let path = dir.path().join("a.txt");
    fs::write(&path, "hi\n").unwrap();
    match inspect(&path).unwrap() {
        Inspected::Text { bytes } => assert_eq!(bytes, b"hi\n"),
        Inspected::Unsupported { kind, .. } => panic!("expected text, got {kind}"),
    }
}

#[test]
fn a_directory_is_unsupported() {
    let dir = TempDir::new("fiber-class-dir");
    let path = dir.path().join("sub");
    fs::create_dir(&path).unwrap();
    let size = fs::symlink_metadata(&path).unwrap().len();
    match inspect(&path).unwrap() {
        Inspected::Unsupported {
            kind,
            size: got,
            hint,
        } => {
            assert!(kind.contains("directory"), "{kind}");
            assert_eq!(got, size);
            assert!(hint.contains("shell"), "{hint}");
            let message = unsupported_message(&path, &kind, got, hint);
            assert!(message.contains(&path.display().to_string()), "{message}");
            assert!(message.contains(&size.to_string()), "{message}");
        }
        Inspected::Text { .. } => panic!("expected a directory"),
    }
}

#[test]
fn a_fifo_is_unsupported_without_opening_it() {
    let dir = TempDir::new("fiber-class-fifo");
    let path = dir.path().join("pipe");
    let status = Command::new("mkfifo").arg(&path).status().unwrap();
    assert!(status.success(), "mkfifo failed");
    match inspect(&path).unwrap() {
        Inspected::Unsupported { kind, .. } => assert!(kind.contains("fifo"), "{kind}"),
        Inspected::Text { .. } => panic!("opening a fifo would block"),
    }
}

#[test]
fn a_socket_is_unsupported() {
    let dir = TempDir::new("fiber-class-sock");
    let path = dir.path().join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    match inspect(&path).unwrap() {
        Inspected::Unsupported { kind, .. } => assert!(kind.contains("socket"), "{kind}"),
        Inspected::Text { .. } => panic!("expected a socket"),
    }
    drop(listener);
}

#[test]
fn a_device_is_unsupported() {
    let path = Path::new("/dev/null");
    match inspect(path).unwrap() {
        Inspected::Unsupported { kind, .. } => assert!(kind.contains("device"), "{kind}"),
        Inspected::Text { .. } => panic!("expected a device"),
    }
}

#[test]
fn recognised_magic_nul_and_invalid_utf8_are_typed() {
    let dir = TempDir::new("fiber-class-magic");
    let cases: &[(&str, &[u8], &str)] = &[
        ("a.png", b"\x89PNG\r\n\x1a\nrest", "PNG"),
        ("a.jpg", &[0xFF, 0xD8, 0xFF, 0x00], "JPEG"),
        ("a.gif", b"GIF89a-rest", "GIF"),
        ("a.webp", b"RIFF\x00\x00\x00\x00WEBPrest", "WebP"),
        ("a.pdf", b"%PDF-1.4", "PDF"),
        ("a.bin", b"hello\0world", "binary data"),
        ("a.dat", b"\xff\xfe", "not UTF-8 text"),
    ];
    for (name, bytes, expect) in cases {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        match inspect(&path).unwrap() {
            Inspected::Unsupported { kind, size, hint } => {
                assert!(kind.contains(expect), "{name}: {kind}");
                assert_eq!(size, u64::try_from(bytes.len()).unwrap(), "{name}");
                if kind.contains("image") || kind.contains("PDF") {
                    assert!(
                        hint.contains("Images and PDFs are not read yet"),
                        "{name}: {hint}"
                    );
                }
            }
            Inspected::Text { .. } => panic!("{name} was read as text"),
        }
    }
}

#[test]
fn an_unreadable_file_is_tool_error() {
    let dir = TempDir::new("fiber-class-perm");
    let path = dir.path().join("secret");
    fs::write(&path, "hi").unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&path, perms).unwrap();
    match inspect(&path).unwrap_err() {
        InspectError::Tool(message) => assert!(message.contains("secret"), "{message}"),
        InspectError::NotFound => panic!("expected tool_error"),
    }
}

#[test]
fn a_missing_path_is_not_found() {
    let dir = TempDir::new("fiber-class-missing");
    let err = inspect(&dir.path().join("nope")).unwrap_err();
    assert!(matches!(err, InspectError::NotFound));
}
