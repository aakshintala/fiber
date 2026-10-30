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
            None,
        )
        .expect("a confined spawn succeeds");
        let super::super::command::Ran::Finished(finished, _) = ran else {
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
                None,
            )
            .expect("a confined spawn succeeds");
            let super::super::command::Ran::Finished(finished, _) = ran else {
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
        error.contains(&*missing.to_string_lossy()),
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

// The protected paths across confined calls: one baseline per project,
// compare, restore and report at a call's end. The production helper is
// the only seam these tests use.

use super::protected::{ProtectedPaths, Report};

/// A Fiber home for the protection state, beside the fixture.
fn protected_home(dir: &fakes::TempDir) -> std::path::PathBuf {
    dir.path().join("fiber-home")
}

/// Opens the handle for `workspace`, failing the test on a setup error.
fn open_handle(
    home: &std::path::Path,
    workspace: &std::path::Path,
) -> std::sync::Arc<ProtectedPaths> {
    let (handle, startup) = ProtectedPaths::open(home, workspace).expect("open recovers");
    assert!(
        startup.is_none(),
        "a fresh project recovers nothing, got {startup:?}"
    );
    handle
}

/// The baseline file of the one project under `home`.
fn baseline_file(home: &std::path::Path) -> std::path::PathBuf {
    let projects = home.join("projects");
    let mut found = Vec::new();
    for project in std::fs::read_dir(&projects).expect("projects lists") {
        let project = project.expect("project entry reads");
        let candidate = project.path().join("protected-paths").join("baseline.json");
        if candidate.is_file() {
            found.push(candidate);
        }
    }
    assert_eq!(found.len(), 1, "one project holds one baseline");
    found.pop().expect("one baseline")
}

/// A linked worktree on a detached head at `name` under the repository.
fn linked(repo: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = repo.join(name);
    fakes::git::git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            path.to_str().expect("path is UTF-8"),
        ],
    );
    path
}

#[test]
fn baseline_covers_shared_state_once_and_each_worktree_pointer_and_fiber() {
    let repo = fakes::git::repo("fiber-protected-baseline");
    let other = linked(repo.path(), "wt1");
    let home = protected_home(&repo);
    let handle = open_handle(&home, repo.path());
    let begin = handle.begin(repo.path()).expect("begin snapshots");
    let raw = std::fs::read_to_string(baseline_file(&home)).expect("baseline reads");
    let baseline: serde_json::Value = serde_json::from_str(&raw).expect("baseline is JSON");
    let entries = baseline
        .get("entries")
        .and_then(|entries| entries.as_array())
        .expect("baseline holds entries");
    let paths: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry.get("path").and_then(|path| path.as_str()))
        .collect();
    let main_git = repo.path().join(".git");
    let other_git = other.join(".git");
    assert!(
        paths.iter().any(|path| path.ends_with("/config")),
        "the shared config is recorded, got {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path.ends_with("/hooks")),
        "the shared hooks are recorded, got {paths:?}"
    );
    assert!(
        !paths.iter().any(|path| Path::new(path) == main_git),
        "the main checkout's own directory is never snapshotted, got {paths:?}"
    );
    assert!(
        paths.iter().any(|path| Path::new(path) == other_git),
        "the linked worktree's pointer is recorded, got {paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|path| Path::new(path) == repo.path().join(".fiber")),
        "the main checkout's fiber dir is recorded, got {paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|path| Path::new(path) == other.join(".fiber")),
        "the linked worktree's fiber dir is recorded, got {paths:?}"
    );
    let shared: Vec<&&str> = paths
        .iter()
        .filter(|path| path.ends_with("/config") || path.ends_with("/hooks"))
        .collect();
    assert_eq!(
        shared.len(),
        2,
        "shared state is stored once, got {paths:?}"
    );
    let report = begin.guard.finish();
    assert!(report.is_empty(), "an untouched call reports nothing");
}

