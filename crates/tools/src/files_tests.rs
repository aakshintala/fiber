use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;

use fakes::TempDir;

use super::{
    InspectError, Inspected, MAX_SYMLINKS, ResolveError, inspect, pdf_magic, read_regular, resolve,
    unsupported_message,
};

fn canonical(path: &Path) -> std::path::PathBuf {
    fs::canonicalize(path).unwrap()
}

fn set_mode(path: &Path, mode: u32) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).unwrap();
}

fn symlink_chain(dir: &Path, links: u32) {
    fs::write(dir.join("target.txt"), "hi").unwrap();
    for n in (0..links).rev() {
        let next = if n + 1 == links {
            "target.txt".to_owned()
        } else {
            format!("link{}", n + 1)
        };
        symlink(next, dir.join(format!("link{n}"))).unwrap();
    }
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
fn dotdot_through_a_regular_file_is_tool_error() {
    let dir = TempDir::new("fiber-resolve-file-dotdot");
    fs::write(dir.path().join("f"), "x").unwrap();
    fs::write(dir.path().join("a.txt"), "y").unwrap();
    let err = resolve(dir.path(), "f/../a.txt").unwrap_err();
    match err {
        ResolveError::Tool(message) => assert!(message.contains("not a directory"), "{message}"),
        ResolveError::Arguments(message) => panic!("expected tool_error, got {message}"),
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
        Inspected::Text { text } => assert_eq!(text, "hi\n"),
        Inspected::Unsupported { kind, .. } => panic!("expected text, got {kind}"),
        Inspected::Image { kind, .. } => panic!("expected text, got {kind}"),
        Inspected::Pdf { .. } | Inspected::PdfOverCap { .. } => panic!("expected text, got a PDF"),
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
        Inspected::Text { .. }
        | Inspected::Image { .. }
        | Inspected::Pdf { .. }
        | Inspected::PdfOverCap { .. } => {
            panic!("expected a directory")
        }
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
        Inspected::Text { .. }
        | Inspected::Image { .. }
        | Inspected::Pdf { .. }
        | Inspected::PdfOverCap { .. } => {
            panic!("opening a fifo would block")
        }
    }
}

#[test]
fn a_socket_is_unsupported() {
    let dir = TempDir::new("fiber-class-sock");
    let path = dir.path().join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    match inspect(&path).unwrap() {
        Inspected::Unsupported { kind, .. } => assert!(kind.contains("socket"), "{kind}"),
        Inspected::Text { .. }
        | Inspected::Image { .. }
        | Inspected::Pdf { .. }
        | Inspected::PdfOverCap { .. } => {
            panic!("expected a socket")
        }
    }
    drop(listener);
}

#[test]
fn a_device_is_unsupported() {
    let path = Path::new("/dev/null");
    match inspect(path).unwrap() {
        Inspected::Unsupported { kind, .. } => assert!(kind.contains("device"), "{kind}"),
        Inspected::Text { .. }
        | Inspected::Image { .. }
        | Inspected::Pdf { .. }
        | Inspected::PdfOverCap { .. } => {
            panic!("expected a device")
        }
    }
}

#[test]
fn recognised_magic_nul_and_invalid_utf8_are_typed() {
    let dir = TempDir::new("fiber-class-magic");
    let images: &[(&str, &[u8], &str)] = &[
        ("a.png", b"\x89PNG\r\n\x1a\nrest", "PNG"),
        ("a.jpg", &[0xFF, 0xD8, 0xFF, 0x00], "JPEG"),
        ("a.gif", b"GIF89a-rest", "GIF"),
        ("a.gif87", b"GIF87a-rest", "GIF"),
        ("a.webp", b"RIFF\x00\x00\x00\x00WEBPrest", "WebP"),
    ];
    for (name, bytes, expect) in images {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        match inspect(&path).unwrap() {
            Inspected::Image { kind, size, .. } => {
                assert!(kind.contains(expect), "{name}: {kind}");
                assert_eq!(size, u64::try_from(bytes.len()).unwrap(), "{name}");
            }
            Inspected::Text { .. }
            | Inspected::Unsupported { .. }
            | Inspected::Pdf { .. }
            | Inspected::PdfOverCap { .. } => {
                panic!("{name} was not an image")
            }
        }
    }
    let others: &[(&str, &[u8], &str)] = &[
        ("a.bin", b"hello\0world", "binary data"),
        ("a.dat", b"\xff\xfe", "not UTF-8 text"),
        ("a.riff", b"RIFF\x00\x00\x00\x00WAVErest", "binary data"),
    ];
    for (name, bytes, expect) in others {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        match inspect(&path).unwrap() {
            Inspected::Unsupported { kind, size, .. } => {
                assert!(kind.contains(expect), "{name}: {kind}");
                assert_eq!(size, u64::try_from(bytes.len()).unwrap(), "{name}");
            }
            Inspected::Text { .. }
            | Inspected::Image { .. }
            | Inspected::Pdf { .. }
            | Inspected::PdfOverCap { .. } => {
                panic!("{name} was read as text or an image")
            }
        }
    }
    let pdf = dir.path().join("a.pdf");
    let pdf_bytes = b"%PDF-1.4";
    fs::write(&pdf, pdf_bytes).unwrap();
    match inspect(&pdf).unwrap() {
        Inspected::Pdf { size, hash } => {
            assert_eq!(size, u64::try_from(pdf_bytes.len()).unwrap());
            assert_eq!(hash, super::hash_bytes(pdf_bytes));
        }
        Inspected::Text { .. }
        | Inspected::Image { .. }
        | Inspected::Unsupported { .. }
        | Inspected::PdfOverCap { .. } => {
            panic!("a.pdf was not a PDF")
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

#[test]
fn dotdot_on_the_root_stays_at_the_root() {
    let got = resolve(Path::new("/unused"), "/..").unwrap();
    assert_eq!(got, Path::new("/"));
}

#[test]
fn dotdot_from_a_relative_start_is_parent() {
    let got = resolve(Path::new("."), "..").unwrap();
    assert_eq!(got, Path::new(".."));
}

#[test]
fn an_unsearchable_directory_is_tool_error() {
    let dir = TempDir::new("fiber-resolve-unsearch");
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    set_mode(&locked, 0o000);
    let err = resolve(dir.path(), "locked/child").unwrap_err();
    set_mode(&locked, 0o755);
    match err {
        ResolveError::Tool(message) => assert!(message.contains("locked"), "{message}"),
        ResolveError::Arguments(message) => panic!("expected tool_error, got {message}"),
    }
}

#[test]
fn forty_symlinks_resolve_and_one_more_is_tool_error() {
    // The temp directory's own path can contain a symlink (`/var` on macOS).
    // Count only the chain.
    let dir = TempDir::new("fiber-resolve-chain");
    let root = canonical(dir.path());
    symlink_chain(&root, MAX_SYMLINKS);
    let got = resolve(&root, "link0").unwrap();
    assert_eq!(got, root.join("target.txt"));

    let over = TempDir::new("fiber-resolve-chain-over");
    let over_root = canonical(over.path());
    symlink_chain(&over_root, MAX_SYMLINKS + 1);
    let err = resolve(&over_root, "link0").unwrap_err();
    match err {
        ResolveError::Tool(message) => assert!(message.contains("symbolic link"), "{message}"),
        ResolveError::Arguments(message) => panic!("expected tool_error, got {message}"),
    }
}

#[test]
fn an_unsearchable_path_is_tool_error_not_missing() {
    let dir = TempDir::new("fiber-class-unsearch");
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    set_mode(&locked, 0o000);
    let err = inspect(&locked.join("secret")).unwrap_err();
    set_mode(&locked, 0o755);
    match err {
        InspectError::Tool(message) => assert!(message.contains("secret"), "{message}"),
        InspectError::NotFound => panic!("expected tool_error"),
    }
}

#[test]
fn a_file_that_vanishes_before_it_is_read_is_not_found() {
    let dir = TempDir::new("fiber-class-vanished");
    let err = read_regular(&dir.path().join("gone")).unwrap_err();
    assert!(matches!(err, InspectError::NotFound));
}

#[test]
fn an_unreadable_regular_file_is_tool_error_from_the_read() {
    let dir = TempDir::new("fiber-class-read-perm");
    let path = dir.path().join("secret");
    fs::write(&path, "hi").unwrap();
    set_mode(&path, 0o000);
    let err = read_regular(&path).unwrap_err();
    set_mode(&path, 0o644);
    match err {
        InspectError::Tool(message) => assert!(message.contains("secret"), "{message}"),
        InspectError::NotFound => panic!("expected tool_error"),
    }
}

#[test]
fn riff_without_webp_and_webp_without_riff_are_text() {
    let dir = TempDir::new("fiber-class-riff");
    let cases: &[(&str, &[u8])] = &[
        ("riff.txt", b"RIFF not webp"),
        ("mark.txt", b"xxxxxxxxWEBP"),
    ];
    for (name, bytes) in cases {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        match inspect(&path).unwrap() {
            Inspected::Text { text } => assert_eq!(text.as_bytes(), *bytes),
            Inspected::Unsupported { kind, .. } => panic!("{name} was {kind}"),
            Inspected::Image { kind, .. } => panic!("{name} was {kind}"),
            Inspected::Pdf { .. } | Inspected::PdfOverCap { .. } => panic!("{name} was a PDF"),
        }
    }
}

#[test]
fn with_locks_shares_one_lock_set_with_outside_holders() {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use super::{Files, PathLocks};

    const DEADLINE: Duration = Duration::from_secs(10);

    let dir = TempDir::new("fiber-files-with-locks");
    let locks = Arc::new(PathLocks::new());
    let files = Files::with_locks(dir.path().to_path_buf(), Arc::clone(&locks));
    assert!(Arc::ptr_eq(&locks, &files.locks()));
    let path = dir.path().join("a.txt");
    let first = Arc::clone(&locks);
    let holder_path = path.clone();
    let (holding, holding_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let guard = first.lock(&holder_path);
        holding.send(()).unwrap();
        release_rx
            .recv_timeout(DEADLINE)
            .expect("waited 10s for the release of the outside holder");
        drop(guard);
    });
    assert!(
        holding_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the outside holder to be held"
    );
    let through_files = files.locks();
    let (done, done_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let guard = through_files.lock(&path);
        done.send(()).unwrap();
        drop(guard);
    });
    let probe = Arc::clone(&locks);
    let (seen, seen_rx) = mpsc::channel();
    std::thread::spawn(move || {
        while probe.waiting() != 1 {
            std::thread::yield_now();
        }
        seen.send(()).unwrap();
    });
    assert!(
        seen_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the lock through Files to be waiting"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "the lock through Files acquired the path while it was held"
    );
    release.send(()).unwrap();
    assert!(
        done_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the lock through Files to acquire the path"
    );
    holder.join().unwrap();
    handle.join().unwrap();
    assert!(locks.is_clear());
}

#[test]
fn pdf_magic_reads_only_the_first_four_bytes() {
    let dir = TempDir::new("fiber-pdf-magic");
    let short = dir.path().join("short");
    fs::write(&short, b"abc").unwrap();
    assert!(!pdf_magic(&short).unwrap());
    let text = dir.path().join("text");
    fs::write(&text, b"abcdefgh").unwrap();
    assert!(!pdf_magic(&text).unwrap());
    let pdf = dir.path().join("magic");
    fs::write(&pdf, b"%PDF-1.4 rest").unwrap();
    assert!(pdf_magic(&pdf).unwrap());
}

#[test]
fn a_pdf_over_the_cap_is_classified_from_its_metadata() {
    let dir = TempDir::new("fiber-class-pdf-cap");
    let path = dir.path().join("big.pdf");
    fs::write(&path, b"%PDF-1.4 sparse").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(crate::pdf::PDF_MAX_BYTES + 1)
        .unwrap();
    match inspect(&path).unwrap() {
        Inspected::PdfOverCap { size } => {
            assert_eq!(size, crate::pdf::PDF_MAX_BYTES + 1);
        }
        Inspected::Text { .. }
        | Inspected::Image { .. }
        | Inspected::Pdf { .. }
        | Inspected::Unsupported { .. } => {
            panic!("a PDF over the cap was loaded")
        }
    }
}
