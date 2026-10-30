//! The OS sandbox policy: unavailable cases, the read-deny contract proved
//! by confined commands, and the walk measurement helper. Tests that need
//! Landlock or a confined spawn run only on Linux, and Seatbelt ones only
//! on macOS; the unavailable variants they assert never arise elsewhere.

#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Duration;

#[cfg(target_os = "linux")]
use super::denied_paths;
#[cfg(target_os = "macos")]
use super::{
    MarkerWatch, StripMarker, apply_cause, check_available_with_exec, marker_token, prepare,
    wrap_command,
};
use super::{Settings, Unavailable, check_available};

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
    #[cfg(target_os = "macos")]
    check_available(&settings, &workspace).expect("no overlap is available");
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
fn a_relative_deny_inside_the_workspace_is_unavailable() {
    struct RestoreCwd(Option<PathBuf>);
    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                drop(std::env::set_current_dir(previous));
            }
        }
    }
    let dir = fixture("fiber-sandbox-relative-overlap");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(workspace.join("secret"), "secret").expect("write secret");
    let previous = std::env::current_dir().expect("read the working directory");
    let _restore = RestoreCwd(Some(previous));
    std::env::set_current_dir(dir.path()).expect("enter the fixture root");
    let relative = PathBuf::from("workspace/secret");
    let mut settings = workspace_settings(&dir.path().join("fiber-home"));
    settings.deny_read.push(relative.clone());
    let unavailable = check_available(&settings, &workspace).expect_err("overlap is unavailable");
    let lexical = workspace.join("secret");
    assert_eq!(
        unavailable,
        Unavailable::DenyInsideWrite {
            configured: relative.clone(),
            overlapping: lexical.clone(),
            root: workspace.clone(),
        }
    );
    assert_eq!(unavailable.code(), contract::ErrorCode::InvalidArguments);
    let notice = unavailable.to_string();
    assert!(
        notice.contains(&*relative.to_string_lossy()),
        "names the configured spelling in {notice}"
    );
    assert!(
        notice.contains(&*lexical.to_string_lossy()),
        "names the normalized path in {notice}"
    );
    assert!(notice.contains('\u{2192}'), "arrows in {notice}");
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
fn a_symlink_to_a_file_denies_both_paths_and_keeps_siblings() {
    let dir = fixture("fiber-sandbox-file-link");
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let workspace = dir.path().join("workspace");
    let fiber_home = dir.path().join("fiber-home");
    for directory in [&home, &data, &workspace] {
        std::fs::create_dir_all(directory).expect("create fixture dir");
    }
    let secret = b"shh-file-secret";
    std::fs::write(data.join("secret"), secret).expect("write target file");
    std::fs::write(data.join("other"), "ok").expect("write data sibling");
    std::fs::write(home.join("sibling"), "ok").expect("write home sibling");
    std::os::unix::fs::symlink("../data/secret", home.join("link")).expect("link to the file");
    let mut settings = workspace_settings(&fiber_home);
    // The deny names the file symlink itself; the resolved target must stay
    // unreadable by its real path as well as through the link.
    settings.deny_read.push(home.join("link"));
    let fixture = HomeFixture {
        dir,
        home,
        workspace,
        settings,
    };
    for command in ["cat link", "cat ../data/secret"] {
        let denied = fixture.confined(command);
        assert!(
            denied.status.is_some_and(|status| !status.success()),
            "{command} stays unreadable"
        );
        let super::super::command::Kept::Whole(bytes) = &denied.output else {
            panic!("{command} keeps a small output whole");
        };
        assert!(
            bytes.windows(secret.len()).all(|window| window != secret),
            "{command} leaks no secret bytes"
        );
    }
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

#[cfg(target_os = "linux")]
#[test]
fn setup_error_maps_each_case_to_its_code() {
    use super::SetupError;
    // An unavailable policy keeps its own code: off is a tool error,
    // while a deny inside a writable grant is a bad configuration. A
    // mutant replacing the delegation with one fixed code fails one side.
    let off = SetupError::Unavailable(Unavailable::Off);
    assert_eq!(off.code(), contract::ErrorCode::ToolError);
    let overlap = SetupError::Unavailable(Unavailable::DenyInsideWrite {
        configured: PathBuf::from("/work/secret"),
        overlapping: PathBuf::from("/work/secret"),
        root: PathBuf::from("/work"),
    });
    assert_eq!(overlap.code(), contract::ErrorCode::InvalidArguments);
    // Every setup failure is a tool error: a mutant mapping one case
    // elsewhere fails its line below.
    let resolve = SetupError::Resolve {
        label: "the workspace",
        path: PathBuf::from("/work"),
        source: std::io::Error::from_raw_os_error(libc::ENOENT),
    };
    assert_eq!(resolve.code(), contract::ErrorCode::ToolError);
    assert!(
        resolve.to_string().contains("/work"),
        "the resolve message names the root: {resolve}"
    );
    let open = SetupError::Open {
        path: PathBuf::from("/work/grant"),
        source: std::io::Error::from_raw_os_error(libc::ENOENT),
    };
    assert_eq!(open.code(), contract::ErrorCode::ToolError);
    assert!(
        open.to_string().contains("/work/grant"),
        "the open message names the file: {open}"
    );
    let list = SetupError::List {
        path: PathBuf::from("/work/ancestor"),
        source: std::io::Error::from_raw_os_error(libc::ENOENT),
    };
    assert_eq!(list.code(), contract::ErrorCode::ToolError);
    assert!(
        list.to_string().contains("/work/ancestor"),
        "the list message names the directory: {list}"
    );
    let ruleset = SetupError::Ruleset {
        detail: "landlock failed".to_owned(),
    };
    assert_eq!(ruleset.code(), contract::ErrorCode::ToolError);
    let grant = SetupError::AddGrant {
        detail: "landlock failed".to_owned(),
    };
    assert_eq!(grant.code(), contract::ErrorCode::ToolError);
    let seccomp = SetupError::Seccomp {
        detail: "unknown architecture for seccomp".to_owned(),
    };
    assert_eq!(seccomp.code(), contract::ErrorCode::ToolError);
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

/// A macOS fixture: an enabled sandbox with no overlap, so `prepare`
/// builds a profile. Runs only on macOS, where `sandbox-exec` ships.
#[cfg(target_os = "macos")]
struct MacosFixture {
    dir: fakes::TempDir,
    workspace: PathBuf,
    settings: Settings,
}

#[cfg(target_os = "macos")]
impl MacosFixture {
    fn new(prefix: &str) -> Self {
        let dir = fixture(prefix);
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let settings = workspace_settings(&dir.path().join("fiber-home"));
        Self {
            dir,
            workspace,
            settings,
        }
    }

    fn binding<'a>(prepared: &'a super::Prepared, name: &str) -> &'a str {
        prepared
            .bindings
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("a {name} binding exists"))
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_enabled_without_overlap_is_available() {
    let fixture = MacosFixture::new("fiber-sandbox-macos-available");
    check_available(&fixture.settings, &fixture.workspace)
        .expect("overlap-free settings are available on macOS");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_missing_executable_reports_the_binary() {
    let fixture = MacosFixture::new("fiber-sandbox-macos-missing-exec");
    let missing = fixture.dir.path().join("no-sandbox-exec");
    let unavailable = check_available_with_exec(&fixture.settings, &fixture.workspace, &missing)
        .expect_err("a missing binary is unavailable");
    assert_eq!(
        unavailable,
        Unavailable::SandboxExecUnavailable {
            path: missing.clone()
        }
    );
    assert_eq!(unavailable.code(), contract::ErrorCode::ToolError);
    assert!(
        unavailable
            .to_string()
            .contains(&*missing.to_string_lossy()),
        "the notice names the binary it looked for: {unavailable}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_a_directory_as_the_executable_is_unavailable() {
    let fixture = MacosFixture::new("fiber-sandbox-macos-dir-exec");
    let unavailable =
        check_available_with_exec(&fixture.settings, &fixture.workspace, fixture.dir.path())
            .expect_err("a directory is not a usable binary");
    assert!(
        matches!(unavailable, Unavailable::SandboxExecUnavailable { .. }),
        "a directory reports the missing-binary case: {unavailable}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_deny_inside_write_is_unavailable_with_a_generic_label() {
    let mut fixture = MacosFixture::new("fiber-sandbox-macos-overlap");
    let denied = fixture.workspace.join("secret");
    std::fs::write(&denied, "secret").expect("write secret");
    fixture.settings.deny_read.push(denied.clone());
    let unavailable = check_available(&fixture.settings, &fixture.workspace).expect_err("overlap");
    assert_eq!(
        unavailable,
        Unavailable::DenyInsideWrite {
            configured: denied.clone(),
            overlapping: denied.clone(),
            root: fixture.workspace.clone(),
        }
    );
    assert_eq!(unavailable.code(), contract::ErrorCode::InvalidArguments);
    let notice = unavailable.to_string();
    assert!(
        notice.contains("sandbox read-deny path"),
        "the label names no source: {notice}"
    );
    assert!(
        !notice.contains("sandbox.deny_read"),
        "a credential path gets no deny_read attribution: {notice}"
    );
    assert!(
        notice.contains(&*denied.to_string_lossy()),
        "the notice names the configured path: {notice}"
    );
    assert!(
        notice.contains(&*fixture.workspace.to_string_lossy()),
        "the notice names the writable root: {notice}"
    );
    assert!(!notice.contains('\u{2192}'), "no target in {notice}");
}

/// The actual wrapper rejects an invalid profile before the user program
/// runs: no marker line is observed, so the call fails closed with the
/// wrapper's diagnostic instead of a normal end.
#[cfg(target_os = "macos")]
#[test]
fn macos_rejected_profile_runs_nothing_and_fails_closed() {
    use super::super::command::{MovePolicy, SandboxApply, execute_confined};
    let dir = fixture("fiber-sandbox-macos-rejected");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let side_effect = workspace.join("side-effect");
    let command = format!("touch {}", side_effect.display());
    let clock = fakes::clock::FakeClock::new();
    let cancel = fakes::CancelToken::new();
    let Err(error) = execute_confined(
        std::path::Path::new("/bin/sh"),
        &command,
        &workspace,
        Duration::from_secs(60),
        clock.as_ref(),
        &cancel,
        &fakes::Recorder::default(),
        MovePolicy::Stay,
        None,
        None,
        None,
        support::group::alive,
        std::path::Path::new("ps"),
        "(version 1)\n(deny bogus-operation-for-fiber-test)",
        &[],
    ) else {
        panic!("a rejected profile fails");
    };
    let apply = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<SandboxApply>())
        .expect("a rejected profile maps to a sandbox-apply failure");
    assert!(
        apply.cause.contains("sandbox-exec"),
        "the cause names the wrapper: {}",
        apply.cause
    );
    assert!(
        !side_effect.exists(),
        "the user program never ran past the rejected profile"
    );
}

/// Unavailable settings never reach spawn: the pre-spawn check fails the
/// call with the overlap, and no command runs.
#[cfg(target_os = "macos")]
#[test]
fn macos_unavailable_settings_never_reach_spawn() {
    use super::super::command::{MovePolicy, SandboxApply, execute};
    let mut fixture = MacosFixture::new("fiber-sandbox-macos-no-spawn");
    let missing = fixture.dir.path().join("missing-write");
    fixture.settings.allow_write.push(missing.clone());
    let clock = fakes::clock::FakeClock::new();
    let cancel = fakes::CancelToken::new();
    let Err(error) = execute(
        std::path::Path::new("/bin/sh"),
        "touch side-effect",
        &fixture.workspace,
        Duration::from_secs(60),
        clock.as_ref(),
        &cancel,
        &fakes::Recorder::default(),
        MovePolicy::Stay,
        None,
        None,
        None,
        &fixture.settings,
        &fixture.workspace,
    ) else {
        panic!("unavailable settings fail");
    };
    let apply = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<SandboxApply>())
        .expect("an unavailable sandbox maps to a sandbox-apply failure");
    assert!(
        apply.cause.contains(&*missing.to_string_lossy()),
        "the cause names the unresolvable root: {}",
        apply.cause
    );
    assert!(
        !fixture.workspace.join("side-effect").exists(),
        "no command ran past the failed check"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_prepare_failure_names_the_unresolvable_root() {
    let mut fixture = MacosFixture::new("fiber-sandbox-macos-unresolved");
    let missing = fixture.dir.path().join("missing-write");
    fixture.settings.allow_write.push(missing.clone());
    let error = prepare(&fixture.settings, &fixture.workspace).expect_err("no grant is missing");
    assert!(
        error.to_string().contains(&*missing.to_string_lossy()),
        "the setup error names the root: {error}"
    );
}

/// The profile carries every write grant, every read deny and the git
/// protections, while no dynamic path text reaches the Seatbelt source.
#[cfg(target_os = "macos")]
#[test]
fn macos_profile_grants_roots_denies_and_names_no_paths() {
    let mut fixture = MacosFixture::new("fiber-sandbox-macos-profile");
    std::fs::create_dir_all(fixture.workspace.join(".git")).expect("create git dir");
    let secret = fixture.dir.path().join("secret");
    std::fs::write(&secret, "secret").expect("write secret");
    fixture.settings.deny_read.push(secret.clone());
    let prepared = prepare(&fixture.settings, &fixture.workspace).expect("a profile builds");
    let profile = &prepared.profile;
    for rule in [
        "(deny default)",
        "(allow process-exec)",
        "(allow process-fork)",
        "(allow sysctl-read)",
        "(allow file-read* (subpath \"/\"))",
        "(allow file-read* file-write* file-ioctl",
        "(deny network-outbound)",
        "(deny network-inbound)",
        "(deny network-bind)",
        "(deny system-socket)",
        "launchd",
        "/.git/config",
        "/.git/hooks",
        "/.fiber",
        "(param \"FIBER_WS\")",
        "(param \"FIBER_TMPDIR\")",
        "(param \"FIBER_TMP\")",
    ] {
        assert!(profile.contains(rule), "the profile carries {rule}");
    }
    let workspace = std::fs::canonicalize(&fixture.workspace).expect("resolve workspace");
    let temp = std::fs::canonicalize(std::env::temp_dir()).expect("resolve temp");
    let tmp = std::fs::canonicalize("/tmp").expect("resolve /tmp");
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_WS"),
        workspace.to_str().expect("utf8")
    );
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_TMPDIR"),
        temp.to_str().expect("utf8")
    );
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_TMP"),
        tmp.to_str().expect("utf8")
    );
    for dynamic in [
        workspace.to_string_lossy(),
        temp.to_string_lossy(),
        tmp.to_string_lossy(),
        secret.to_string_lossy(),
    ] {
        assert!(
            !profile.contains(dynamic.as_ref()),
            "no dynamic path text in the profile source: {dynamic}"
        );
    }
    assert!(
        prepared
            .bindings
            .iter()
            .any(|(_, value)| value == &secret.to_string_lossy()),
        "the deny travels as a binding"
    );
}

/// A quote, parentheses and a newline stay literal through `-D`: the
/// profile source never sees them and the binding carries them whole.
#[cfg(target_os = "macos")]
#[test]
fn macos_profile_keeps_hostile_paths_literal() {
    let mut fixture = MacosFixture::new("fiber-sandbox-macos-literal");
    let hostile = fixture.dir.path().join("we'ird (dir)");
    std::fs::create_dir_all(&hostile).expect("create hostile dir");
    let newline = fixture.dir.path().join("line\nbreak");
    std::fs::create_dir_all(&newline).expect("create newline dir");
    fixture.settings.allow_write.push(hostile.clone());
    fixture.settings.deny_read.push(newline.clone());
    let prepared = prepare(&fixture.settings, &fixture.workspace).expect("a profile builds");
    for dynamic in ["we'ird (dir)", "line\nbreak"] {
        assert!(
            !prepared.profile.contains(dynamic),
            "hostile text never reaches the profile source"
        );
    }
    for expected in [&hostile, &newline] {
        assert!(
            prepared
                .bindings
                .iter()
                .any(|(_, value)| Path::new(value) == expected),
            "the binding carries {} whole",
            expected.display()
        );
    }
}

/// A symlink deny binds both spellings: the lexical path and the resolved
/// target the sandbox judges the access by.
#[cfg(target_os = "macos")]
#[test]
fn macos_profile_denies_a_link_and_its_target() {
    use std::os::unix::fs::symlink;
    let mut fixture = MacosFixture::new("fiber-sandbox-macos-link-deny");
    let target = fixture.dir.path().join("target");
    std::fs::write(&target, "shh").expect("write target");
    let link = fixture.dir.path().join("link");
    symlink(&target, &link).expect("link to the target");
    fixture.settings.deny_read.push(link.clone());
    let prepared = prepare(&fixture.settings, &fixture.workspace).expect("a profile builds");
    for expected in [&link, &target] {
        assert!(
            prepared
                .bindings
                .iter()
                .any(|(_, value)| Path::new(value) == expected),
            "the deny binds {}",
            expected.display()
        );
    }
}

/// A checkout's `.git` directory grants commits while its config and hooks
/// stay denied.
#[cfg(target_os = "macos")]
#[test]
fn macos_profile_protects_a_checkout_gitdir() {
    let fixture = MacosFixture::new("fiber-sandbox-macos-checkout");
    let gitdir = fixture.workspace.join(".git");
    std::fs::create_dir_all(&gitdir).expect("create git dir");
    let prepared = prepare(&fixture.settings, &fixture.workspace).expect("a profile builds");
    let resolved = std::fs::canonicalize(&fixture.workspace).expect("resolve workspace");
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_GITDIR"),
        resolved.join(".git").to_str().expect("utf8")
    );
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_COMMONDIR"),
        resolved.join(".git").to_str().expect("utf8")
    );
}

