use super::*;

const CHILD: &str = "FIBER_FAKES_RERUN_CHILD";
const PROBE: &str = "FIBER_FAKES_RERUN_PROBE";

#[test]
#[allow(
    clippy::print_stdout,
    reason = "the parent test reads this line from the child's output"
)]
fn rerun_returns_the_child_output_with_its_extra_env() {
    if std::env::var_os(CHILD).is_some() {
        assert_eq!(std::env::var(PROBE).unwrap(), "yes");
        assert_eq!(std::env::var("HTTPS_PROXY").unwrap(), "http://127.0.0.1:9");
        for var in SCRUBBED {
            if var == "HTTPS_PROXY" {
                continue;
            }
            assert!(std::env::var_os(var).is_none(), "{var} reaches the child");
        }
        println!("rerun child ran");
        return;
    }
    let output = rerun(
        "rerun::tests::rerun_returns_the_child_output_with_its_extra_env",
        &[
            (CHILD, "1"),
            (PROBE, "yes"),
            ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ],
    );
    assert!(
        output.status.success(),
        "the re-run child ran:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("rerun child ran"),
        "the child's output comes back"
    );
}

#[test]
fn rerun_scrubs_the_eight_proxy_variables() {
    for var in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        assert!(SCRUBBED.contains(&var), "{var} is scrubbed");
    }
    assert_eq!(SCRUBBED.len(), 8);
}