#[test]
fn a_call_from_another_worktree_reuses_the_same_baseline() {
    let repo = fakes::git::repo("fiber-protected-shared");
    let other = linked(repo.path(), "wt1");
    let home = protected_home(&repo);
    let handle = open_handle(&home, repo.path());
    let first = handle.begin(repo.path()).expect("first call snapshots");
    let before = std::fs::read(baseline_file(&home)).expect("baseline reads");
    let second = handle.begin(&other).expect("second call reuses");
    let after = std::fs::read(baseline_file(&home)).expect("baseline reads");
    assert_eq!(before, after, "an active call fixes the baseline");
    assert!(
        second.guard.recovered.is_empty(),
        "a reused baseline recovers nothing"
    );
    let first_report = first.guard.finish();
    assert!(first_report.is_empty(), "an untouched call reports nothing");
    let second_report = second.guard.finish();
    assert!(
        second_report.is_empty(),
        "an untouched call reports nothing"
    );
}

#[test]
fn changed_bytes_new_entries_deletions_and_modes_are_restored() {
    let workspace = fakes::TempDir::new("fiber-protected-restore");
    let fiber = workspace.path().join(".fiber");
    std::fs::create_dir_all(fiber.join("sub")).expect("fiber dir created");
    std::fs::write(fiber.join("keep.txt"), b"original").expect("file written");
    std::fs::write(fiber.join("sub").join("nested.txt"), b"nested").expect("file written");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    // A confined call's damage: changed bytes, a new file, a new directory,
    // a deletion, a file swapped for a directory, and changed permissions.
    std::fs::write(fiber.join("keep.txt"), b"changed").expect("file changed");
    std::fs::write(fiber.join("added.txt"), b"new").expect("file added");
    std::fs::create_dir_all(fiber.join("newdir")).expect("dir added");
    std::fs::remove_file(fiber.join("sub").join("nested.txt")).expect("file deleted");
    std::fs::write(fiber.join("sub").join("other.txt"), b"other").expect("file written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            fiber.join("keep.txt"),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("mode changed");
    }
    let report = begin.guard.finish();
    assert_eq!(
        std::fs::read(fiber.join("keep.txt")).expect("file reads"),
        b"original",
        "changed bytes are restored"
    );
    assert!(!fiber.join("added.txt").exists(), "a new file is removed");
    assert!(!fiber.join("newdir").exists(), "a new dir is removed");
    assert_eq!(
        std::fs::read(fiber.join("sub").join("nested.txt")).expect("file reads"),
        b"nested",
        "a deletion is recreated"
    );
    assert!(
        !fiber.join("sub").join("other.txt").exists(),
        "a new nested file is removed"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(fiber.join("keep.txt"))
            .expect("file stats")
            .permissions()
            .mode()
            & 0o7777;
        assert_ne!(mode, 0o600, "changed permissions are restored");
    }
    assert_eq!(report.restored, vec![".fiber".to_owned()]);
    assert!(
        report.failed.is_empty(),
        "nothing fails, got {:?}",
        report.failed
    );
    assert_eq!(
        report.notice_lines().len(),
        1,
        "one notice per changed path"
    );
}

#[test]
fn created_paths_for_missing_entries_are_removed() {
    let workspace = fakes::TempDir::new("fiber-protected-missing");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    let fiber = workspace.path().join(".fiber");
    std::fs::create_dir_all(fiber.join("made")).expect("dir created");
    std::fs::write(fiber.join("made").join("file.txt"), b"x").expect("file written");
    let report = begin.guard.finish();
    assert!(!fiber.exists(), "a created fiber dir is removed");
    assert_eq!(report.restored, vec![".fiber".to_owned()]);
}

#[test]
fn an_untouched_call_reports_nothing() {
    let workspace = fakes::TempDir::new("fiber-protected-quiet");
    std::fs::create_dir_all(workspace.path().join(".fiber")).expect("fiber dir created");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    let report = begin.guard.finish();
    assert!(report.is_empty(), "no change means no report");
    assert!(
        report.notice_lines().is_empty(),
        "no change means no notice"
    );
    assert!(
        Report {
            restored: Vec::new(),
            failed: Vec::new(),
        }
        .details_value()
        .is_none(),
        "no change means no details"
    );
}