/// A worktree's `.git` file denies the pointer while the outside gitdir and
/// the shared objects, refs and logs stay writable for commits.
#[cfg(target_os = "macos")]
#[test]
fn macos_profile_follows_a_worktree_pointer() {
    let fixture = MacosFixture::new("fiber-sandbox-macos-worktree");
    let gitdir = fixture.dir.path().join("real-gitdir");
    std::fs::create_dir_all(&gitdir).expect("create gitdir");
    std::fs::write(gitdir.join("commondir"), "../common").expect("write commondir");
    let common = fixture.dir.path().join("common");
    for name in ["objects", "refs", "logs", "hooks"] {
        std::fs::create_dir_all(common.join(name)).expect("create common dir");
    }
    std::fs::write(
        fixture.workspace.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .expect("write the worktree pointer");
    let prepared = prepare(&fixture.settings, &fixture.workspace).expect("a profile builds");
    let gitdir_resolved = std::fs::canonicalize(&gitdir).expect("resolve gitdir");
    let common_resolved = std::fs::canonicalize(&common).expect("resolve common");
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_GITDIR"),
        gitdir_resolved.to_str().expect("utf8")
    );
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_COMMONDIR"),
        common_resolved.to_str().expect("utf8")
    );
    for rule in [
        "(allow file-write* (subpath (param \"FIBER_GITDIR\")))",
        "(deny file-write* (subpath (string-append (param \"FIBER_COMMONDIR\") \"/hooks\")))",
        "(deny file-write* (literal (string-append (param \"FIBER_COMMONDIR\") \"/config\")))",
        "(deny file-write* (literal (string-append (param \"FIBER_WS\") \"/.git\")))",
    ] {
        assert!(
            prepared.profile.contains(rule),
            "the profile carries {rule}"
        );
    }
}

