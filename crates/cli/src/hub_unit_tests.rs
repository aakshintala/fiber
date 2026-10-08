//! The service's name, its unit file's path, and the unit text for both
//! managers, escaping included.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use contract::ErrorCode;

use super::{Manager, Service, name, unit_path};

#[test]
fn the_name_encodes_the_home_path() {
    let rows = [
        ("/Users/a/.fiber", "fiber-hub-Users-a-.fiber"),
        ("/tmp/a/b", "fiber-hub-tmp-a-b"),
        ("/tmp/a-b", "fiber-hub-tmp-a_2db"),
        ("/tmp/a_b", "fiber-hub-tmp-a_5fb"),
        ("/tmp/x y", "fiber-hub-tmp-x_20y"),
        ("/tmp/é", "fiber-hub-tmp-_c3_a9"),
        ("/tmp/V2", "fiber-hub-tmp-V2"),
    ];
    for (home, expected) in rows {
        assert_eq!(name(Path::new(home)).unwrap(), expected, "{home}");
    }
}

#[test]
fn a_name_over_240_bytes_is_refused_naming_fiber_home() {
    // `fiber-hub-` is 10 bytes, so 230 kept bytes make exactly 240.
    let longest = format!("/{}", "a".repeat(230));
    assert_eq!(name(Path::new(&longest)).unwrap().len(), 240);
    let over = format!("/{}", "a".repeat(231));
    let error = name(Path::new(&over)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.message.contains("FIBER_HOME"), "{}", error.message);
}

#[test]
fn homes_that_differ_only_in_separators_and_escapes_never_share_a_name() {
    let alphabet = ["/", "-", "_", "2", "d"];
    let mut homes = Vec::new();
    for a in alphabet {
        homes.push(format!("/t{a}"));
        for b in alphabet {
            homes.push(format!("/t{a}{b}"));
            for c in alphabet {
                homes.push(format!("/t{a}{b}{c}"));
            }
        }
    }
    assert!(homes.len() >= 50);
    let names: BTreeSet<String> = homes
        .iter()
        .map(|home| name(Path::new(home)).unwrap())
        .collect();
    assert_eq!(names.len(), homes.len());
}

fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |key| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| OsString::from(v))
    }
}

#[test]
fn launchd_units_live_in_the_users_launch_agents() {
    let path = unit_path(
        Manager::Launchd { uid: 501 },
        "fiber-hub-x",
        &vars(&[("HOME", "/Users/a"), ("XDG_CONFIG_HOME", "/elsewhere")]),
    )
    .unwrap();
    assert_eq!(
        path,
        PathBuf::from("/Users/a/Library/LaunchAgents/fiber-hub-x.plist")
    );
}

#[test]
fn systemd_units_live_under_an_absolute_xdg_config_home() {
    let path = unit_path(
        Manager::Systemd,
        "fiber-hub-x",
        &vars(&[("HOME", "/home/a"), ("XDG_CONFIG_HOME", "/cfg")]),
    )
    .unwrap();
    assert_eq!(path, PathBuf::from("/cfg/systemd/user/fiber-hub-x.service"));
}

#[test]
fn a_relative_or_unset_xdg_config_home_falls_back_to_home() {
    for set in [
        vars(&[("HOME", "/home/a"), ("XDG_CONFIG_HOME", "cfg")]),
        vars(&[("HOME", "/home/a")]),
    ] {
        let path = unit_path(Manager::Systemd, "fiber-hub-x", &set).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/home/a/.config/systemd/user/fiber-hub-x.service")
        );
    }
}

#[test]
fn an_unset_or_relative_home_is_usage_naming_home() {
    for manager in [Manager::Launchd { uid: 501 }, Manager::Systemd] {
        for set in [vars(&[]), vars(&[("HOME", "home/a")])] {
            let error = unit_path(manager, "fiber-hub-x", &set).unwrap_err();
            assert_eq!(error.code, ErrorCode::Usage);
            assert!(error.message.contains("HOME"), "{}", error.message);
        }
    }
}

#[test]
fn locate_names_the_service_and_its_unit() {
    let service = Service::locate(
        Manager::Systemd,
        Path::new("/home/a/.fiber"),
        Path::new("/bin/fiber"),
        &vars(&[("HOME", "/home/a")]),
    )
    .unwrap();
    assert_eq!(service.name, "fiber-hub-home-a-.fiber");
    assert_eq!(
        service.unit,
        PathBuf::from("/home/a/.config/systemd/user/fiber-hub-home-a-.fiber.service")
    );
    assert_eq!(service.exe, PathBuf::from("/bin/fiber"));
    assert_eq!(service.home, PathBuf::from("/home/a/.fiber"));
}