#[test]
fn recovery_restores_and_drops_a_stale_baseline() {
    let workspace = fakes::TempDir::new("fiber-protected-stale");
    let fiber = workspace.path().join(".fiber");
    std::fs::create_dir_all(&fiber).expect("fiber dir created");
    std::fs::write(fiber.join("keep.txt"), b"original").expect("file written");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    // A call that dies before its end: the guard drops without finishing,
    // and the operating system releases the active lock with it.
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    std::fs::write(fiber.join("keep.txt"), b"changed").expect("file changed");
    drop(begin.guard);
    let baseline = baseline_file(&home);
    assert!(baseline.is_file(), "a dead call leaves its baseline");
    let (_, startup) = ProtectedPaths::open(&home, workspace.path()).expect("open recovers");
    let report = startup.expect("a stale baseline reports");
    assert_eq!(report.restored, vec![".fiber".to_owned()]);
    assert_eq!(
        std::fs::read(fiber.join("keep.txt")).expect("file reads"),
        b"original",
        "startup recovery restores the stale change"
    );
    assert!(!baseline.exists(), "startup recovery drops the baseline");
    let (_, quiet) = ProtectedPaths::open(&home, workspace.path()).expect("open recovers");
    assert!(quiet.is_none(), "a dropped baseline recovers nothing");
}

#[test]
fn recovery_leaves_a_live_baseline_untouched() {
    let workspace = fakes::TempDir::new("fiber-protected-live");
    let fiber = workspace.path().join(".fiber");
    std::fs::create_dir_all(&fiber).expect("fiber dir created");
    std::fs::write(fiber.join("keep.txt"), b"original").expect("file written");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    std::fs::write(fiber.join("keep.txt"), b"changed").expect("file changed");
    let (_, startup) = ProtectedPaths::open(&home, workspace.path()).expect("open recovers");
    assert!(startup.is_none(), "a live baseline is left for its holders");
    assert_eq!(
        std::fs::read(fiber.join("keep.txt")).expect("file reads"),
        b"changed",
        "a live baseline restores nothing at startup"
    );
    let report = begin.guard.finish();
    assert_eq!(report.restored, vec![".fiber".to_owned()]);
    assert_eq!(
        std::fs::read(fiber.join("keep.txt")).expect("file reads"),
        b"original",
        "the holder's end restores"
    );
}

#[test]
fn a_stale_baseline_is_recovered_before_a_new_snapshot() {
    let workspace = fakes::TempDir::new("fiber-protected-resnapshot");
    let fiber = workspace.path().join(".fiber");
    std::fs::create_dir_all(&fiber).expect("fiber dir created");
    std::fs::write(fiber.join("keep.txt"), b"original").expect("file written");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    let first = handle.begin(workspace.path()).expect("begin snapshots");
    std::fs::write(fiber.join("keep.txt"), b"changed").expect("file changed");
    drop(first.guard);
    let second = handle.begin(workspace.path()).expect("begin recovers");
    assert_eq!(
        second.guard.recovered.restored,
        vec![".fiber".to_owned()],
        "the stale change is recovered and reported"
    );
    assert_eq!(
        std::fs::read(fiber.join("keep.txt")).expect("file reads"),
        b"original",
        "the new snapshot records the restored value"
    );
    let report = second.guard.finish();
    // The end carries the stale recovery with the call's own changes,
    // each path named once: nothing further changed past the recovery.
    assert_eq!(
        report.restored,
        vec![".fiber".to_owned()],
        "the end names the recovered path once"
    );
}

#[test]
fn a_workspace_no_project_key_can_name_fails_before_spawn() {
    let dir = fakes::TempDir::new("fiber-protected-bad-key");
    let home = protected_home(&dir);
    let handle = open_handle(&home, dir.path());
    // A NUL byte survives the slug but no project key can name it.
    let bad = dir.path().join("no\0key");
    let error = handle.begin(&bad).expect_err("no key names the workspace");
    assert!(
        error.to_string().contains("is not a project key"),
        "the failure names the key: {error}"
    );
    assert!(
        !home.join("projects").exists(),
        "the failed begin records nothing"
    );
}

/// No baseline file exists under `home`.
fn no_baseline_yet(home: &std::path::Path) {
    let projects = home.join("projects");
    if let Ok(entries) = std::fs::read_dir(&projects) {
        for project in entries.flatten() {
            assert!(
                !project
                    .path()
                    .join("protected-paths")
                    .join("baseline.json")
                    .is_file(),
                "no baseline is recorded yet"
            );
        }
    }
}