/// A hostile `.git` pointer at the filesystem root earns no git write
/// grant: `/` is no recognized git layout, so both directories fall back to
/// the workspace-local `.git` the workspace grant already covers.
#[cfg(target_os = "macos")]
#[test]
fn macos_absolute_git_pointer_at_root_grants_neither() {
    let fixture = MacosFixture::new("fiber-sandbox-macos-hostile-root");
    std::fs::write(fixture.workspace.join(".git"), "gitdir: /\n").expect("write hostile pointer");
    let prepared = prepare(&fixture.settings, &fixture.workspace).expect("a profile builds");
    let workspace = std::fs::canonicalize(&fixture.workspace).expect("resolve workspace");
    let fallback = workspace.join(".git").to_str().expect("utf8").to_owned();
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_GITDIR"),
        fallback,
        "an unverified gitdir falls back inside the workspace"
    );
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_COMMONDIR"),
        fallback,
        "an unverified common dir falls back inside the workspace"
    );
    for (_, value) in &prepared.bindings {
        assert!(
            !["/", "/objects", "/refs", "/logs"].contains(&value.as_str()),
            "no binding names the root or its git entries: {value}"
        );
    }
}

/// A pointer at an existing directory without git entries earns no grant
/// either: only a recognized layout carries the commit paths.
#[cfg(target_os = "macos")]
#[test]
fn macos_git_pointer_at_a_non_git_dir_grants_neither() {
    let fixture = MacosFixture::new("fiber-sandbox-macos-hostile-plain");
    let plain = fixture.dir.path().join("plain");
    std::fs::create_dir_all(&plain).expect("create plain dir");
    std::fs::write(
        fixture.workspace.join(".git"),
        format!("gitdir: {}\n", plain.display()),
    )
    .expect("write hostile pointer");
    let prepared = prepare(&fixture.settings, &fixture.workspace).expect("a profile builds");
    let workspace = std::fs::canonicalize(&fixture.workspace).expect("resolve workspace");
    let fallback = workspace.join(".git").to_str().expect("utf8").to_owned();
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_GITDIR"),
        fallback,
        "a non-git dir falls back inside the workspace"
    );
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_COMMONDIR"),
        fallback,
        "a non-git common dir falls back inside the workspace"
    );
    assert!(
        prepared
            .bindings
            .iter()
            .all(|(_, value)| value != &plain.to_string_lossy()),
        "the plain dir earns no binding"
    );
}

