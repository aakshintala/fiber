//! The Linux sandbox policy: unavailable cases, the read-deny contract
//! proved by confined commands, and the walk measurement helper. Tests that
//! need Landlock or a confined spawn run only on Linux; the unavailable
//! variants they assert never arise elsewhere.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use super::{Settings, Unavailable, check_available, denied_paths};

/// A fixture root under the owner's home, not the temp dir: the sandbox
/// grants the temp dir as writable, so a deny beneath it would overlap and
/// report unavailable instead of confining. Each call gets a fresh random
/// suffix and removes itself on drop.
fn fixture(prefix: &str) -> fakes::TempDir {
    let home = std::env::var_os("HOME").expect("HOME names the owner's home");
    fakes::TempDir::under(std::path::Path::new(&home), prefix)
}

fn disabled() -> Settings {
    Settings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: PathBuf::new(),
    }
}

fn workspace_settings(fiber_home: &Path) -> Settings {
    Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: fiber_home.to_path_buf(),
    }
}

#[test]
fn disabled_reports_off() {
    let dir = fixture("fiber-sandbox-disabled");
    let unavailable =
        check_available(&disabled(), dir.path()).expect_err("disabled is unavailable");
    assert_eq!(unavailable, Unavailable::Off);
    assert_eq!(unavailable.to_string(), "the sandbox is off");
}

#[test]
fn enabled_without_overlap_is_available_on_linux() {
    let dir = fixture("fiber-sandbox-available");
    let workspace = dir.path().join("workspace");
    let fiber_home = dir.path().join("fiber-home");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let settings = workspace_settings(&fiber_home);
    #[cfg(target_os = "linux")]
    check_available(&settings, &workspace).expect("no overlap is available");
    #[cfg(not(target_os = "linux"))]
    check_available(&settings, &workspace).expect_err("other platforms are unavailable");
}

#[cfg(target_os = "linux")]
#[test]
fn deny_inside_the_workspace_is_unavailable_naming_the_path() {
    let dir = fixture("fiber-sandbox-overlap");
    let workspace = dir.path().join("workspace");
    let denied = workspace.join("secret");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(&denied, "secret").expect("write secret");
    let mut settings = workspace_settings(&dir.path().join("fiber-home"));
    settings.deny_read.push(denied.clone());
    let unavailable = check_available(&settings, &workspace).expect_err("overlap is unavailable");
    assert_eq!(
        unavailable,
        Unavailable::DenyInsideWrite {
            configured: denied.clone(),
            overlapping: denied.clone(),
            root: workspace.clone(),
        }
    );
    assert_eq!(unavailable.code(), contract::ErrorCode::InvalidArguments);
    // The lexical deny overlaps, so no arrow follows the configured path.
    let notice = unavailable.to_string();
    assert!(
        notice.contains(&*denied.to_string_lossy()),
        "names {notice}"
    );
    assert!(
        notice.contains(&*workspace.to_string_lossy()),
        "names {notice}"
    );
    assert!(!notice.contains('\u{2192}'), "no target in {notice}");
}

#[cfg(target_os = "linux")]
#[test]
fn deny_inside_allow_write_is_unavailable() {
    let dir = fixture("fiber-sandbox-allow-overlap");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let extra = dir.path().join("extra-write");
    std::fs::create_dir_all(&extra).expect("create extra write");
    let denied = extra.join("secret");
    std::fs::write(&denied, "secret").expect("write secret");
    let mut settings = workspace_settings(&dir.path().join("fiber-home"));
    settings.allow_write.push(extra.clone());
    settings.deny_read.push(denied.clone());
    let unavailable = check_available(&settings, &workspace).expect_err("overlap is unavailable");
    assert_eq!(
        unavailable,
        Unavailable::DenyInsideWrite {
            configured: denied.clone(),
            overlapping: denied,
            root: extra,
        }
    );
    assert_eq!(unavailable.code(), contract::ErrorCode::InvalidArguments);
}