#[test]
fn an_approved_write_advances_the_baseline() {
    let workspace = fakes::TempDir::new("fiber-protected-approved");
    let fiber = workspace.path().join(".fiber");
    std::fs::create_dir_all(&fiber).expect("fiber dir created");
    let protected = fiber.join("keep.txt");
    std::fs::write(&protected, b"original").expect("file written");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    // An unrelated path is a no-op, even before any baseline exists.
    handle.note_approved_write(workspace.path(), &workspace.path().join("unrelated.txt"));
    no_baseline_yet(&home);
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    // Fiber's own write lands, then advances the entry under the lock.
    std::fs::write(&protected, b"approved").expect("file written");
    handle.note_approved_write(workspace.path(), &protected);
    handle.note_approved_write(workspace.path(), &workspace.path().join("unrelated.txt"));
    let report = begin.guard.finish();
    assert!(
        report.is_empty(),
        "an approved write is never restored away"
    );
    assert_eq!(
        std::fs::read(&protected).expect("file reads"),
        b"approved",
        "the approved value stands"
    );
}

#[test]
fn a_shell_side_change_after_an_approved_write_still_restores() {
    let workspace = fakes::TempDir::new("fiber-protected-approved-shell");
    let fiber = workspace.path().join(".fiber");
    std::fs::create_dir_all(&fiber).expect("fiber dir created");
    let protected = fiber.join("keep.txt");
    std::fs::write(&protected, b"original").expect("file written");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    std::fs::write(&protected, b"approved").expect("file written");
    handle.note_approved_write(workspace.path(), &protected);
    // The shell then changes the same path: the approved value is what
    // the call's end compares against.
    std::fs::write(&protected, b"shell").expect("file written");
    let report = begin.guard.finish();
    assert_eq!(report.restored, vec![".fiber".to_owned()]);
    assert_eq!(
        std::fs::read(&protected).expect("file reads"),
        b"approved",
        "the shell change is restored to the approved value"
    );
}

#[test]
fn setup_fails_closed_when_the_state_cannot_be_recorded() {
    let workspace = fakes::TempDir::new("fiber-protected-unwritable");
    let file = workspace.path().join("file-home");
    std::fs::write(&file, b"x").expect("file written");
    let error = ProtectedPaths::open(&file, workspace.path()).expect_err("no state is recorded");
    assert!(!error.is_empty(), "the failure names its cause: {error}");
}

#[test]
fn a_corrupt_baseline_fails_the_call_closed() {
    let workspace = fakes::TempDir::new("fiber-protected-corrupt");
    std::fs::create_dir_all(workspace.path().join(".fiber")).expect("fiber dir created");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    let baseline = baseline_file(&home);
    drop(begin.guard);
    std::fs::write(&baseline, b"not json").expect("baseline corrupted");
    let error = handle
        .begin(workspace.path())
        .expect_err("a corrupt baseline stops the call");
    assert!(!error.to_string().is_empty(), "the failure names its cause");
}

#[test]
fn symlinks_renames_and_type_swaps_are_restored() {
    let workspace = fakes::TempDir::new("fiber-protected-links");
    let fiber = workspace.path().join(".fiber");
    std::fs::create_dir_all(fiber.join("dir")).expect("dir created");
    std::fs::write(fiber.join("dir").join("file.txt"), b"data").expect("file written");
    #[cfg(unix)]
    std::os::unix::fs::symlink("dir/file.txt", fiber.join("link")).expect("link created");
    let home = protected_home(&workspace);
    let handle = open_handle(&home, workspace.path());
    let begin = handle.begin(workspace.path()).expect("begin snapshots");
    // Rename the directory away, replace the link with a file, and add a
    // new symlink the baseline never recorded.
    std::fs::rename(fiber.join("dir"), fiber.join("moved")).expect("dir renamed");
    #[cfg(unix)]
    {
        std::fs::remove_file(fiber.join("link")).expect("link removed");
        std::fs::write(fiber.join("link"), b"now a file").expect("file written");
        std::os::unix::fs::symlink("moved", fiber.join("extra")).expect("link created");
    }
    let report = begin.guard.finish();
    assert!(
        fiber.join("dir").join("file.txt").is_file(),
        "the rename is undone"
    );
    assert!(!fiber.join("moved").exists(), "the renamed dir is gone");
    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::read_link(fiber.join("link")).expect("link reads"),
            std::path::Path::new("dir/file.txt"),
            "the link target is restored"
        );
        assert!(
            !fiber.join("extra").exists(),
            "an unrecorded link is removed"
        );
    }
    assert_eq!(report.restored, vec![".fiber".to_owned()]);
}