/// A valid gitdir whose `commondir` points at `/` still grants neither: the
/// common directory is validated as a recognized layout too.
#[cfg(target_os = "macos")]
#[test]
fn macos_git_pointer_with_root_commondir_grants_neither() {
    let fixture = MacosFixture::new("fiber-sandbox-macos-hostile-common");
    let gitdir = fixture.dir.path().join("real-gitdir");
    std::fs::create_dir_all(&gitdir).expect("create gitdir");
    std::fs::write(gitdir.join("commondir"), "/\n").expect("write hostile commondir");
    std::fs::write(
        fixture.workspace.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .expect("write the worktree pointer");
    let prepared = prepare(&fixture.settings, &fixture.workspace).expect("a profile builds");
    let workspace = std::fs::canonicalize(&fixture.workspace).expect("resolve workspace");
    let fallback = workspace.join(".git").to_str().expect("utf8").to_owned();
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_GITDIR"),
        fallback,
        "an unverified common dir pulls the gitdir back inside the workspace"
    );
    assert_eq!(
        MacosFixture::binding(&prepared, "FIBER_COMMONDIR"),
        fallback,
        "an unverified common dir falls back inside the workspace"
    );
    for (_, value) in &prepared.bindings {
        assert!(
            !["/", "/objects", "/refs", "/logs"].contains(&value.as_str()),
            "no binding names the root or its git entries: {value}"
        );
    }
}

