//! Tests of `scripts/install.sh` (`docs/releasing.md`, "Installing"), run
//! under `/bin/dash` against a fixture release served as file URLs. The
//! checked-in stubs under `install_fixture/` stand in for `uname`, `sysctl`
//! and the released binary, so no test writes an executable; the one file a
//! run executes after writing it is the stub `install.sh` unpacks, which is
//! the subject under test. Every run carries the test's one deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::Watchdog;
use fakes::ustar::{archive, gzip, header, sha256};
use support::{Deadline, Output, Setup};

/// Every target a release publishes an archive for.
const TARGETS: [&str; 3] = [
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/install_fixture")
}

/// The checked-in stub binary each fixture archive holds.
fn stub() -> Vec<u8> {
    fs::read(fixture().join("fiber")).unwrap()
}

/// The operating system and CPU the stubs report.
struct Host {
    os: &'static str,
    cpu: &'static str,
    arm64: &'static str,
}

const LINUX_X86_64: Host = Host {
    os: "Linux",
    cpu: "x86_64",
    arm64: "0",
};

impl Setup {
    /// A root whose stub binary reports `version`.
    fn new_with_stub_version(version: &str) -> Self {
        let deadline = Deadline::start();
        let setup = Self {
            deadline,
            root: fakes::TempDir::new("fis"),
        };
        for dir in ["stub", "home", "tmp", "release"] {
            fs::create_dir(setup.path(dir)).unwrap();
        }
        fs::write(setup.path("stub/version"), version).unwrap();
        setup
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn dest(&self) -> PathBuf {
        self.path("dest")
    }

    fn base(&self) -> String {
        format!("file://{}", self.path("release").display())
    }

    /// Writes each target's archive, holding the stub as `fiber`, and its
    /// `.sha256` file as `sha256sum` writes it, under `release/<dir>/`.
    fn publish(&self, dir: &str, targets: &[&str]) {
        let at = self.path("release").join(dir);
        fs::create_dir_all(&at).unwrap();
        let stub = stub();
        let bytes = gzip(&archive(&[(
            header("fiber", b'0', stub.len() as u64, 0o755, ""),
            &stub,
        )]));
        for target in targets {
            let name = format!("fiber-{target}.tar.gz");
            fs::write(at.join(&name), &bytes).unwrap();
            fs::write(
                at.join(format!("{name}.sha256")),
                format!("{}  {name}\n", sha256(&bytes)),
            )
            .unwrap();
        }
    }

    /// Writes each target's archive, holding the stub as `fiber`, and its
    /// `.sha256` file as `sha256sum` writes it, under `release/download/<tag>/`.
    fn publish_nightly(&self, tag: &str, targets: &[&str]) {
        self.publish(&format!("download/{tag}"), targets);
    }

    /// Writes `release/releases.json` in the GitHub release-list shape:
    /// one pretty-printed object per entry with `tag_name`, `prerelease`
    /// and `draft`. Field order alternates per entry, so the test proves
    /// the channel pick reads fields, not positions.
    fn write_list(&self, entries: &[(&str, bool, bool)]) {
        let mut json = String::from("[\n");
        for (i, (tag, prerelease, draft)) in entries.iter().enumerate() {
            if i > 0 {
                json.push_str(",\n");
            }
            if i % 2 == 0 {
                json.push_str(&format!(
                    "  {{\n    \"tag_name\": \"{tag}\",\n    \"prerelease\": {prerelease},\n    \"draft\": {draft}\n  }}"
                ));
            } else {
                json.push_str(&format!(
                    "  {{\n    \"draft\": {draft},\n    \"tag_name\": \"{tag}\",\n    \"prerelease\": {prerelease}\n  }}"
                ));
            }
        }
        json.push_str("\n]\n");
        fs::write(self.path("release/releases.json"), json).unwrap();
    }

    /// What the stub's `release-install` recorded, or `None` if it never
    /// ran.
    fn calls(&self) -> Option<String> {
        fs::read_to_string(self.path("stub/calls")).ok()
    }

    /// Runs `/bin/dash scripts/install.sh --base-url <release>` on `host`,
    /// with `FIBER_INSTALL_DIR` set to `dest` and then each of `env`, and
    /// waits for it under the test's [`Deadline`].
    #[track_caller]
    fn install(&self, host: &Host, env: &[(&str, &str)]) -> Output {
        self.install_args(host, &[], env)
    }

    /// As [`Setup::install`], with `args` passed after `--base-url`.
    #[track_caller]
    fn install_args(&self, host: &Host, args: &[&str], env: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh");
        let path = format!("{}:/usr/bin:/bin", fixture().join("bin").display());
        let mut command = Command::new("/bin/dash");
        command
            .arg(script)
            .args(["--base-url", &self.base()])
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", path)
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("tmp"))
            .env("FIBER_HOME", self.path("stub"))
            .env("FIBER_INSTALL_DIR", self.dest())
            .env("FIBER_TEST_UNAME_S", host.os)
            .env("FIBER_TEST_UNAME_M", host.cpu)
            .env("FIBER_TEST_ARM64", host.arm64)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.process_group(0).spawn().unwrap();
        let group = child.id();
        let watchdog = Watchdog::group(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, "install.sh to exit"),
        };
        watchdog.stand_down(self.deadline.cleanup());
        assert_eq!(
            fs::read_dir(self.path("tmp")).unwrap().count(),
            0,
            "install.sh left its temporary directory"
        );
        Output::from(output)
    }

    /// Asserts `dest/fiber` is the stub with mode 755 and no staging
    /// directory is left beside it.
    fn assert_installed(&self, dest: &Path) {
        let installed = dest.join("fiber");
        assert_eq!(fs::read(&installed).unwrap(), stub());
        assert_eq!(
            fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let left: Vec<_> = fs::read_dir(dest)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != "fiber")
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

#[test]
fn a_good_install_puts_the_binary_in_place_and_runs_its_step() {
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let run = setup.install(&LINUX_X86_64, &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    setup.assert_installed(&setup.dest());
    assert_eq!(
        setup.calls().unwrap(),
        format!("release-install 0.0.9 --base-url {}\n", setup.base())
    );
    assert!(
        run.stderr.ends_with(&format!(
            "fiber: installed {}/fiber 0.0.9.\n",
            setup.dest().display()
        )),
        "{}",
        run.stderr
    );
}

#[test]
fn each_released_pair_picks_its_archive() {
    let pairs = [
        (
            Host {
                os: "Darwin",
                cpu: "arm64",
                arm64: "0",
            },
            "aarch64-apple-darwin",
        ),
        (
            Host {
                os: "Darwin",
                cpu: "x86_64",
                arm64: "1",
            },
            "aarch64-apple-darwin",
        ),
        (
            Host {
                os: "Linux",
                cpu: "aarch64",
                arm64: "0",
            },
            "aarch64-unknown-linux-musl",
        ),
        (
            Host {
                os: "Linux",
                cpu: "amd64",
                arm64: "0",
            },
            "x86_64-unknown-linux-musl",
        ),
    ];
    for (host, target) in pairs {
        // Only the expected archive is published, so a success proves the
        // pick.
        let setup = Setup::new_with_stub_version("0.0.9");
        setup.publish("latest/download", &[target]);
        let run = setup.install(&host, &[]);
        assert_eq!(
            run.code,
            Some(0),
            "{} {}: {}",
            host.os,
            host.cpu,
            run.stderr
        );
        setup.assert_installed(&setup.dest());
    }
}

#[test]
fn a_checksum_mismatch_leaves_the_destination_untouched() {
    for existing in [true, false] {
        let setup = Setup::new_with_stub_version("0.0.9");
        setup.publish("latest/download", &TARGETS);
        let sum =
            setup.path("release/latest/download/fiber-x86_64-unknown-linux-musl.tar.gz.sha256");
        fs::write(&sum, format!("{}\n", sha256(b"other"))).unwrap();
        if existing {
            fs::create_dir(setup.dest()).unwrap();
            fs::write(setup.dest().join("fiber"), "old").unwrap();
        }
        let run = setup.install(&LINUX_X86_64, &[]);
        assert_eq!(run.code, Some(1), "{}", run.stderr);
        assert_eq!(
            run.stderr,
            "fiber: fiber-x86_64-unknown-linux-musl.tar.gz does not match its .sha256 file.\n"
        );
        assert_eq!(setup.calls(), None);
        if existing {
            assert_eq!(
                fs::read_to_string(setup.dest().join("fiber")).unwrap(),
                "old"
            );
            assert_eq!(fs::read_dir(setup.dest()).unwrap().count(), 1);
        } else {
            assert!(!setup.dest().exists());
        }
    }
}

#[test]
fn a_checksum_file_without_a_digest_is_refused() {
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let sum = setup.path("release/latest/download/fiber-x86_64-unknown-linux-musl.tar.gz.sha256");
    for text in ["", "abc\n", &"g".repeat(64), &"a".repeat(65)] {
        fs::write(&sum, text).unwrap();
        let run = setup.install(&LINUX_X86_64, &[]);
        assert_eq!(run.code, Some(1), "{text:?}: {}", run.stderr);
        assert_eq!(
            run.stderr, "fiber: fiber-x86_64-unknown-linux-musl.tar.gz.sha256 holds no SHA-256.\n",
            "{text:?}"
        );
        assert!(!setup.dest().exists());
    }
}

#[test]
fn a_checksum_in_capitals_with_a_crlf_is_accepted() {
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let name = "fiber-x86_64-unknown-linux-musl.tar.gz";
    let at = setup.path("release/latest/download");
    let digest = sha256(&fs::read(at.join(name)).unwrap()).to_uppercase();
    fs::write(at.join(format!("{name}.sha256")), format!("  {digest}\r\n")).unwrap();
    let run = setup.install(&LINUX_X86_64, &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    setup.assert_installed(&setup.dest());
}

#[test]
fn an_unreleased_pair_stops_before_any_download() {
    for (host, message) in [
        (
            Host {
                os: "Darwin",
                cpu: "x86_64",
                arm64: "0",
            },
            "fiber: Darwin x86_64 has no release.\n",
        ),
        (
            Host {
                os: "FreeBSD",
                cpu: "amd64",
                arm64: "0",
            },
            "fiber: FreeBSD amd64 has no release.\n",
        ),
        (
            Host {
                os: "Linux",
                cpu: "riscv64",
                arm64: "0",
            },
            "fiber: Linux riscv64 has no release.\n",
        ),
    ] {
        let setup = Setup::new_with_stub_version("0.0.9");
        setup.publish("latest/download", &TARGETS);
        let run = setup.install(&host, &[]);
        assert_eq!(run.code, Some(1), "{}", run.stderr);
        assert_eq!(run.stderr, message);
        assert!(!setup.dest().exists());
        assert_eq!(setup.calls(), None);
    }
}

#[test]
fn an_install_directory_not_on_path_is_warned_about_without_failing() {
    let warning = |setup: &Setup| {
        format!(
            "fiber: {} is not on PATH. Add it to use fiber.\n",
            setup.dest().display()
        )
    };
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let run = setup.install(&LINUX_X86_64, &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(run.stderr.contains(&warning(&setup)), "{}", run.stderr);

    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let path = format!(
        "{}:{}:/usr/bin:/bin",
        fixture().join("bin").display(),
        setup.dest().display()
    );
    let run = setup.install(&LINUX_X86_64, &[("PATH", &path)]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(!run.stderr.contains("is not on PATH"), "{}", run.stderr);
}

#[test]
fn fiber_version_installs_that_release() {
    for asked in ["0.0.8", "v0.0.8"] {
        let setup = Setup::new_with_stub_version("0.0.9");
        setup.publish("download/v0.0.8", &TARGETS);
        let run = setup.install(&LINUX_X86_64, &[("FIBER_VERSION", asked)]);
        assert_eq!(run.code, Some(0), "{asked}: {}", run.stderr);
        setup.assert_installed(&setup.dest());
        assert_eq!(
            setup.calls().unwrap(),
            format!("release-install 0.0.8 --base-url {}\n", setup.base())
        );
    }
}

#[test]
fn a_fiber_version_that_is_not_a_version_is_refused() {
    for asked in [
        "latest",
        "0.8",
        "0.0.8.1",
        "v",
        "0..8",
        "0.0.8-rc1",
        "vv0.0.8",
    ] {
        let setup = Setup::new_with_stub_version("0.0.9");
        setup.publish("latest/download", &TARGETS);
        let run = setup.install(&LINUX_X86_64, &[("FIBER_VERSION", asked)]);
        assert_eq!(run.code, Some(2), "{asked}: {}", run.stderr);
        assert_eq!(
            run.stderr,
            format!("fiber: FIBER_VERSION={asked} is not a version such as 0.3.0.\n")
        );
        assert!(!setup.dest().exists());
    }
}

#[test]
fn an_empty_install_directory_means_the_default() {
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let run = setup.install(&LINUX_X86_64, &[("FIBER_INSTALL_DIR", "")]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    setup.assert_installed(&setup.path("home/.local/bin"));
    assert!(!setup.dest().exists());
}

#[test]
fn a_failed_step_keeps_the_binary_and_says_to_run_again() {
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    fs::write(setup.path("stub/exit"), "1").unwrap();
    let run = setup.install(&LINUX_X86_64, &[]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    setup.assert_installed(&setup.dest());
    assert!(
        run.stderr.ends_with(&format!(
            "fiber: installed {}/fiber, but its docs and extensions were not installed. \
             Run install.sh again.\n",
            setup.dest().display()
        )),
        "{}",
        run.stderr
    );
}

/// The nightly channel installs the newest nightly from a mixed release
/// list (`docs/releasing.md`, "Installing"): `0.0.10-nightly.*` sorts
/// above `0.0.9-nightly.*` whatever the dates say, a draft pre-release is
/// skipped even when it is the highest version, and a duplicated entry
/// for one date still resolves to that date's tag.
#[test]
fn the_nightly_channel_installs_the_newest_nightly() {
    let setup = Setup::new_with_stub_version("0.0.10-nightly.20261018");
    for tag in [
        "v0.0.9-nightly.20261018",
        "v0.0.9-nightly.20261019",
        "v0.0.9-nightly.20261020",
        "v0.0.10-nightly.20261018",
    ] {
        setup.publish_nightly(tag, &TARGETS);
    }
    setup.write_list(&[
        ("v0.0.10-nightly.20261018", true, false),
        ("v0.0.9", false, false),
        ("v0.0.9-nightly.20261020", true, false),
        ("v0.0.99-nightly.20261231", true, true),
        ("v0.0.9-nightly.20261018", true, false),
        ("v0.0.9-nightly.20261019", true, false),
        ("v0.0.10-nightly.20261018", true, false),
    ]);
    let run = setup.install_args(&LINUX_X86_64, &["--channel", "nightly"], &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    setup.assert_installed(&setup.dest());
    assert_eq!(
        setup.calls().unwrap(),
        format!(
            "release-install 0.0.10-nightly.20261018 --base-url {}\n",
            setup.base()
        )
    );
    assert!(
        run.stderr.ends_with(&format!(
            "fiber: installed {}/fiber 0.0.10-nightly.20261018.\n",
            setup.dest().display()
        )),
        "{}",
        run.stderr
    );
}

/// The stable channel installs from `latest/download` untouched by
/// nightlies: the release list beside it changes nothing.
#[test]
fn the_stable_channel_ignores_nightlies() {
    for args in [&[][..], &["--channel", "stable"][..]] {
        let setup = Setup::new_with_stub_version("0.0.9");
        setup.publish("latest/download", &TARGETS);
        setup.publish_nightly("v0.0.10-nightly.20261018", &TARGETS);
        setup.write_list(&[
            ("v0.0.9", false, false),
            ("v0.0.10-nightly.20261018", true, false),
        ]);
        let run = setup.install_args(&LINUX_X86_64, args, &[]);
        assert_eq!(run.code, Some(0), "{args:?}: {}", run.stderr);
        setup.assert_installed(&setup.dest());
        assert_eq!(
            setup.calls().unwrap(),
            format!("release-install 0.0.9 --base-url {}\n", setup.base())
        );
    }
}

/// The nightly pick tracks JSON structure: nested `author` and `assets`
/// objects around a release's fields never reset its candidate, so the
/// entry whose `tag_name` sits above its flags still wins
/// (`docs/releasing.md`, "Installing").
#[test]
fn the_nightly_channel_survives_nested_release_objects() {
    let setup = Setup::new_with_stub_version("0.0.10-nightly.20261018");
    for tag in ["v0.0.9-nightly.20261018", "v0.0.10-nightly.20261018"] {
        setup.publish_nightly(tag, &TARGETS);
    }
    let entry = |tag: &str| {
        format!(
            concat!(
                "  {{\n",
                "    \"tag_name\": \"{tag}\",\n",
                "    \"author\": {{\n",
                "      \"login\": \"octocat\"\n",
                "    }},\n",
                "    \"prerelease\": true,\n",
                "    \"draft\": false,\n",
                "    \"assets\": [\n",
                "      {{\n",
                "        \"name\": \"fiber.tar.gz\",\n",
                "        \"uploader\": {{\n",
                "          \"login\": \"octocat\"\n",
                "        }}\n",
                "      }}\n",
                "    ]\n",
                "  }}"
            ),
            tag = tag
        )
    };
    let json = format!(
        "[\n{},\n{}\n]\n",
        entry("v0.0.9-nightly.20261018"),
        entry("v0.0.10-nightly.20261018")
    );
    fs::write(setup.path("release/releases.json"), json).unwrap();
    let run = setup.install_args(&LINUX_X86_64, &["--channel", "nightly"], &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    setup.assert_installed(&setup.dest());
    assert_eq!(
        setup.calls().unwrap(),
        format!(
            "release-install 0.0.10-nightly.20261018 --base-url {}\n",
            setup.base()
        )
    );
}

/// An empty list and a stable-only list fail naming the nightly channel:
/// there is no nightly to install.
#[test]
fn the_nightly_channel_without_a_match_fails() {
    for entries in [
        vec![],
        vec![("v0.0.9", false, false)],
        vec![("v0.0.99-nightly.20261231", true, true)],
    ] {
        let setup = Setup::new_with_stub_version("0.0.9");
        setup.publish("latest/download", &TARGETS);
        setup.write_list(&entries);
        let run = setup.install_args(&LINUX_X86_64, &["--channel", "nightly"], &[]);
        assert_eq!(run.code, Some(1), "{entries:?}: {}", run.stderr);
        assert_eq!(
            run.stderr, "fiber: no nightly release found.\n",
            "{entries:?}"
        );
        assert!(!setup.dest().exists());
    }
}

/// An unknown channel, by flag or environment, exits 2.
#[test]
fn an_unknown_channel_is_refused() {
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let run = setup.install_args(&LINUX_X86_64, &["--channel", "bogus"], &[]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "fiber: --channel bogus is not stable or nightly.\n"
    );
    assert!(!setup.dest().exists());

    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let run = setup.install(&LINUX_X86_64, &[("FIBER_CHANNEL", "bogus")]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "fiber: --channel bogus is not stable or nightly.\n"
    );
    assert!(!setup.dest().exists());
}

/// The channel precedence (`docs/releasing.md`, "Installing"): the flag
/// beats the environment either way, an empty environment means stable,
/// and an explicit `FIBER_VERSION` beats the nightly channel.
#[test]
fn the_channel_precedence() {
    // The environment alone picks nightly.
    let setup = Setup::new_with_stub_version("0.0.10-nightly.20261018");
    setup.publish("latest/download", &TARGETS);
    setup.publish_nightly("v0.0.10-nightly.20261018", &TARGETS);
    setup.write_list(&[
        ("v0.0.9", false, false),
        ("v0.0.10-nightly.20261018", true, false),
    ]);
    let run = setup.install(&LINUX_X86_64, &[("FIBER_CHANNEL", "nightly")]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        setup.calls().unwrap(),
        format!(
            "release-install 0.0.10-nightly.20261018 --base-url {}\n",
            setup.base()
        )
    );

    // The flag beats the environment: stable over nightly, nightly over stable.
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    setup.publish_nightly("v0.0.10-nightly.20261018", &TARGETS);
    setup.write_list(&[
        ("v0.0.9", false, false),
        ("v0.0.10-nightly.20261018", true, false),
    ]);
    let run = setup.install_args(
        &LINUX_X86_64,
        &["--channel", "stable"],
        &[("FIBER_CHANNEL", "nightly")],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        setup.calls().unwrap(),
        format!("release-install 0.0.9 --base-url {}\n", setup.base())
    );

    let setup = Setup::new_with_stub_version("0.0.10-nightly.20261018");
    setup.publish("latest/download", &TARGETS);
    setup.publish_nightly("v0.0.10-nightly.20261018", &TARGETS);
    setup.write_list(&[
        ("v0.0.9", false, false),
        ("v0.0.10-nightly.20261018", true, false),
    ]);
    let run = setup.install_args(
        &LINUX_X86_64,
        &["--channel", "nightly"],
        &[("FIBER_CHANNEL", "stable")],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        setup.calls().unwrap(),
        format!(
            "release-install 0.0.10-nightly.20261018 --base-url {}\n",
            setup.base()
        )
    );

    // An empty environment means stable.
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    setup.publish_nightly("v0.0.10-nightly.20261018", &TARGETS);
    setup.write_list(&[
        ("v0.0.9", false, false),
        ("v0.0.10-nightly.20261018", true, false),
    ]);
    let run = setup.install(&LINUX_X86_64, &[("FIBER_CHANNEL", "")]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        setup.calls().unwrap(),
        format!("release-install 0.0.9 --base-url {}\n", setup.base())
    );

    // An explicit version beats the nightly channel.
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("download/v0.0.8", &TARGETS);
    setup.publish_nightly("v0.0.10-nightly.20261018", &TARGETS);
    setup.write_list(&[
        ("v0.0.9", false, false),
        ("v0.0.10-nightly.20261018", true, false),
    ]);
    let run = setup.install_args(
        &LINUX_X86_64,
        &["--channel", "nightly"],
        &[("FIBER_VERSION", "0.0.8")],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        setup.calls().unwrap(),
        format!("release-install 0.0.8 --base-url {}\n", setup.base())
    );
}

/// A nightly-form `FIBER_VERSION` stays refused: pinning a single nightly
/// is out of scope.
#[test]
fn a_nightly_fiber_version_is_refused() {
    let asked = "0.0.1-nightly.20261020";
    let setup = Setup::new_with_stub_version("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let run = setup.install(&LINUX_X86_64, &[("FIBER_VERSION", asked)]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        format!("fiber: FIBER_VERSION={asked} is not a version such as 0.3.0.\n")
    );
    assert!(!setup.dest().exists());
}