#[test]
fn overlapping_calls_share_one_baseline_and_restore_once() {
    let repo = fakes::git::repo("fiber-protected-overlap");
    let other = linked(repo.path(), "wt1");
    let fiber = repo.path().join(".fiber");
    std::fs::create_dir_all(&fiber).expect("fiber dir created");
    std::fs::write(fiber.join("note.txt"), b"original").expect("note written");
    let home = protected_home(&repo);
    let handle = open_handle(&home, repo.path());
    // Plain receivers are Send but not Sync, so each one waits
    // behind a mutex to cross the scoped-thread boundary.
    let (a_running_tx, a_running_rx) = mpsc::channel::<()>();
    let (b_started_tx, b_started_rx) = mpsc::channel::<()>();
    let (a_done_tx, a_done_rx) = mpsc::channel::<()>();
    let a_running_rx = std::sync::Mutex::new(a_running_rx);
    let b_started_rx = std::sync::Mutex::new(b_started_rx);
    let a_done_rx = std::sync::Mutex::new(a_done_rx);
    let wait = |what: &str, rx: &std::sync::Mutex<mpsc::Receiver<()>>| {
        let rx = rx.lock().expect("the rendezvous locks");
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap_or_else(|_| panic!("{what}"));
    };
    std::thread::scope(|scope| {
        // B starts while A holds the baseline: it reuses the fixed
        // snapshot and ends with nothing to restore.
        scope.spawn(|| {
            wait("A takes its baseline first", &a_running_rx);
            let begin = handle.begin(&other).expect("B reuses the live baseline");
            assert!(
                begin.guard.recovered.is_empty(),
                "a reused baseline recovers nothing"
            );
            let baseline = std::fs::read(baseline_file(&home)).expect("the baseline reads");
            b_started_tx.send(()).expect("B signals");
            wait("A ends first", &a_done_rx);
            assert_eq!(
                std::fs::read(baseline_file(&home)).expect("the baseline reads"),
                baseline,
                "the baseline never moves while calls hold it"
            );
            let report = begin.guard.finish();
            assert!(report.is_empty(), "B restores nothing back");
        });
        // A runs on this thread: it restores exactly once.
        let begin = handle.begin(repo.path()).expect("A takes the baseline");
        a_running_tx.send(()).expect("A signals");
        wait("B reuses the baseline", &b_started_rx);
        std::fs::write(fiber.join("note.txt"), b"changed").expect("note changed");
        let report = begin.guard.finish();
        assert_eq!(report.restored, vec![".fiber".to_owned()]);
        assert_eq!(
            std::fs::read(fiber.join("note.txt")).expect("note reads"),
            b"original",
            "A restores"
        );
        a_done_tx.send(()).expect("A signals");
    });
}

#[cfg(target_os = "linux")]
mod linux_signal {
    use super::super::protected::{
        ProtectedPaths, linux_policy, supervisor, supervisor_rules, supports_signal_scope,
    };
    use super::{open_handle, protected_home};

    /// The supervisor under test: distinct session and hub identities, so
    /// every rule names both.
    fn two_process() -> super::super::protected::Supervisor {
        super::super::protected::Supervisor {
            session_pid: 100,
            session_pgid: 300,
            hub_pid: 200,
            hub_pgid: 400,
        }
    }

    fn syscalls(rules: &[(i64, Vec<seccompiler::SeccompRule>)]) -> Vec<i64> {
        let mut syscalls: Vec<i64> = rules.iter().map(|(syscall, _)| *syscall).collect();
        syscalls.sort_unstable();
        syscalls
    }

    #[test]
    fn the_narrow_rule_names_only_the_ruled_targets() {
        let rules = supervisor_rules(&two_process()).expect("rules build");
        assert_eq!(
            syscalls(&rules),
            {
                let mut syscalls: Vec<i64> = vec![
                    libc::SYS_kill,
                    libc::SYS_pidfd_open,
                    libc::SYS_tgkill,
                    libc::SYS_tkill,
                ];
                syscalls.sort_unstable();
                syscalls
            },
            "only the ruled syscalls are filtered"
        );
    }