fn service(manager: Manager, exe: &Path, home: &Path) -> Service {
    Service {
        manager,
        name: name(home).unwrap(),
        unit: PathBuf::from("/unused"),
        exe: exe.to_path_buf(),
        home: home.to_path_buf(),
    }
}

const SYSTEMD_4040: &str = r#"# hub.port 4040
[Unit]
Description=Fiber hub

[Service]
ExecStart="/home/a/.local/bin/fiber" "hub" "serve" "--installed"
Environment="FIBER_HOME=/home/a/.fiber"
KillMode=process
Restart=on-failure
RestartSec=10
SuccessExitStatus=129 130 143

[Install]
WantedBy=default.target
"#;

const PLIST_NONE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- hub.port none -->
<plist version="1.0">
<dict>
  <key>Label</key><string>fiber-hub-Users-a-.fiber</string>
  <key>ProgramArguments</key><array><string>/Users/a/.local/bin/fiber</string><string>hub</string><string>serve</string><string>--installed</string></array>
  <key>EnvironmentVariables</key><dict><key>FIBER_HOME</key><string>/Users/a/.fiber</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>AbandonProcessGroup</key><true/>
</dict>
</plist>
"#;

#[test]
fn the_systemd_unit_runs_the_installed_hub() {
    let unit = service(
        Manager::Systemd,
        Path::new("/home/a/.local/bin/fiber"),
        Path::new("/home/a/.fiber"),
    );
    assert_eq!(unit.render(Some(4040)).unwrap(), SYSTEMD_4040);
    assert_eq!(
        unit.render(None).unwrap(),
        SYSTEMD_4040.replace("# hub.port 4040", "# hub.port none")
    );
}

#[test]
fn the_launchd_plist_runs_the_installed_hub() {
    let plist = service(
        Manager::Launchd { uid: 501 },
        Path::new("/Users/a/.local/bin/fiber"),
        Path::new("/Users/a/.fiber"),
    );
    assert_eq!(plist.render(None).unwrap(), PLIST_NONE);
    assert_eq!(
        plist.render(Some(4040)).unwrap(),
        PLIST_NONE.replace("<!-- hub.port none -->", "<!-- hub.port 4040 -->")
    );
}

#[test]
fn systemd_escapes_each_character_class_it_would_reinterpret() {
    let rows = [
        (
            "/b\\in/f",
            r#""/b\\in/f""#,
            r#""FIBER_HOME=/h\\me""#,
            "/h\\me",
        ),
        (
            "/b\"in/f",
            r#""/b\"in/f""#,
            r#""FIBER_HOME=/h\"me""#,
            "/h\"me",
        ),
        (
            "/b%in/f",
            r#""/b%%in/f""#,
            r#""FIBER_HOME=/h%%me""#,
            "/h%me",
        ),
        ("/b$in/f", r#""/b$$in/f""#, r#""FIBER_HOME=/h$me""#, "/h$me"),
        ("/b in/f", r#""/b in/f""#, r#""FIBER_HOME=/h me""#, "/h me"),
    ];
    for (exe, exec, environment, home) in rows {
        let unit = service(Manager::Systemd, Path::new(exe), Path::new(home))
            .render(None)
            .unwrap();
        assert!(
            unit.contains(&format!(
                "ExecStart={exec} \"hub\" \"serve\" \"--installed\"\n"
            )),
            "{unit}"
        );
        assert!(
            unit.contains(&format!("Environment={environment}\n")),
            "{unit}"
        );
    }
}

#[test]
fn launchd_escapes_markup_in_strings() {
    let rows = [("&", "&amp;"), ("<", "&lt;"), (">", "&gt;")];
    for (raw, escaped) in rows {
        let plist = service(
            Manager::Launchd { uid: 501 },
            Path::new(&format!("/b{raw}/f")),
            Path::new(&format!("/h{raw}")),
        )
        .render(None)
        .unwrap();
        assert!(
            plist.contains(&format!("<array><string>/b{escaped}/f</string>")),
            "{plist}"
        );
        assert!(
            plist.contains(&format!(
                "<key>FIBER_HOME</key><string>/h{escaped}</string>"
            )),
            "{plist}"
        );
    }
}

#[test]
fn a_control_character_or_non_utf8_path_is_refused_naming_it() {
    let bad = [
        PathBuf::from("/b\nin/f"),
        PathBuf::from("/b\tin/f"),
        PathBuf::from(OsStr::from_bytes(b"/b\xffin/f")),
    ];
    for manager in [Manager::Launchd { uid: 501 }, Manager::Systemd] {
        for path in &bad {
            for (exe, home) in [(path.as_path(), Path::new("/h")), (Path::new("/f"), path)] {
                let error = service(manager, exe, home).render(None).unwrap_err();
                assert_eq!(error.code, ErrorCode::IoFailed);
                assert!(
                    error.message.contains(&format!("{path:?}")),
                    "{}",
                    error.message
                );
            }
        }
    }
}