/// The wrapper prints the marker before the user program; the strip
/// adapter removes exactly that first line, even when the user program
/// echoes the marker text afterwards (`docs/sandbox.md`, "On macOS": the
/// marker line precedes the user program, and the reader strips exactly
/// that first line).
#[cfg(target_os = "macos")]
#[test]
fn macos_marker_wrap_then_strip_is_byte_identical() {
    use std::io::Read as _;
    let token = marker_token();
    let user = "user output\nFIBER_SANDBOX_CONFINED echoed\ntrailing\n";
    let wrapped = wrap_command("ignored", &token);
    assert!(
        wrapped.starts_with(&format!(
            "printf '%s\\n' 'FIBER_SANDBOX_CONFINED {token}'; "
        )),
        "the marker runs first: {wrapped}"
    );
    let watch = std::sync::Arc::new(MarkerWatch::new(&token));
    let output = format!("FIBER_SANDBOX_CONFINED {token}\n{user}");
    let mut strip = StripMarker::new(std::io::Cursor::new(output.into_bytes()), watch.clone());
    let mut kept = Vec::new();
    strip.read_to_end(&mut kept).expect("strip reads");
    assert!(watch.observed(), "the marker is recognized");
    assert_eq!(kept, user.as_bytes(), "user output stays byte-identical");
}