    #[test]
    fn kill_covers_processes_groups_and_everyone_but_never_zero() {
        let rules = supervisor_rules(&two_process()).expect("rules build");
        let kill = rules
            .iter()
            .find(|(syscall, _)| *syscall == libc::SYS_kill)
            .expect("kill is filtered");
        // Five distinct targets: the two processes, the two groups as
        // negative pids, and -1 for every process.
        assert_eq!(kill.1.len(), 5, "kill names five targets, got {kill:?}");
        let debug = format!("{kill:?}");
        for pid in [100u64, 200] {
            assert!(
                debug.contains(&format!("value: {pid}")),
                "kill names pid {pid}"
            );
        }
        for pgid in [300u64, 400] {
            let neg = 0u64.wrapping_sub(pgid);
            assert!(
                debug.contains(&format!("value: {neg}")),
                "kill names group -{pgid}"
            );
            assert!(
                !debug.contains(&format!("value: {pgid}")),
                "a group is never a bare pid"
            );
        }
        assert!(
            debug.contains(&format!("value: {}", u64::MAX)),
            "kill names -1"
        );
        assert!(
            !debug.contains("value: 0"),
            "pid 0 stays open for the call's own group"
        );
    }

    #[test]
    fn neighbors_of_the_targets_are_not_matched() {
        let rules = supervisor_rules(&two_process()).expect("rules build");
        let debug = format!("{rules:?}");
        for pid in [99u64, 101, 199, 201, 299, 301, 399, 401] {
            assert!(
                !debug.contains(&format!("value: {pid}")),
                "pid {pid} is not a target"
            );
        }
    }

    #[test]
    fn tgkill_covers_both_arguments_and_pidfd_open_covers_only_pids() {
        let rules = supervisor_rules(&two_process()).expect("rules build");
        let tgkill = rules
            .iter()
            .find(|(syscall, _)| *syscall == libc::SYS_tgkill)
            .expect("tgkill is filtered");
        assert_eq!(tgkill.1.len(), 4, "tgkill names both args, got {tgkill:?}");
        let pidfd = rules
            .iter()
            .find(|(syscall, _)| *syscall == libc::SYS_pidfd_open)
            .expect("pidfd_open is filtered");
        assert_eq!(pidfd.1.len(), 2, "pidfd_open names two pids, got {pidfd:?}");
        let tkill = rules
            .iter()
            .find(|(syscall, _)| *syscall == libc::SYS_tkill)
            .expect("tkill is filtered");
        assert_eq!(tkill.1.len(), 2, "tkill names two tids, got {tkill:?}");
    }

    #[test]
    fn one_process_collapses_the_hub_slot() {
        let single = super::super::protected::Supervisor {
            session_pid: 100,
            session_pgid: 300,
            hub_pid: 100,
            hub_pgid: 300,
        };
        let rules = supervisor_rules(&single).expect("rules build");
        let kill = rules
            .iter()
            .find(|(syscall, _)| *syscall == libc::SYS_kill)
            .expect("kill is filtered");
        assert_eq!(
            kill.1.len(),
            3,
            "one process names three targets, got {kill:?}"
        );
    }

    /// The project's `protected-paths` directory: the one project under `home`.
    fn state_dir(home: &std::path::Path) -> std::path::PathBuf {
        let mut found = Vec::new();
        for project in std::fs::read_dir(home.join("projects")).expect("projects lists") {
            let project = project.expect("project entry reads");
            found.push(project.path().join("protected-paths"));
        }
        assert_eq!(found.len(), 1, "one project holds one state dir");
        found.pop().expect("one state dir")
    }

