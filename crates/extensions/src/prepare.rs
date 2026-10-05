//! What runs after the person approves and before anything is put in place:
//! this platform's binary, and the manifest's install step
//! (`docs/extensions.md`, "Versions" and "Installing").

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use config::Manifest;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

use crate::Error;

/// The largest binary Fiber downloads.
// debt: a fixed 256 MiB cap; raise it when a real binary needs more.
const MAX_BINARY: u64 = 268_435_456;

/// This platform's key in a manifest's `binaries`, such as `darwin-arm64`.
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// Downloads this platform's binary into `dir/bin/` and checks its SHA-256,
/// then runs the install step in `dir`. Another platform's binary is never
/// fetched.
pub(crate) fn prepare(dir: &Path, manifest: &Manifest) -> Result<(), Error> {
    if let Some(binary) = manifest.binaries.get(&platform()) {
        let bytes = download(&manifest.name, &binary.url)?;
        let got = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
        if !got.eq_ignore_ascii_case(binary.sha256.trim()) {
            return Err(Error::BinaryChecksum {
                name: manifest.name.clone(),
                url: binary.url.clone(),
            });
        }
        let file = binary_name(&binary.url).ok_or_else(|| Error::Download {
            name: manifest.name.clone(),
            why: format!("`{}` names no file", binary.url),
        })?;
        let out = dir.join("bin");
        let path = out.join(file);
        let write = fs::create_dir_all(&out).and_then(|()| {
            fs::remove_file(&path).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o755)
                .open(&path)?;
            f.write_all(&bytes)
        });
        write.map_err(crate::install::io(&path))?;
    }
    if let Some(step) = &manifest.install
        && let Some((program, args)) = step.split_first()
    {
        let out = Command::new(program)
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::InstallStep {
                name: manifest.name.clone(),
                why: format!("`{program}`: {e}"),
            })?;
        if !out.status.success() {
            return Err(Error::InstallExited {
                name: manifest.name.clone(),
                why: format!(
                    "`{}` failed: {}",
                    step.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }
    }
    Ok(())
}

/// The file name a URL ends in, without its query.
fn binary_name(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next()?;
    let name = path.rsplit('/').next()?;
    (!name.is_empty() && name != "." && name != "..").then_some(name)
}

fn download(name: &str, url: &str) -> Result<Vec<u8>, Error> {
    let fail = |why: String| Error::Download {
        name: name.into(),
        why: format!("{url}: {why}"),
    };
    let config = Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    let agent: Agent = config.into();
    let mut response = agent.get(url).call().map_err(|e| fail(e.to_string()))?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_BINARY)
        .read_to_vec()
        .map_err(|e| fail(e.to_string()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{binary_name, download, hex, platform};

    #[test]
    fn a_url_names_its_file_without_the_query() {
        assert_eq!(binary_name("https://x/y/tool-1.0?sig=1"), Some("tool-1.0"));
        for bad in ["https://x/y/", "https://x/..", "https://x/y/.?a"] {
            assert_eq!(binary_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_digest_is_lowercase_hex() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }

    #[test]
    fn the_platform_key_is_os_and_arch() {
        let key = platform();
        assert!(key.contains('-'), "{key}");
        assert!(!key.contains("macos") && !key.contains("aarch64"), "{key}");
    }

    /// Present in a re-executed child, absent in the parent.
    const PROXY_CHILD: &str = "FIBER_TEST_PREPARE_PROXY_CHILD";

    /// The download URL, passed to the child on its environment.
    const PROXY_CHILD_URL: &str = "FIBER_TEST_PREPARE_URL";

    /// How long the parent waits for the re-executed child to exit.
    const CHILD_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

    /// How long the parent waits for the proxy to record a CONNECT.
    const CONNECT_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

    /// Downloads the scripted bytes in a child whose environment holds
    /// `extra`, on top of the proxy URL and the marker. The child re-runs
    /// this same test, which downloads and fails the child when the bytes do
    /// not arrive. `None` in the child, after its assertions.
    fn download_in_child(
        test: &str,
        extra: &[(&str, &str)],
    ) -> Option<(fakes::ProviderServer, fakes::ConnectProxy)> {
        if std::env::var_os(PROXY_CHILD).is_some() {
            let url = std::env::var(PROXY_CHILD_URL).unwrap();
            let bytes = download("probe", &url).unwrap();
            assert_eq!(bytes, b"{}");
            return None;
        }
        let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
        let proxy = fakes::ConnectProxy::start().unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(PROXY_CHILD, "1")
            .env("HTTPS_PROXY", proxy.url())
            .envs(extra.iter().copied())
            .env(PROXY_CHILD_URL, format!("{}/tool", server.url()))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let output = child.wait_with_output().unwrap();
            match done.send(output) {
                Ok(()) | Err(_) => {}
            }
        });
        let output = match finished.recv_timeout(CHILD_WITHIN) {
            Ok(output) => output,
            Err(_) => panic!("waited {CHILD_WITHIN:?} for the proxy-env child"),
        };
        assert!(
            output.status.success(),
            "the proxy-env child downloaded:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Some((server, proxy))
    }

    #[test]
    fn download_tunnels_through_the_proxy_environment() {
        let Some((server, proxy)) = download_in_child(
            "prepare::tests::download_tunnels_through_the_proxy_environment",
            &[],
        ) else {
            return;
        };
        let port = server.url().rsplit(':').next().unwrap().to_owned();
        let target = format!("127.0.0.1:{port}");
        assert!(
            proxy.await_connects(1, CONNECT_WITHIN),
            "the proxy recorded CONNECT {target}"
        );
        assert_eq!(proxy.connects(), [target]);
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn download_bypasses_the_proxy_for_no_proxy_hosts() {
        let Some((server, proxy)) = download_in_child(
            "prepare::tests::download_bypasses_the_proxy_for_no_proxy_hosts",
            &[("NO_PROXY", "127.0.0.1")],
        ) else {
            return;
        };
        assert!(
            proxy.connects().is_empty(),
            "nothing went through the proxy"
        );
        assert_eq!(server.requests().len(), 1);
    }
}
