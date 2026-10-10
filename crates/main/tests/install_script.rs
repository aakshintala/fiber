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
use support::Deadline;

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

/// A temporary root holding the fixture release, the install directory,
/// `HOME`, `TMPDIR` and the stub's data files, removed on drop; and the
/// test's deadline.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
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

struct Run {
    code: Option<i32>,
    stderr: String,
}

impl Setup {
    /// A root whose stub binary reports `version`.
    fn new(version: &str) -> Self {
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

    /// What the stub's `release-install` recorded, or `None` if it never
    /// ran.
    fn calls(&self) -> Option<String> {
        fs::read_to_string(self.path("stub/calls")).ok()
    }

    /// Runs `/bin/dash scripts/install.sh --base-url <release>` on `host`,
    /// with `FIBER_INSTALL_DIR` set to `dest` and then each of `env`, and
    /// waits for it under the test's [`Deadline`].
    fn install(&self, host: &Host, env: &[(&str, &str)]) -> Run {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh");
        let path = format!("{}:/usr/bin:/bin", fixture().join("bin").display());
        let mut command = Command::new("/bin/dash");
        command
            .arg(script)
            .args(["--base-url", &self.base()])
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
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, "install.sh to exit"),
        };
        watchdog.stand_down(self.deadline.cleanup());
        assert_eq!(
            fs::read_dir(self.path("tmp")).unwrap().count(),
            0,
            "install.sh left its temporary directory"
        );
        Run {
            code: output.status.code(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
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
    let setup = Setup::new("0.0.9");
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
        let setup = Setup::new("0.0.9");
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
        let setup = Setup::new("0.0.9");
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
    let setup = Setup::new("0.0.9");
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
    let setup = Setup::new("0.0.9");
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
        let setup = Setup::new("0.0.9");
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
    let setup = Setup::new("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let run = setup.install(&LINUX_X86_64, &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(run.stderr.contains(&warning(&setup)), "{}", run.stderr);

    let setup = Setup::new("0.0.9");
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
        let setup = Setup::new("0.0.9");
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
        let setup = Setup::new("0.0.9");
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
    let setup = Setup::new("0.0.9");
    setup.publish("latest/download", &TARGETS);
    let run = setup.install(&LINUX_X86_64, &[("FIBER_INSTALL_DIR", "")]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    setup.assert_installed(&setup.path("home/.local/bin"));
    assert!(!setup.dest().exists());
}

#[test]
fn a_failed_step_keeps_the_binary_and_says_to_run_again() {
    let setup = Setup::new("0.0.9");
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
