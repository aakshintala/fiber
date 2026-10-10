//! This platform's binary name, its key and its hex digest, and downloads
//! through the proxy environment (`docs/extensions.md`, "Versions" and
//! "Installing").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

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
    let proxy_url = proxy.url();
    let download_url = format!("{}/tool", server.url());
    let mut env = vec![
        (PROXY_CHILD, "1"),
        ("HTTPS_PROXY", proxy_url.as_str()),
        (PROXY_CHILD_URL, download_url.as_str()),
    ];
    env.extend(extra.iter().copied());
    let output = fakes::rerun(test, &env);
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