/// A pty translates the marker's newline to carriage-return newline; the
/// adapter still recognizes it and passes the rest through untouched.
#[cfg(target_os = "macos")]
#[test]
fn macos_marker_strip_tolerates_a_pty_carriage_return() {
    use std::io::Read as _;
    let token = marker_token();
    let watch = std::sync::Arc::new(MarkerWatch::new(&token));
    let output = format!("FIBER_SANDBOX_CONFINED {token}\r\nuser\n");
    let mut strip = StripMarker::new(std::io::Cursor::new(output.into_bytes()), watch.clone());
    let mut kept = Vec::new();
    strip.read_to_end(&mut kept).expect("strip reads");
    assert!(watch.observed(), "the carriage-return marker is recognized");
    assert_eq!(kept, b"user\n");
}

/// Observing the marker wakes the parked drive loop: a quiet call offers
/// no later output to wake on, so the observation itself must wake the
/// loop (`docs/sandbox.md`, "On macOS": no move before the marker is
/// observed). The test asserts delivery, not a count; the observable
/// handoff timing is pinned by
/// `macos_confined_quiet_call_hands_off_right_after_its_marker`.
#[cfg(target_os = "macos")]
#[test]
fn macos_marker_observation_wakes_the_parked_loop() {
    let token = marker_token();
    let watch = MarkerWatch::new(&token);
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delivered = fired.clone();
    watch.set_wake(std::sync::Arc::new(move || {
        delivered.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    assert!(
        watch.check(format!("FIBER_SANDBOX_CONFINED {token}").as_bytes()),
        "the marker is recognized"
    );
    assert!(
        fired.load(std::sync::atomic::Ordering::SeqCst),
        "the first observation wakes the loop"
    );
    assert!(
        !watch.check(b"something else"),
        "a later line is not the marker"
    );
    assert!(watch.observed(), "the marker stays observed");
}

/// Without the marker the adapter drains what the wrapper wrote and reports
/// a miss, so the call fails closed with the wrapper's own bytes as cause.
#[cfg(target_os = "macos")]
#[test]
fn macos_missing_marker_drains_as_a_miss() {
    use std::io::Read as _;
    let token = marker_token();
    let watch = std::sync::Arc::new(MarkerWatch::new(&token));
    let diagnostic = "sandbox-exec: unbound variable: bogus\n\nBacktrace: <input>\n";
    let mut strip = StripMarker::new(
        std::io::Cursor::new(diagnostic.as_bytes().to_vec()),
        watch.clone(),
    );
    let mut kept = Vec::new();
    strip.read_to_end(&mut kept).expect("strip reads");
    assert!(!watch.observed(), "no marker means no confinement");
    assert_eq!(
        kept,
        diagnostic.as_bytes(),
        "the diagnostic drains for the cause"
    );
    let cause = apply_cause(&watch, None);
    assert!(
        cause.contains("sandbox-exec: unbound variable: bogus"),
        "the cause names Seatbelt's diagnostic: {cause}"
    );
}

/// A marker that arrives alone in the first reads is not end-of-file: the
/// reader must wait for the user bytes behind it, never close the pipe
/// while the command still runs.
#[cfg(target_os = "macos")]
#[test]
fn macos_marker_arriving_alone_is_not_end_of_file() {
    use std::io::Read as _;
    struct Ones {
        data: Vec<u8>,
        pos: usize,
    }
    impl std::io::Read for Ones {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.data[self.pos];
            self.pos += 1;
            Ok(1)
        }
    }
    let token = marker_token();
    let watch = std::sync::Arc::new(MarkerWatch::new(&token));
    let mut bytes = format!("FIBER_SANDBOX_CONFINED {token}\n").into_bytes();
    bytes.extend_from_slice(b"user\n");
    let mut strip = StripMarker::new(
        Ones {
            data: bytes,
            pos: 0,
        },
        watch.clone(),
    );
    let mut kept = Vec::new();
    strip.read_to_end(&mut kept).expect("strip reads");
    assert!(watch.observed(), "the split marker is recognized");
    assert_eq!(kept, b"user\n", "the reader waits past a lone marker");
}

/// A first line without a newline past the stash budget fails closed too.
/// The expected marker is fixed and short, so such a line is never the
/// marker; the budget only bounds the buffering. At, just below and just
/// above the edge the call fails closed identically, and the stream stays
/// byte-identical: the boundary must neither lose nor duplicate a byte.
#[cfg(target_os = "macos")]
#[test]
fn macos_a_line_past_the_stash_budget_is_a_miss() {
    use std::io::Read as _;
    /// Yields at most `chunk` bytes per read, so the stash crosses the
    /// budget mid-prefix instead of in one read.
    struct Chunks {
        data: Vec<u8>,
        pos: usize,
        chunk: usize,
    }
    impl std::io::Read for Chunks {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let end = (self.pos + self.chunk)
                .min(self.data.len())
                .min(self.pos + buf.len());
            let count = end - self.pos;
            buf[..count].copy_from_slice(&self.data[self.pos..end]);
            self.pos = end;
            Ok(count)
        }
    }
    for length in [
        super::MARKER_STASH_CAP - 1,
        super::MARKER_STASH_CAP,
        super::MARKER_STASH_CAP + 1,
    ] {
        let token = marker_token();
        let watch = std::sync::Arc::new(MarkerWatch::new(&token));
        let mut bytes = vec![b'x'; length];
        bytes.extend_from_slice(b"tail\n");
        let input = bytes.clone();
        let mut strip = StripMarker::new(
            Chunks {
                data: bytes,
                pos: 0,
                chunk: 1024,
            },
            watch.clone(),
        );
        let mut kept = Vec::new();
        strip.read_to_end(&mut kept).expect("strip reads");
        assert!(
            !watch.observed(),
            "a {length}-byte first line is never the marker"
        );
        assert_eq!(
            kept, input,
            "the boundary keeps a {length}-byte prefix byte-identical"
        );
    }
}