#[cfg(target_os = "linux")]
#[test]
fn deny_through_an_aliased_write_root_is_unavailable() {
    let dir = fixture("fiber-sandbox-alias-overlap");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let real = dir.path().join("real-write");
    std::fs::create_dir_all(&real).expect("create real write");
    let alias = dir.path().join("alias-write");
    std::os::unix::fs::symlink(&real, &alias).expect("link the alias");
    let denied = real.join("secret");
    std::fs::write(&denied, "secret").expect("write secret");
    let mut settings = workspace_settings(&dir.path().join("fiber-home"));
    settings.allow_write.push(alias);
    settings.deny_read.push(denied.clone());
    let unavailable = check_available(&settings, &workspace).expect_err("overlap is unavailable");
    // The lexical roots miss the deny; the resolved write root catches it.
    assert_eq!(
        unavailable,
        Unavailable::DenyInsideWrite {
            configured: denied.clone(),
            overlapping: denied,
            root: real,
        }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn resolved_target_inside_a_write_grant_is_unavailable_with_an_arrow() {
    let dir = fixture("fiber-sandbox-target-overlap");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let grants = dir.path().join("grants");
    std::fs::create_dir_all(&grants).expect("create grants");
    let link = dir.path().join("link");
    let target = grants.join("secret");
    std::fs::write(&target, "secret").expect("write secret");
    std::os::unix::fs::symlink(&target, &link).expect("link to the target");
    let mut settings = workspace_settings(&dir.path().join("fiber-home"));
    settings.allow_write.push(grants.clone());
    settings.deny_read.push(link.clone());
    let unavailable = check_available(&settings, &workspace).expect_err("overlap is unavailable");
    // Only the resolved target overlaps, so the notice carries the arrow.
    assert_eq!(
        unavailable,
        Unavailable::DenyInsideWrite {
            configured: link.clone(),
            overlapping: target.clone(),
            root: grants.clone(),
        }
    );
    assert_eq!(unavailable.code(), contract::ErrorCode::InvalidArguments);
    let notice = unavailable.to_string();
    assert!(notice.contains(&*link.to_string_lossy()), "names {notice}");
    assert!(
        notice.contains(&*target.to_string_lossy()),
        "resolves {notice}"
    );
    assert!(notice.contains('\u{2192}'), "arrows in {notice}");
    assert!(
        notice.contains(&*grants.to_string_lossy()),
        "grants in {notice}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn an_unresolved_write_root_is_unavailable() {
    let dir = fixture("fiber-sandbox-unresolved-write");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let missing = dir.path().join("missing-write");
    let mut settings = workspace_settings(&dir.path().join("fiber-home"));
    settings.allow_write.push(missing.clone());
    let unavailable =
        check_available(&settings, &workspace).expect_err("unresolved is unavailable");
    assert_eq!(unavailable, Unavailable::UnresolvedWrite { root: missing });
    assert_eq!(unavailable.code(), contract::ErrorCode::ToolError);
}

#[cfg(target_os = "linux")]
#[test]
fn denied_paths_compare_as_a_set() {
    let dir = fixture("fiber-sandbox-denied-paths");
    let fiber_home = dir.path().join("fiber-home");
    let denied: Vec<PathBuf> = vec![dir.path().join("b"), dir.path().join("a")];
    let settings = Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: denied.clone(),
        fiber_home: fiber_home.clone(),
    };
    let mut expected = BTreeSet::new();
    expected.insert(fiber_home.join("credentials"));
    expected.extend(denied);
    assert_eq!(
        denied_paths(&settings).into_iter().collect::<BTreeSet<_>>(),
        expected
    );
}

/// A fixture home with one denied file and readable siblings. The workspace
/// the sandbox grants is a sibling directory: the deny never overlaps it.
#[cfg(target_os = "linux")]
struct HomeFixture {
    dir: fakes::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    settings: Settings,
}

#[cfg(target_os = "linux")]
impl HomeFixture {
    fn new(prefix: &str, deny: &[&str]) -> Self {
        let dir = fixture(prefix);
        let home = dir.path().join("home");
        let workspace = dir.path().join("workspace");
        let fiber_home = dir.path().join("fiber-home");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let mut settings = workspace_settings(&fiber_home);
        settings.deny_read = deny.iter().map(|rel| dir.path().join(rel)).collect();
        Self {
            dir,
            home,
            workspace,
            settings,
        }
    }

    fn confined(&self, command: &str) -> super::super::command::Finished {
        use super::super::command::{MovePolicy, execute};
        let clock = fakes::clock::FakeClock::new();
        let cancel = fakes::CancelToken::new();
        let ran = execute(
            std::path::Path::new("/bin/sh"),
            command,
            &self.home,
            Duration::from_secs(60),
            clock.as_ref(),
            &cancel,
            &fakes::Recorder::default(),
            MovePolicy::Stay,
            None,
            None,
            None,
            &self.settings,
            &self.workspace,
        )
        .expect("a confined spawn succeeds");
        let super::super::command::Ran::Finished(finished) = ran else {
            panic!("a Stay call finishes in the foreground");
        };
        finished
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_sibling_of_a_denied_file_stays_readable() {
    let mut fixture = HomeFixture::new("fiber-sandbox-sibling", &["home/secret"]);
    std::fs::write(fixture.home.join("secret"), "shh").expect("write secret");
    std::fs::write(fixture.home.join("sibling"), "ok").expect("write sibling");
    // Two denies prove the point of the fix: one walk must never re-grant
    // the other's denied subtree.
    fixture
        .settings
        .deny_read
        .push(fixture.dir.path().join("home/other-secret"));
    let secret = fixture.confined("cat secret");
    assert!(
        secret.status.is_some_and(|status| !status.success()),
        "the denied file stays unreadable"
    );
    let sibling = fixture.confined("cat sibling");
    assert!(sibling.status.is_some_and(|status| status.success()));
    assert!(
        matches!(sibling.output, super::super::command::Kept::Whole(ref bytes) if bytes == b"ok"),
        "the sibling stays readable"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_missing_deny_leaves_the_parents_siblings_readable() {
    let fixture = HomeFixture::new("fiber-sandbox-missing", &["home/.aws"]);
    std::fs::write(fixture.home.join("sibling"), "ok").expect("write sibling");
    // Nothing grants the missing deny point upward: only its existing
    // parent's siblings stay readable.
    let sibling = fixture.confined("cat sibling");
    assert!(sibling.status.is_some_and(|status| status.success()));
    assert!(
        matches!(sibling.output, super::super::command::Kept::Whole(ref bytes) if bytes == b"ok"),
        "the sibling stays readable past the missing deny"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_symlink_to_a_directory_denies_its_target_and_keeps_siblings() {
    let dir = fixture("fiber-sandbox-dir-link");
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let workspace = dir.path().join("workspace");
    let fiber_home = dir.path().join("fiber-home");
    for directory in [&home, &data, &workspace] {
        std::fs::create_dir_all(directory).expect("create fixture dir");
    }
    let aws = data.join("aws");
    std::fs::create_dir_all(&aws).expect("create aws");
    std::fs::write(aws.join("file"), "shh").expect("write target file");
    std::fs::write(data.join("other"), "ok").expect("write data sibling");
    std::fs::write(home.join("sibling"), "ok").expect("write home sibling");
    std::os::unix::fs::symlink(&aws, home.join(".aws")).expect("link to the directory");
    let mut settings = workspace_settings(&fiber_home);
    // The deny path traverses `.aws` as a symlink component. The resolved
    // target must stay unreadable by its real path as well as through the link.
    settings.deny_read.push(home.join(".aws/file"));
    let fixture = HomeFixture {
        dir,
        home,
        workspace,
        settings,
    };
    let target = fixture.confined("cat ../data/aws/file");
    assert!(
        target.status.is_some_and(|status| !status.success()),
        "the linked target stays unreadable"
    );
    let through_link = fixture.confined("cat .aws/file");
    assert!(
        through_link.status.is_some_and(|status| !status.success()),
        "the link itself stays unreadable"
    );
    let other = fixture.confined("cat ../data/other");
    assert!(other.status.is_some_and(|status| status.success()));
    assert!(
        matches!(other.output, super::super::command::Kept::Whole(ref bytes) if bytes == b"ok"),
        "the target's sibling stays readable"
    );
    let sibling = fixture.confined("cat sibling");
    assert!(sibling.status.is_some_and(|status| status.success()));
    assert!(
        matches!(sibling.output, super::super::command::Kept::Whole(ref bytes) if bytes == b"ok"),
        "the home sibling stays readable"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_dangling_directory_link_with_a_suffix_keeps_siblings_readable() {
    use std::os::unix::fs::symlink;

    let fixture = HomeFixture::new("fiber-sandbox-dangling-suffix", &["home/link/secret"]);
    symlink("missing-directory", fixture.home.join("link")).expect("dangle the directory link");
    std::fs::write(fixture.home.join("sibling"), "ok").expect("write sibling");
    let sibling = fixture.confined("cat sibling");
    assert!(sibling.status.is_some_and(|status| status.success()));
    assert!(
        matches!(sibling.output, super::super::command::Kept::Whole(ref bytes) if bytes == b"ok"),
        "the dangling deny leaves its sibling readable"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_dangling_link_created_after_setup_stays_unreadable() {
    let fixture = HomeFixture::new("fiber-sandbox-dangling", &["home/link"]);
    // The link dangles at setup; its target arrives after, and the confined
    // read still cannot reach it.
    std::os::unix::fs::symlink("target", fixture.home.join("link")).expect("dangle the link");
    std::fs::write(fixture.home.join("sibling"), "ok").expect("write sibling");
    std::fs::write(fixture.home.join("target"), "shh").expect("create the target after setup");
    let target = fixture.confined("cat target");
    assert!(
        target.status.is_some_and(|status| !status.success()),
        "the late target stays unreadable"
    );
    let sibling = fixture.confined("cat sibling");
    assert!(sibling.status.is_some_and(|status| status.success()));
    assert!(
        matches!(sibling.output, super::super::command::Kept::Whole(ref bytes) if bytes == b"ok"),
        "the sibling stays readable"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_created_missing_deny_stays_unreadable_during_the_call() {
    use super::super::command::{MovePolicy, execute};
    let dir = fixture("fiber-sandbox-created-missing");
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let fiber_home = dir.path().join("fiber-home");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(home.join("sibling"), "ok").expect("write sibling");
    let mut settings = workspace_settings(&fiber_home);
    settings.deny_read.push(home.join(".aws/credentials"));
    // The handshake fifos live in the granted workspace: the confined side
    // could never open them under the denied home.
    let ready = workspace.join("ready");
    let go = workspace.join("go");
    for fifo in [&ready, &go] {
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .expect("make the handshake fifo");
    }
    let script = format!(
        "echo ready > {}; read go < {}; cat .aws/credentials; echo \"secret-rc=$?\"; cat sibling",
        ready.display(),
        go.display()
    );
    // The confined command blocks on its handshake fifo; the unconfined
    // test creates the missing deny path, then releases it. Pipes, not
    // sleeps, order the two sides.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn({
        let home = home.clone();
        let workspace = workspace.clone();
        move || {
            let clock = fakes::clock::FakeClock::new();
            let cancel = fakes::CancelToken::new();
            let ran = execute(
                std::path::Path::new("/bin/sh"),
                &script,
                &home,
                Duration::from_secs(60),
                clock.as_ref(),
                &cancel,
                &fakes::Recorder::default(),
                MovePolicy::Stay,
                None,
                None,
                None,
                &settings,
                &workspace,
            )
            .expect("a confined spawn succeeds");
            let super::super::command::Ran::Finished(finished) = ran else {
                panic!("a Stay call finishes in the foreground");
            };
            let _sent = tx.send(finished);
        }
    });
    // `ready` opens only once the confined side writes; `go` opens only
    // once it reads, so the secret lands between the two.
    let mut ready = std::fs::File::open(&ready).expect("the confined side signals ready");
    {
        use std::io::BufRead as _;
        let mut line = String::new();
        std::io::BufReader::new(&mut ready)
            .read_line(&mut line)
            .expect("read the ready line");
        assert_eq!(line.trim(), "ready");
    }
    let aws = home.join(".aws");
    std::fs::create_dir_all(&aws).expect("create the missing deny parent");
    std::fs::write(aws.join("credentials"), "shh").expect("create the missing deny file");
    std::fs::write(&go, "go\n").expect("release the confined side");
    let finished = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the confined call ends within the deadline");
    let super::super::command::Kept::Whole(bytes) = finished.output else {
        panic!("a small output stays whole");
    };
    let text = String::from_utf8(bytes).expect("the output is text");
    assert!(
        text.contains("secret-rc=1"),
        "the created deny stays unreadable: {text}"
    );
    assert!(text.ends_with("ok"), "the sibling stays readable: {text}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_symlink_loop_makes_the_sandbox_unavailable() {
    let dir = fixture("fiber-sandbox-loop");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::os::unix::fs::symlink("b", &a).expect("link a to b");
    std::os::unix::fs::symlink("a", &b).expect("link b to a");
    let mut settings = workspace_settings(&dir.path().join("fiber-home"));
    settings.deny_read.push(a.clone());
    let unavailable = check_available(&settings, &workspace).expect_err("a loop is unavailable");
    assert_eq!(unavailable, Unavailable::UnresolvableDeny { configured: a });
    assert_eq!(unavailable.code(), contract::ErrorCode::ToolError);
}

#[cfg(target_os = "linux")]
#[test]
fn a_permission_denied_during_deny_resolution_makes_the_sandbox_unavailable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fixture("fiber-sandbox-unresolvable-deny");
    let workspace = dir.path().join("workspace");
    let locked = dir.path().join("locked");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir(&locked).expect("create locked directory");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o0))
        .expect("hide locked directory");
    let denied = locked.join("secret");
    let mut settings = workspace_settings(&dir.path().join("fiber-home"));
    settings.deny_read.push(denied.clone());

    let result = check_available(&settings, &workspace);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700))
        .expect("restore locked directory permissions");
    assert_eq!(
        result.expect_err("a permission error is not a missing path"),
        Unavailable::UnresolvableDeny { configured: denied }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn forty_hops_resolve_and_forty_one_do_not() {
    use std::os::unix::fs::symlink;
    for (prefix, links, available) in [
        ("fiber-sandbox-hops-ok", 40, true),
        ("fiber-sandbox-hops-over", 41, false),
    ] {
        let dir = fixture(prefix);
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let real = dir.path().join("real");
        std::fs::write(&real, "shh").expect("write the target");
        let mut next = real.clone();
        for hop in 0..links {
            let link = dir.path().join(format!("l{hop}"));
            symlink(&next, &link).expect("chain the link");
            next = link;
        }
        let target = super::resolve_target(&next);
        assert_eq!(
            target.is_ok(),
            available,
            "resolve_target accepts exactly 40 endpoint hops"
        );
        if available {
            assert_eq!(target.expect("40 hops resolve"), Some(real.clone()));
        }
        let mut settings = workspace_settings(&dir.path().join("fiber-home"));
        settings.deny_read.push(next.clone());
        let result = check_available(&settings, &workspace);
        assert_eq!(
            result.is_ok(),
            available,
            "{links} hops resolve within the budget"
        );
        if !available {
            assert_eq!(
                result.expect_err("over budget is unavailable"),
                Unavailable::UnresolvableDeny { configured: next }
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn forty_symlink_components_resolve_and_forty_one_fail() {
    use std::os::unix::fs::symlink;

    for (links, available) in [(40, true), (41, false)] {
        let dir = fixture("fiber-sandbox-component-hops");
        let start = dir.path().join("start");
        std::fs::create_dir(&start).expect("create start directory");
        let mut current = start.clone();
        for hop in 0..links {
            let target = dir.path().join(format!("target-{hop}"));
            std::fs::create_dir(&target).expect("create target directory");
            symlink(&target, current.join("next")).expect("link to the next directory");
            current = target;
        }
        let mut path = start;
        for _ in 0..links {
            path.push("next");
        }

        let result = super::resolve_target(&path);

        assert_eq!(
            result.is_ok(),
            available,
            "resolve_target accepts exactly 40 symlink components"
        );
        if available {
            assert_eq!(result.expect("40 components resolve"), Some(current));
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn open_nofollow_never_resolves_a_symlink() {
    use std::os::unix::fs::symlink;
    use std::os::unix::io::AsFd as _;
    let dir = fixture("fiber-sandbox-open");
    let file = dir.path().join("file");
    std::fs::write(&file, "data").expect("write the file");
    let link = dir.path().join("link");
    symlink(&file, &link).expect("link to the file");
    // `O_PATH` opens the link itself without resolving it: the descriptor
    // names the link's inode, never the target's.
    let fd = super::open_nofollow(&link).expect("a link opens as a link");
    let opened = rustix::fs::fstat(rustix::fd::AsFd::as_fd(&fd)).expect("stat the descriptor");
    let link_meta = rustix::fs::lstat(&link).expect("stat the link");
    let file_meta = rustix::fs::stat(&file).expect("stat the target");
    assert_eq!(opened.st_ino, link_meta.st_ino);
    assert_ne!(opened.st_ino, file_meta.st_ino);
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).expect("create the dir");
    for path in [&file, &sub] {
        let fd = super::open_nofollow(path).expect("a real entry opens");
        // Dropping `O_CLOEXEC` clears the flag: the check below kills it.
        let flags = rustix::io::fcntl_getfd(fd.as_fd()).expect("read the descriptor flags");
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
    }
    // Dropping `O_PATH` lets reads through: this read must fail.
    let fd = super::open_nofollow(&file).expect("a real file opens");
    let mut byte = [0u8; 1];
    let result = rustix::io::read(rustix::fd::AsFd::as_fd(&fd), &mut byte);
    assert_eq!(
        result,
        Err(rustix::io::Errno::BADF),
        "an O_PATH descriptor never reads"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn errno_of_names_the_syscall_errno() {
    assert_eq!(
        super::errno_of(&std::io::Error::from_raw_os_error(libc::EPERM)),
        libc::EPERM
    );
    // An error with no operating-system cause falls back to EIO.
    assert_eq!(super::errno_of(&std::fmt::Error), libc::EIO);
}

/// Walks `denied` under `root` with `clock` and returns the grant count and
/// the walk time. Fixtures pass their own root, so the count depends only
/// on the fixture; production passes `/`. Reports the time to the caller
/// but never asserts it (`docs/testing.md`, "No time-based assertions").
#[cfg(target_os = "linux")]
pub(crate) fn measure_read_walk(
    root: &Path,
    denied: &[PathBuf],
    clock: &dyn contract::clock::Clock,
) -> (usize, Duration) {
    let start = clock.now();
    let mut rules = Vec::new();
    for deny in denied {
        super::add_ancestor_grants(deny, denied, landlock::ABI::V2, root, &mut rules)
            .expect("a fixture walk succeeds");
    }
    let elapsed = clock.now().saturating_duration_since(start);
    (rules.len(), elapsed)
}

#[cfg(target_os = "linux")]
#[test]
fn a_fixture_walk_counts_every_grant_exactly() {
    let dir = fixture("fiber-sandbox-count");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("create home");
    for name in ["secret", "first", "second", "third"] {
        std::fs::write(home.join(name), name).expect("write the fixture file");
    }
    let denied = [home.join("secret")];
    let clock = fakes::clock::FakeClock::new();
    let (count, _elapsed) = measure_read_walk(dir.path(), &denied, clock.as_ref());
    // The walk grants `home`'s three readable siblings and nothing else:
    // the denied entry, its missing parents and the root's other entries
    // stay out. A mutant returning 0 or 1 fails here.
    assert_eq!(count, 3);
}

#[cfg(target_os = "linux")]
#[allow(
    clippy::print_stdout,
    reason = "the pull-request body reports this line"
)]
#[test]
fn the_owner_home_walk_reports_its_count_and_time() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        panic!("HOME names the owner's home");
    };
    // The system clock reports the real walk time for the pull-request
    // body; nothing asserts on it (`docs/testing.md`, "No time-based
    // assertions").
    let clock = fakes::clock::SystemClock;
    let (count, elapsed) =
        measure_read_walk(&PathBuf::from("/"), &[home.join("credentials")], &clock);
    assert!(count > 1, "a home walk grants more than one rule");
    // Reported for the pull-request body, never asserted.
    println!("home sandbox walk: {count} read grants in {elapsed:?}");
}