    /// A foreign process holds the active lock at `lock`, then dies without
    /// releasing it, proving the operating system releases the lock with
    /// the dead process.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_dead_holders_lock_releases_and_startup_recovers() {
        use std::os::unix::process::CommandExt as _;
        let workspace = fakes::TempDir::new("fiber-protected-crash");
        let fiber = workspace.path().join(".fiber");
        std::fs::create_dir_all(&fiber).expect("fiber dir created");
        std::fs::write(fiber.join("keep.txt"), b"original").expect("file written");
        let home = protected_home(&workspace);
        let handle = open_handle(&home, workspace.path());
        // A call takes its baseline, damages a protected path, and dies
        // before its end: dropping the guard releases the descriptor, as
        // the operating system releases a dead process's descriptors.
        let begin = handle.begin(workspace.path()).expect("begin snapshots");
        std::fs::write(fiber.join("keep.txt"), b"changed").expect("file changed");
        let active = state_dir(&home).join("active.lock");
        drop(begin.guard);
        let mut child = std::process::Command::new("flock");
        child
            .arg("-s")
            .arg(&active)
            .arg("sleep")
            .arg("30")
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = child
            .spawn()
            .expect("flock holds the active lock in a dead-end process");
        let pid = child.id();
        // The holder really holds it, bounded and without sleeping: the
        // probe fails exactly while the foreign descriptor locks.
        let held = active.clone();
        fakes::within(
            "the holder takes the lock",
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                loop {
                    let file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&held)
                        .expect("the lock opens");
                    match file.try_lock() {
                        Err(std::fs::TryLockError::WouldBlock) => return,
                        Ok(()) => file.unlock().expect("the probe releases"),
                        Err(std::fs::TryLockError::Error(source)) => {
                            panic!("the lock probes: {source}")
                        }
                    }
                }
            },
        );
        // SIGKILL to the holder's group: the spawned child leads its own
        // group, and flock forks the sleeper holding the inherited lock
        // descriptor, so killing the parent alone leaves the lock held. The
        // wait reaps the direct child on every path past this point.
        support::group::signal(pid, rustix::process::Signal::KILL).expect("the holder dies");
        child.wait().expect("the holder reaps");
        // Nobody holds the active lock anymore: the kernel frees the last
        // descriptor at death, and the bounded probe covers delivery.
        let released = active.clone();
        fakes::within(
            "the dead holder released the lock",
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || loop {
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&released)
                    .expect("the lock opens");
                match file.try_lock() {
                    Ok(()) => {
                        file.unlock().expect("the probe releases");
                        return;
                    }
                    Err(std::fs::TryLockError::WouldBlock) => {}
                    Err(std::fs::TryLockError::Error(source)) => {
                        panic!("the lock probes: {source}")
                    }
                }
            },
        );
        // The next session start restores the abandoned damage and drops
        // the stale baseline.
        let (_, startup) = ProtectedPaths::open(&home, workspace.path()).expect("open recovers");
        let report = startup.expect("a stale baseline reports");
        assert_eq!(report.restored, vec![".fiber".to_owned()]);
        assert_eq!(
            std::fs::read(fiber.join("keep.txt")).expect("file reads"),
            b"original",
            "startup recovery restores"
        );
        assert!(
            !state_dir(&home).join("baseline.json").exists(),
            "startup recovery drops the baseline"
        );
    }

    #[test]
    fn the_policy_matches_the_probe() {
        let policy = linux_policy().expect("policy selects");
        if supports_signal_scope() {
            assert!(policy.signal_scope, "ABI 6 and above uses signal scope");
            assert!(
                policy.supervisor_rules.is_empty(),
                "scope adds no seccomp rule"
            );
        } else {
            assert!(!policy.signal_scope, "below ABI 6 uses the seccomp rule");
            assert_eq!(syscalls(&policy.supervisor_rules).len(), 4);
        }
    }

    #[test]
    fn collection_names_this_process() {
        let collected = supervisor();
        assert_eq!(collected.session_pid, std::process::id());
        assert!(collected.session_pgid != 0, "the session group is real");
        assert!(collected.hub_pid != 0, "the hub slot is real");
    }

    #[test]
    fn the_compiled_filter_keeps_socket_denials_beside_supervisor_rules() {
        let rules = supervisor_rules(&two_process()).expect("rules build");
        let program = super::super::compile_seccomp(&rules).expect("filter compiles");
        // Instruction classes from the kernel's filter header: the low
        // three bits name the class, so a jump comparing a number keeps
        // working whatever addressing the compiler chose.
        const JUMP: u16 = 0x05;
        const RETURN: u16 = 0x06;
        // Every denied call is compared by number: a jump on the syscall
        // number reaches the refusal below, so a dropped entry with no
        // comparison to make cannot deny.
        for denied in [
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_io_uring_setup,
        ] {
            let want = u32::try_from(denied).expect("denied syscall number fits in u32");
            assert!(
                program
                    .iter()
                    .any(|instruction| instruction.code & 0x07 == JUMP && instruction.k == want),
                "the filter compares {denied}"
            );
        }
        // The refusal the comparisons reach: EPERM, the errno the sandbox
        // promises for denied calls.
        const EPERM_RETURN: u32 = 0x0005_0000 | libc::EPERM as u32;
        assert!(
            program
                .iter()
                .any(|instruction| instruction.code & 0x07 == RETURN
                    && instruction.k == EPERM_RETURN),
            "the filter returns EPERM"
        );
    }
}
